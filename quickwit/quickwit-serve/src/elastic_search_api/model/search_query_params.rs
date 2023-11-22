// Copyright (C) 2023 Quickwit, Inc.
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

use std::str::FromStr;
use std::time::Duration;

use quickwit_proto::search::SortOrder;
use quickwit_query::BooleanOperand;
use quickwit_search::SearchError;
use serde::{Deserialize, Serialize};

use super::super::TrackTotalHits;
use super::MultiSearchHeader;
use crate::elastic_search_api::model::{default_elasticsearch_sort_order, SortField};
use crate::simple_list::{from_simple_list, to_simple_list};

#[serde_with::skip_serializing_none]
#[derive(Default, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SearchQueryParams {
    #[serde(serialize_with = "to_simple_list")]
    #[serde(deserialize_with = "from_simple_list")]
    #[serde(default)]
    pub _source: Option<Vec<String>>,
    #[serde(serialize_with = "to_simple_list")]
    #[serde(deserialize_with = "from_simple_list")]
    #[serde(default)]
    pub _source_excludes: Option<Vec<String>>,
    #[serde(serialize_with = "to_simple_list")]
    #[serde(deserialize_with = "from_simple_list")]
    #[serde(default)]
    pub _source_includes: Option<Vec<String>>,
    #[serde(default)]
    pub allow_no_indices: Option<bool>,
    #[serde(default)]
    pub allow_partial_search_results: Option<bool>,
    #[serde(default)]
    pub analyze_wildcard: Option<bool>,
    #[serde(default)]
    pub analyzer: Option<String>,
    #[serde(default)]
    pub batched_reduce_size: Option<u64>,
    #[serde(default)]
    pub ccs_minimize_roundtrips: Option<bool>,
    #[serde(default)]
    pub default_operator: Option<BooleanOperand>,
    #[serde(default)]
    pub df: Option<String>,
    #[serde(serialize_with = "to_simple_list")]
    #[serde(deserialize_with = "from_simple_list")]
    #[serde(default)]
    pub docvalue_fields: Option<Vec<String>>,
    #[serde(default)]
    pub error_trace: Option<bool>,
    #[serde(serialize_with = "to_simple_list")]
    #[serde(deserialize_with = "from_simple_list")]
    #[serde(default)]
    pub expand_wildcards: Option<Vec<ExpandWildcards>>,
    #[serde(default)]
    pub explain: Option<bool>,
    #[serde(serialize_with = "to_simple_list")]
    #[serde(deserialize_with = "from_simple_list")]
    #[serde(default)]
    pub filter_path: Option<Vec<String>>,
    #[serde(default)]
    pub force_synthetic_source: Option<bool>,
    #[serde(default)]
    pub from: Option<u64>,
    #[serde(default)]
    pub human: Option<bool>,
    #[serde(default)]
    pub ignore_throttled: Option<bool>,
    #[serde(default)]
    pub ignore_unavailable: Option<bool>,
    #[serde(default)]
    pub lenient: Option<bool>,
    #[serde(default)]
    pub max_concurrent_shard_requests: Option<u64>,
    #[serde(default)]
    pub min_compatible_shard_node: Option<String>,
    #[serde(default)]
    pub pre_filter_shard_size: Option<u64>,
    #[serde(default)]
    pub preference: Option<String>,
    #[serde(default)]
    pub pretty: Option<bool>,
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub request_cache: Option<bool>,
    #[serde(default)]
    pub rest_total_hits_as_int: Option<bool>,
    #[serde(serialize_with = "to_simple_list")]
    #[serde(deserialize_with = "from_simple_list")]
    #[serde(default)]
    pub routing: Option<Vec<String>>,
    #[serde(default)]
    pub scroll: Option<String>,
    #[serde(default)]
    pub seq_no_primary_term: Option<bool>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(serialize_with = "to_simple_list")]
    #[serde(deserialize_with = "from_simple_list")]
    #[serde(default)]
    pub sort: Option<Vec<String>>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(serialize_with = "to_simple_list")]
    #[serde(deserialize_with = "from_simple_list")]
    #[serde(default)]
    pub stats: Option<Vec<String>>,
    #[serde(serialize_with = "to_simple_list")]
    #[serde(deserialize_with = "from_simple_list")]
    #[serde(default)]
    pub stored_fields: Option<Vec<String>>,
    #[serde(default)]
    pub suggest_field: Option<String>,
    #[serde(default)]
    pub suggest_mode: Option<SuggestMode>,
    #[serde(default)]
    pub suggest_size: Option<u64>,
    #[serde(default)]
    pub suggest_text: Option<String>,
    #[serde(default)]
    pub terminate_after: Option<u64>,
    #[serde(default)]
    pub timeout: Option<String>,
    #[serde(default)]
    pub track_scores: Option<bool>,
    #[serde(default)]
    pub track_total_hits: Option<TrackTotalHits>,
    #[serde(default)]
    pub typed_keys: Option<bool>,
    #[serde(default)]
    pub version: Option<bool>,
}

