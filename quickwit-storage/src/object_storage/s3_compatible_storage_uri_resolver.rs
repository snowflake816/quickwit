// Copyright (C) 2022 Quickwit, Inc.
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

use std::sync::Arc;

pub use rusoto_core::Region;

use crate::{DebouncedStorage, S3CompatibleObjectStorage, StorageFactory};

/// S3 Object storage Uri Resolver
#[derive(Default)]
pub struct S3CompatibleObjectStorageFactory;

impl StorageFactory for S3CompatibleObjectStorageFactory {
    fn protocol(&self) -> String {
        "s3".to_string()
    }

    fn resolve(&self, uri: &str) -> crate::StorageResult<std::sync::Arc<dyn crate::Storage>> {
        let storage = DebouncedStorage::new(S3CompatibleObjectStorage::from_uri(uri)?);
        Ok(Arc::new(storage))
    }
}
