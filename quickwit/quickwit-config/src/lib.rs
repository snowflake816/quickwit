// Copyright (C) 2024 Quickwit, Inc.
//
// Quickwit is offered under the AGPL v3.0 and as commercial software.
// For commercial licensing, contact us at hello@quickwit.io.
//
// AGPL:
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program. If not, see <http://www.gnu.org/licenses/>.

#![deny(clippy::disallowed_methods)]

use std::str::FromStr;

use anyhow::{bail, Context};
use json_comments::StripComments;
use once_cell::sync::Lazy;
use quickwit_common::net::is_valid_hostname;
use quickwit_common::uri::Uri;
use regex::Regex;

mod config_value;
mod index_config;
pub mod merge_policy_config;
mod metastore_config;
mod node_config;
mod qw_env_vars;
pub mod service;
mod source_config;
mod storage_config;
mod templating;

// We export that one for backward compatibility.
// See #2048
use index_config::serialize::{IndexConfigV0_7, VersionedIndexConfig};
pub use index_config::{
    build_doc_mapper, load_index_config_from_user_config, DocMapping, IndexConfig,
    IndexingResources, IndexingSettings, RetentionPolicy, SearchSettings,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value as JsonValue;
pub use source_config::{
    load_source_config_from_user_config, FileSourceParams, GcpPubSubSourceParams,
    KafkaSourceParams, KinesisSourceParams, PulsarSourceAuth, PulsarSourceParams, RegionOrEndpoint,
    SourceConfig, SourceInputFormat, SourceParams, TransformConfig, VecSourceParams,
    VoidSourceParams, CLI_INGEST_SOURCE_ID, INGEST_API_SOURCE_ID, INGEST_SOURCE_ID,
};
use tracing::warn;

use crate::merge_policy_config::{
    ConstWriteAmplificationMergePolicyConfig, MergePolicyConfig, StableLogMergePolicyConfig,
};
pub use crate::metastore_config::{
    MetastoreBackend, MetastoreConfig, MetastoreConfigs, PostgresMetastoreConfig,
};
pub use crate::node_config::{
    IndexerConfig, IngestApiConfig, JaegerConfig, NodeConfig, SearcherConfig, SplitCacheLimits,
    DEFAULT_QW_CONFIG_PATH,
};
use crate::source_config::serialize::{SourceConfigV0_7, VersionedSourceConfig};
pub use crate::storage_config::{
    AzureStorageConfig, FileStorageConfig, RamStorageConfig, S3StorageConfig, StorageBackend,
    StorageBackendFlavor, StorageConfig, StorageConfigs,
};

#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    IndexingResources,
    IndexingSettings,
    SearchSettings,
    RetentionPolicy,
    MergePolicyConfig,
    DocMapping,
    VersionedSourceConfig,
    SourceConfigV0_7,
    VersionedIndexConfig,
    IndexConfigV0_7,
    SourceInputFormat,
    SourceParams,
    FileSourceParams,
    GcpPubSubSourceParams,
    KafkaSourceParams,
    KinesisSourceParams,
    PulsarSourceParams,
    PulsarSourceAuth,
    RegionOrEndpoint,
    ConstWriteAmplificationMergePolicyConfig,
    StableLogMergePolicyConfig,
    TransformConfig,
    VecSourceParams,
    VoidSourceParams,
)))]
/// Schema used for the OpenAPI generation which are apart of this crate.
pub struct ConfigApiSchemas;

/// Checks whether an identifier conforms to Quickwit object naming conventions.
pub fn validate_identifier(label: &str, value: &str) -> anyhow::Result<()> {
    static IDENTIFIER_REGEX: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"^[a-zA-Z][a-zA-Z0-9-_\.]{2,254}$").expect("regular expression should compile")
    });
    if IDENTIFIER_REGEX.is_match(value) {
        return Ok(());
    }
    bail!(
        "{label} identifier `{value}` is invalid. identifiers must match the following regular \
         expression: `^[a-zA-Z][a-zA-Z0-9-_\\.]{{2,254}}$`"
    );
}

/// Checks whether an index ID pattern conforms to Quickwit conventions.
/// Index ID patterns accept the same characters as identifiers AND accept `*`
/// chars to allow for glob-like patterns.
pub fn validate_index_id_pattern(pattern: &str) -> anyhow::Result<()> {
    static IDENTIFIER_REGEX_WITH_GLOB_PATTERN: Lazy<Regex> = Lazy::new(|| {
        Regex::new(r"^[a-zA-Z\*][a-zA-Z0-9-_\.\*]{0,254}$")
            .expect("regular expression should compile")
    });
    if !IDENTIFIER_REGEX_WITH_GLOB_PATTERN.is_match(pattern) {
        bail!(
            "index ID pattern `{pattern}` is invalid: patterns must match the following regular \
             expression: `^[a-zA-Z\\*][a-zA-Z0-9-_\\.\\*]{{0,254}}$`"
        );
    }
    // Forbid multiple stars in the pattern to force the user making simpler patterns
    // as multiple stars does not bring any value.
    if pattern.contains("**") {
        bail!(
            "index ID pattern `{pattern}` is invalid: patterns must not contain multiple \
             consecutive `*`"
        );
    }
    // If there is no star in the pattern, we need at least 3 characters.
    if !pattern.contains('*') && pattern.len() < 3 {
        bail!(
            "index ID pattern `{pattern}` is invalid: an index ID must have at least 3 characters"
        );
    }
    Ok(())
}

