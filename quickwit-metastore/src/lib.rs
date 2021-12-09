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

//! `quickwit-metastore` is the abstraction used in quickwit to interface itself to different
//! metastore:
//! - single file metastore
//! etc.

#[macro_use]
mod tests;
mod split_metadata;
mod split_metadata_version;

#[cfg(feature = "postgres")]
extern crate openssl;

#[cfg(feature = "postgres")]
#[macro_use]
extern crate diesel_migrations;

#[cfg(feature = "postgres")]
#[macro_use]
extern crate diesel;

#[allow(missing_docs)]
pub mod checkpoint;
mod error;
mod metastore;
mod metastore_resolver;

#[cfg(feature = "postgres")]
#[allow(missing_docs)]
pub mod postgresql;

pub use error::{MetastoreError, MetastoreResolverError, MetastoreResult};
#[cfg(feature = "postgres")]
pub use metastore::postgresql_metastore::PostgresqlMetastore;
pub use metastore::single_file_metastore::SingleFileMetastore;
#[cfg(feature = "testsuite")]
pub use metastore::MockMetastore;
pub use metastore::{IndexMetadata, Metastore};
pub use metastore_resolver::{MetastoreFactory, MetastoreUriResolver};
pub use split_metadata::{Split, SplitMetadata, SplitState};
pub(crate) use split_metadata_version::VersionedSplitMetadataDeserializeHelper;
