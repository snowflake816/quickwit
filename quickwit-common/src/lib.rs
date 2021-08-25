/*
 * Copyright (C) 2021 Quickwit Inc.
 *
 * Quickwit is offered under the AGPL v3.0 and as commercial software.
 * For commercial licensing, contact us at hello@quickwit.io.
 *
 * AGPL:
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as
 * published by the Free Software Foundation, either version 3 of the
 * License, or (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU Affero General Public License for more details.
 *
 * You should have received a copy of the GNU Affero General Public License
 * along with this program.  If not, see <http://www.gnu.org/licenses/>.
 */

mod coolid;

use once_cell::sync::Lazy;
use regex::Regex;
use std::net::{SocketAddr, ToSocketAddrs};

pub use coolid::new_coolid;

/// Filenames used for hotcache files.
pub const HOTCACHE_FILENAME: &str = "hotcache";

/// This function takes such a index_url and breaks it into
/// s3://my_bucket/some_path_containing_my_indices / my_index
///                                                  \------/
///                                                  index_id
pub fn extract_index_id_from_index_uri(mut index_uri: &str) -> anyhow::Result<&str> {
    static INDEX_URI_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^.+://.+/.+$").unwrap());
    static INDEX_ID_PATTERN: Lazy<Regex> =
        Lazy::new(|| Regex::new(r"^[a-zA-Z][a-zA-Z0-9_\-]*$").unwrap());

    if !INDEX_URI_PATTERN.is_match(index_uri) {
        anyhow::bail!(
            "Invalid index uri `{}`. Expected format is: `protocol://bucket/path-to-target`.",
            index_uri
        );
    }

    if index_uri.ends_with('/') {
        index_uri = &index_uri[..index_uri.len() - 1];
    }
    let parts: Vec<&str> = index_uri.rsplitn(2, '/').collect();
    if parts.len() != 2 {
        anyhow::bail!("Failed to parse the uri into a metastore_uri and an index_id.");
    }
    if !INDEX_ID_PATTERN.is_match(parts[0]) {
        anyhow::bail!("Invalid index_id `{}`. Only alpha-numeric, `-` and `_` characters allowed. Cannot start with `-`, `_` or digit.", parts[0]);
    }

    Ok(parts[0])
}

/// Resolve DNS entry and convert them to SocketAddr.
pub fn to_socket_addr(addr_str: &str) -> anyhow::Result<SocketAddr> {
    if let Some(addr) = addr_str.to_socket_addrs()?.next() {
        Ok(addr)
    } else {
        Err(anyhow::anyhow!(
            "Unable to resolve the socket address. {}",
            addr_str
        ))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum QuickwitEnv {
    UNSET,
    LOCAL,
}

impl Default for QuickwitEnv {
    fn default() -> Self {
        Self::UNSET
    }
}

pub fn get_quickwit_env() -> QuickwitEnv {
    match std::env::var("QUICKWIT_ENV") {
        Ok(val) if val == "LOCAL" => QuickwitEnv::LOCAL,
        Ok(val) => {
            panic!("unknown value set for QUICKWIT_ENV {}", val)
        }
        Err(_) => QuickwitEnv::UNSET,
    }
}

pub fn setup_logging_for_tests() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        env_logger::builder().format_timestamp(None).init();
    });
}
