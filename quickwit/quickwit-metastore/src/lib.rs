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

#![warn(missing_docs)]
#![allow(clippy::bool_assert_comparison)]
#![deny(clippy::disallowed_methods)]
#![allow(rustdoc::invalid_html_tags)]

//! `quickwit-metastore` is the abstraction used in Quickwit to interface itself to different
//! metastore:
//! - file-backed metastore
//! - PostgreSQL metastore
//! etc.

#[allow(missing_docs)]
pub mod checkpoint;
mod error;
mod metastore;
mod metastore_factory;
mod metastore_resolver;
mod split_metadata;
mod split_metadata_version;
#[cfg(test)]
pub(crate) mod tests;

use std::ops::Range;

pub use error::MetastoreResolverError;
pub use metastore::control_plane_metastore::ControlPlaneMetastore;
pub use metastore::file_backed_metastore::FileBackedMetastore;
pub(crate) use metastore::index_metadata::serialize::{IndexMetadataV0_6, VersionedIndexMetadata};
#[cfg(feature = "postgres")]
pub use metastore::postgresql_metastore::PostgresqlMetastore;
pub use metastore::{
    file_backed_metastore, AddSourceRequestExt, CreateIndexRequestExt, IndexMetadata,
    IndexMetadataResponseExt, ListIndexesMetadataRequestExt, ListIndexesMetadataResponseExt,
    ListIndexesQuery, ListSplitsQuery, ListSplitsRequestExt, ListSplitsResponseExt,
    MetastoreServiceExt, PublishSplitsRequestExt, StageSplitsRequestExt,
};
pub use metastore_factory::{MetastoreFactory, UnsupportedMetastore};
pub use metastore_resolver::MetastoreResolver;
use quickwit_common::is_disjoint;
use quickwit_doc_mapper::tag_pruning::TagFilterAst;
pub use split_metadata::{Split, SplitInfo, SplitMaturity, SplitMetadata, SplitState};
pub(crate) use split_metadata_version::{SplitMetadataV0_6, VersionedSplitMetadata};

#[derive(utoipa::OpenApi)]
#[openapi(components(schemas(
    Split,
    SplitState,
    VersionedIndexMetadata,
    IndexMetadataV0_6,
    VersionedSplitMetadata,
    SplitMetadataV0_6,
)))]
/// Schema used for the OpenAPI generation which are apart of this crate.
pub struct MetastoreApiSchemas;

/// Returns `true` if the split time range is included in `time_range_opt`.
/// If `time_range_opt` is None, returns always true.
pub fn split_time_range_filter(
    split_metadata: &SplitMetadata,
    time_range_opt: Option<&Range<i64>>,
) -> bool {
    match (time_range_opt, split_metadata.time_range.as_ref()) {
        (Some(filter_time_range), Some(split_time_range)) => {
            !is_disjoint(filter_time_range, split_time_range)
        }
        _ => true, // Return `true` if `time_range` is omitted or the split has no time range.
    }
}
/// Returns `true` if the tags filter evaluation is true.
/// If `tags_filter_opt` is None, returns always true.
pub fn split_tag_filter(
    split_metadata: &SplitMetadata,
    tags_filter_opt: Option<&TagFilterAst>,
) -> bool {
    tags_filter_opt
        .map(|tags_filter_ast| tags_filter_ast.evaluate(&split_metadata.tags))
        .unwrap_or(true)
}

#[cfg(test)]
mod backward_compatibility_tests;

#[cfg(any(test, feature = "testsuite"))]
/// Returns a metastore backed by an "in-memory file" for testing.
pub fn metastore_for_test() -> quickwit_proto::metastore::MetastoreServiceClient {
    quickwit_proto::metastore::MetastoreServiceClient::new(FileBackedMetastore::for_test(
        std::sync::Arc::new(quickwit_storage::RamStorage::default()),
    ))
}
