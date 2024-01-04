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

use serde::Deserialize;
use serde_with::formats::PreferMany;
use serde_with::{serde_as, DefaultOnNull, OneOrMany};

use crate::elastic_query_dsl::{ConvertableToQueryAst, ElasticQueryDslInner};
use crate::not_nan_f32::NotNaNf32;
use crate::query_ast::{self, QueryAst};

/// # Unsupported features
/// - minimum_should_match
/// - named queries
#[serde_as]
#[derive(Deserialize, Debug, PartialEq, Eq, Clone)]
#[serde(deny_unknown_fields)]
pub struct BoolQuery {
    #[serde_as(deserialize_as = "DefaultOnNull<OneOrMany<_, PreferMany>>")]
    #[serde(default)]
    must: Vec<ElasticQueryDslInner>,
    #[serde_as(deserialize_as = "DefaultOnNull<OneOrMany<_, PreferMany>>")]
    #[serde(default)]
    must_not: Vec<ElasticQueryDslInner>,
    #[serde_as(deserialize_as = "DefaultOnNull<OneOrMany<_, PreferMany>>")]
    #[serde(default)]
    should: Vec<ElasticQueryDslInner>,
    #[serde_as(deserialize_as = "DefaultOnNull<OneOrMany<_, PreferMany>>")]
    #[serde(default)]
    filter: Vec<ElasticQueryDslInner>,
    #[serde(default)]
    pub boost: Option<NotNaNf32>,
}

impl BoolQuery {
    // Combines a list of children queries into a boolean union.
    pub(crate) fn union(children: Vec<ElasticQueryDslInner>) -> BoolQuery {
        BoolQuery {
            must: Vec::new(),
            must_not: Vec::new(),
            should: children,
            filter: Vec::new(),
            boost: None,
        }
    }
}

fn convert_vec(query_dsls: Vec<ElasticQueryDslInner>) -> anyhow::Result<Vec<QueryAst>> {
    query_dsls
        .into_iter()
        .map(|query_dsl| query_dsl.convert_to_query_ast())
        .collect()
}

impl ConvertableToQueryAst for BoolQuery {
    fn convert_to_query_ast(self) -> anyhow::Result<QueryAst> {
        let bool_query_ast = query_ast::BoolQuery {
            must: convert_vec(self.must)?,
            must_not: convert_vec(self.must_not)?,
            should: convert_vec(self.should)?,
            filter: convert_vec(self.filter)?,
        };
        Ok(bool_query_ast.into())
    }
}

impl From<BoolQuery> for ElasticQueryDslInner {
    fn from(bool_query: BoolQuery) -> Self {
        ElasticQueryDslInner::Bool(bool_query)
    }
}

#[cfg(test)]
mod tests {
    use crate::elastic_query_dsl::bool_query::BoolQuery;
    use crate::elastic_query_dsl::term_query::term_query_from_field_value;

    #[test]
    fn test_dsl_bool_query_deserialize_simple() {
        let bool_query_json = r#"{
            "must": [
                { "term": {"product_id": {"value": "1" }} },
                { "term": {"product_id": {"value": "2" }} }
            ]
        }"#;
        let bool_query: BoolQuery = serde_json::from_str(bool_query_json).unwrap();
        assert_eq!(
            &bool_query,
            &BoolQuery {
                must: vec![
                    term_query_from_field_value("product_id", "1").into(),
                    term_query_from_field_value("product_id", "2").into(),
                ],
                must_not: Vec::new(),
                should: Vec::new(),
                filter: Vec::new(),
                boost: None,
            }
        );
    }

    #[test]
    fn test_dsl_query_single() {
        let bool_query_json = r#"{
            "must": { "term": {"product_id": {"value": "1" }} },
            "filter": { "term": {"product_id": {"value": "2" }} }
        }"#;
        let bool_query: BoolQuery = serde_json::from_str(bool_query_json).unwrap();
        assert_eq!(
            &bool_query,
            &BoolQuery {
                must: vec![term_query_from_field_value("product_id", "1").into(),],
                must_not: Vec::new(),
                should: Vec::new(),
                filter: vec![term_query_from_field_value("product_id", "2").into(),],
                boost: None,
            }
        );
    }

    #[test]
    fn test_dsl_query_with_null_values() {
        let bool_query_json = r#"{
            "must": null,
            "must_not": null,
            "should": null,
            "filter": null,
            "boost": null
        }"#;
        let bool_query: BoolQuery = serde_json::from_str(bool_query_json).unwrap();
        assert_eq!(
            &bool_query,
            &BoolQuery {
                must: Vec::new(),
                must_not: Vec::new(),
                should: Vec::new(),
                filter: Vec::new(),
                boost: None,
            }
        );
    }
}