pub fn validate_node_id(node_id: &str) -> anyhow::Result<()> {
    if !is_valid_hostname(node_id) {
        bail!(
            "node identifier `{node_id}` is invalid. node identifiers must be valid short \
             hostnames (see RFC 1123)"
        );
    }
    Ok(())
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ConfigFormat {
    Json,
    Toml,
    Yaml,
}

impl ConfigFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            ConfigFormat::Json => "json",
            ConfigFormat::Toml => "toml",
            ConfigFormat::Yaml => "yaml",
        }
    }

    pub fn sniff_from_uri(uri: &Uri) -> anyhow::Result<ConfigFormat> {
        let extension_str: &str = uri.extension().with_context(|| {
            format!(
                "failed to read config file `{uri}`: file extension is missing. supported file \
                 formats and extensions are JSON (.json), TOML (.toml), and YAML (.yaml or .yml)"
            )
        })?;
        ConfigFormat::from_str(extension_str)
            .with_context(|| format!("failed to identify configuration file format {uri}"))
    }

    pub fn parse<T>(&self, payload: &[u8]) -> anyhow::Result<T>
    where T: DeserializeOwned {
        match self {
            ConfigFormat::Json => {
                let mut json_value: JsonValue =
                    serde_json::from_reader(StripComments::new(payload))?;
                let version_value = json_value.get_mut("version").context("missing version")?;
                if let Some(version_number) = version_value.as_u64() {
                    warn!(version_value=?version_value, "`version` is supposed to be a string");
                    *version_value = JsonValue::String(version_number.to_string());
                }
                serde_json::from_value(json_value).context("failed to read JSON file")
            }
            ConfigFormat::Toml => {
                let payload_str = std::str::from_utf8(payload)
                    .context("configuration file contains invalid UTF-8 characters")?;
                let mut toml_value: toml::Value =
                    toml::from_str(payload_str).context("failed to read TOML file")?;
                let version_value = toml_value.get_mut("version").context("missing version")?;
                if let Some(version_number) = version_value.as_integer() {
                    warn!(version_value=?version_value, "`version` is supposed to be a string");
                    *version_value = toml::Value::String(version_number.to_string());
                    let reserialized = toml::to_string(version_value)
                        .context("failed to reserialize toml config")?;
                    toml::from_str(&reserialized).context("failed to read TOML file")
                } else {
                    toml::from_str(payload_str).context("failed to read TOML file")
                }
            }
            ConfigFormat::Yaml => {
                serde_yaml::from_slice(payload).context("failed to read YAML file")
            }
        }
    }
}

impl FromStr for ConfigFormat {
    type Err = anyhow::Error;

    fn from_str(ext: &str) -> anyhow::Result<Self> {
        match ext {
            "json" => Ok(Self::Json),
            "toml" => Ok(Self::Toml),
            "yaml" | "yml" => Ok(Self::Yaml),
            _ => bail!(
                "file extension `.{ext}` is not supported. supported file formats and extensions \
                 are JSON (.json), TOML (.toml), and YAML (.yaml or .yml)",
            ),
        }
    }
}

pub trait TestableForRegression: Serialize + DeserializeOwned {
    fn sample_for_regression() -> Self;
    fn test_equality(&self, other: &Self);
}

#[cfg(test)]
mod tests {
    use super::validate_identifier;
    use crate::validate_index_id_pattern;

    #[test]
    fn test_validate_identifier() {
        validate_identifier("Cluster ID", "").unwrap_err();
        validate_identifier("Cluster ID", "-").unwrap_err();
        validate_identifier("Cluster ID", "_").unwrap_err();
        validate_identifier("Cluster ID", "f").unwrap_err();
        validate_identifier("Cluster ID", "fo").unwrap_err();
        validate_identifier("Cluster ID", "_fo").unwrap_err();
        validate_identifier("Cluster ID", "_foo").unwrap_err();
        validate_identifier("Cluster ID", ".foo.bar").unwrap_err();
        validate_identifier("Cluster ID", "foo").unwrap();
        validate_identifier("Cluster ID", "f-_").unwrap();
        validate_identifier("Index ID", "foo.bar").unwrap();

        assert!(validate_identifier("Cluster ID", "foo!")
            .unwrap_err()
            .to_string()
            .contains("Cluster ID identifier `foo!` is invalid."));
    }

    #[test]
    fn test_validate_index_id_pattern() {
        validate_index_id_pattern("*").unwrap();
        validate_index_id_pattern("abc.*").unwrap();
        validate_index_id_pattern("ab").unwrap_err();
        validate_index_id_pattern("").unwrap_err();
        validate_index_id_pattern("**").unwrap_err();
        assert!(validate_index_id_pattern("foo!")
            .unwrap_err()
            .to_string()
            .contains("index ID pattern `foo!` is invalid:"));
    }
}