// Parse a single sort field parameter from ES sort query string parameter.
fn parse_sort_field_str(sort_field_str: &str) -> Result<SortField, SearchError> {
    if let Some((field, order_str)) = sort_field_str.split_once(':') {
        let order = SortOrder::from_str_name(order_str).ok_or_else(|| {
            SearchError::InvalidArgument(format!(
                "invalid sort order `{}`. expected `asc` or `desc`",
                field
            ))
        })?;
        Ok(SortField {
            field: field.to_string(),
            order,
        })
    } else {
        let order = default_elasticsearch_sort_order(sort_field_str);
        Ok(SortField {
            field: sort_field_str.to_string(),
            order,
        })
    }
}

impl SearchQueryParams {
    /// Accessor for the list of sort fields passed in the sort query string parameter.
    ///
    /// Returns an error if the sort query string are not in the expected format
    /// (`field:order,field2:order2,...`). Returns `Ok(None)` if the sort query string parameter
    /// is not present.
    #[allow(clippy::type_complexity)]
    pub(crate) fn sort_fields(&self) -> Result<Option<Vec<SortField>>, SearchError> {
        let Some(sort_fields_str) = self.sort.as_ref() else {
            return Ok(None);
        };
        let mut sort_fields: Vec<SortField> = Vec::with_capacity(sort_fields_str.len());
        for sort_field_str in sort_fields_str {
            sort_fields.push(parse_sort_field_str(sort_field_str)?);
        }
        Ok(Some(sort_fields))
    }

    /// Returns the scroll duration supplied by the user.
    ///
    /// This function returns an error if the scroll duration is not in the expected format. (`40s`
    /// etc.)
    pub fn parse_scroll_ttl(&self) -> Result<Option<Duration>, SearchError> {
        let Some(scroll_str) = self.scroll.as_ref() else {
            return Ok(None);
        };
        let duration: Duration = humantime::parse_duration(scroll_str).map_err(|_err| {
            SearchError::InvalidArgument(format!("invalid scroll duration: `{scroll_str}`"))
        })?;
        Ok(Some(duration))
    }
}

#[doc = "Whether to expand wildcard expression to concrete indices that are open, closed or both."]
#[derive(Debug, PartialEq, Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum ExpandWildcards {
    Open,
    Closed,
    Hidden,
    None,
    All,
}

impl FromStr for ExpandWildcards {
    type Err = &'static str;
    fn from_str(value_str: &str) -> Result<Self, Self::Err> {
        match value_str {
            "open" => Ok(Self::Open),
            "closed" => Ok(Self::Closed),
            "hidden" => Ok(Self::Hidden),
            "none" => Ok(Self::None),
            "all" => Ok(Self::All),
            _ => Err("unknown enum variant"),
        }
    }
}
impl ToString for ExpandWildcards {
    fn to_string(&self) -> String {
        match &self {
            Self::Open => "open".to_string(),
            Self::Closed => "closed".to_string(),
            Self::Hidden => "hidden".to_string(),
            Self::None => "none".to_string(),
            Self::All => "all".to_string(),
        }
    }
}

impl From<MultiSearchHeader> for SearchQueryParams {
    fn from(multi_search_header: MultiSearchHeader) -> Self {
        SearchQueryParams {
            allow_no_indices: multi_search_header.allow_no_indices,
            expand_wildcards: multi_search_header.expand_wildcards,
            ignore_unavailable: multi_search_header.ignore_unavailable,
            routing: multi_search_header.routing,
            request_cache: multi_search_header.request_cache,
            preference: multi_search_header.preference,
            ..Default::default()
        }
    }
}

/// Specify suggest mode
#[derive(Debug, PartialEq, Deserialize, Serialize, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum SuggestMode {
    Missing,
    Popular,
    Always,
}

impl FromStr for SuggestMode {
    type Err = &'static str;
    fn from_str(value_str: &str) -> Result<Self, Self::Err> {
        match value_str {
            "missing" => Ok(Self::Missing),
            "popular" => Ok(Self::Popular),
            "always" => Ok(Self::Always),
            _ => Err("unknown enum variant"),
        }
    }
}
impl ToString for SuggestMode {
    fn to_string(&self) -> String {
        match &self {
            Self::Missing => "missing".to_string(),
            Self::Popular => "popular".to_string(),
            Self::Always => "always".to_string(),
        }
    }
}
