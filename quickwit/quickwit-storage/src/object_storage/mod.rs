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

mod error;

mod s3_compatible_storage;
pub use self::s3_compatible_storage::S3CompatibleObjectStorage;
pub use self::s3_compatible_storage_resolver::S3CompatibleObjectStorageFactory;

mod policy;
pub use crate::object_storage::policy::MultiPartPolicy;

mod s3_compatible_storage_resolver;

#[cfg(feature = "azure")]
mod azure_blob_storage;
#[cfg(feature = "azure")]
pub use self::azure_blob_storage::{AzureBlobStorage, AzureBlobStorageFactory};
