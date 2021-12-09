// Copyright (C) 2021 Quickwit, Inc.
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

#![warn(missing_docs)]
#![allow(clippy::bool_assert_comparison)]

//! Index config defines how to configure an index and especially how
//! to convert a json like documents to a document indexable by tantivy
//! engine, aka tantivy::Document.

mod config;
mod default_index_config;
mod error;
mod query_builder;
mod sort_by;

pub use config::{build_tag_value, build_too_many_tag_value, match_tag_field_name, IndexConfig};
pub use default_index_config::{
    DefaultIndexConfig, DefaultIndexConfigBuilder, DocParsingError, FieldMappingEntry, SortByConfig,
};
pub use error::QueryParserError;
pub use sort_by::{SortBy, SortOrder};

/// Field name reserved for storing the source document.
pub const SOURCE_FIELD_NAME: &str = "_source";

/// Field name reserved for storing the tags.
pub const TAGS_FIELD_NAME: &str = "_tags";

/// Maximum distinct values allowed for a tag field within a split.
pub const MAX_VALUES_PER_TAG_FIELD: usize = if cfg!(any(test, feature = "testsuite")) {
    6
} else {
    1000
};

/// Returns a default `DefaultIndexConfig` for unit tests.
#[cfg(any(test, feature = "testsuite"))]
pub fn default_config_for_tests() -> DefaultIndexConfig {
    const JSON_CONFIG_VALUE: &str = r#"
        {
            "store_source": true,
            "default_search_fields": [
                "body", "attributes.server", "attributes.server.status"
            ],
            "timestamp_field": "timestamp",
            "sort_by": {
                "field_name": "timestamp",
                "order": "desc"
            },
            "tag_fields": ["owner"],
            "field_mappings": [
                {
                    "name": "timestamp",
                    "type": "i64",
                    "fast": true
                },
                {
                    "name": "body",
                    "type": "text",
                    "stored": true
                },
                {
                    "name": "response_date",
                    "type": "date",
                    "fast": true
                },
                {
                    "name": "response_time",
                    "type": "f64",
                    "fast": true
                },
                {
                    "name": "response_payload",
                    "type": "bytes",
                    "fast": true
                },
                {
                    "name": "owner",
                    "type": "text"
                },
                {
                    "name": "attributes",
                    "type": "object",
                    "field_mappings": [
                        {
                            "name": "tags",
                            "type": "array<i64>"
                        },
                        {
                            "name": "server",
                            "type": "text"
                        },
                        {
                            "name": "server.status",
                            "type": "array<text>"
                        },
                        {
                            "name": "server.payload",
                            "type": "array<bytes>"
                        }
                    ]
                }
            ]
        }"#;
    serde_json::from_str::<DefaultIndexConfig>(JSON_CONFIG_VALUE).unwrap()
}

/// Returns a default `DefaultIndexConfig` for unit tests.
#[cfg(any(test, feature = "testsuite"))]
pub fn default_config_with_demux_for_tests() -> DefaultIndexConfig {
    const JSON_CONFIG_VALUE: &str = r#"
        {
            "store_source": true,
            "default_search_fields": [
                "body", "tenant_id"
            ],
            "timestamp_field": "timestamp",
            "sort_by": {
                "field_name": "timestamp",
                "order": "desc"
            },
            "field_mappings": [
                {
                    "name": "timestamp",
                    "type": "i64",
                    "fast": true
                },
                {
                    "name": "body",
                    "type": "text",
                    "stored": true
                },
                {
                    "name": "tenant_id",
                    "type": "u64",
                    "fast": true
                }
            ]
        }"#;
    serde_json::from_str::<DefaultIndexConfig>(JSON_CONFIG_VALUE).unwrap()
}
