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

use quickwit_ingest::CommitType;
use quickwit_proto::ingest::CommitTypeV2;
use serde::Deserialize;

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq)]
pub struct ElasticBulkOptions {
    #[serde(default)]
    pub refresh: ElasticRefresh,
}

/// ?refresh parameter for elasticsearch bulk request
///
/// The syntax for this parameter is a bit confusing for backward compatibility reasons.
/// - Absence of ?refresh parameter or ?refresh=false means no refresh
/// - Presence of ?refresh parameter without any values or ?refresh=true means force refresh
/// - ?refresh=wait_for means wait for refresh
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, utoipa::ToSchema)]
#[serde(rename_all(deserialize = "snake_case"))]
#[derive(Default)]
pub enum ElasticRefresh {
    // if the refresh parameter is not present it is false
    #[default]
    /// The request doesn't wait for commit
    False,
    // but if it is present without a value like this: ?refresh, it should be the same as
    // ?refresh=true
    #[serde(alias = "")]
    /// The request forces an immediate commit after the last document in the batch and waits for
    /// it to finish.
    True,
    /// The request will wait for the next scheduled commit to finish.
    WaitFor,
}

impl From<ElasticRefresh> for CommitType {
    fn from(val: ElasticRefresh) -> Self {
        match val {
            ElasticRefresh::False => Self::Auto,
            ElasticRefresh::True => Self::Force,
            ElasticRefresh::WaitFor => Self::WaitFor,
        }
    }
}

impl From<ElasticRefresh> for CommitTypeV2 {
    fn from(val: ElasticRefresh) -> Self {
        match val {
            ElasticRefresh::False => Self::Auto,
            ElasticRefresh::True => Self::Force,
            ElasticRefresh::WaitFor => Self::Wait,
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::elastic_search_api::model::bulk_query_params::ElasticRefresh;
    use crate::elastic_search_api::model::ElasticBulkOptions;

    #[test]
    fn test_elastic_refresh_parsing() {
        assert_eq!(
            serde_qs::from_str::<ElasticBulkOptions>("")
                .unwrap()
                .refresh,
            ElasticRefresh::False
        );
        assert_eq!(
            serde_qs::from_str::<ElasticBulkOptions>("refresh=true")
                .unwrap()
                .refresh,
            ElasticRefresh::True
        );
        assert_eq!(
            serde_qs::from_str::<ElasticBulkOptions>("refresh=false")
                .unwrap()
                .refresh,
            ElasticRefresh::False
        );
        assert_eq!(
            serde_qs::from_str::<ElasticBulkOptions>("refresh=wait_for")
                .unwrap()
                .refresh,
            ElasticRefresh::WaitFor
        );
        assert_eq!(
            serde_qs::from_str::<ElasticBulkOptions>("refresh")
                .unwrap()
                .refresh,
            ElasticRefresh::True
        );
        assert_eq!(
            serde_qs::from_str::<ElasticBulkOptions>("refresh=wait")
                .unwrap_err()
                .to_string(),
            "unknown variant `wait`, expected one of `false`, `true`, `wait_for`"
        );
    }
}
