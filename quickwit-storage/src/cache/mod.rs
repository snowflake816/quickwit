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

mod in_ram_slice_cache;
mod memory_sized_cache;
mod storage_with_cache;

use std::ops::Range;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use bytes::Bytes;

pub use self::in_ram_slice_cache::SliceCache;
pub use self::memory_sized_cache::MemorySizedCache;
pub use self::storage_with_cache::StorageWithCacheFactory;

/// The `Cache` trait is the abstraction used to describe the caching logic
/// used in front of a storage. See `StorageWithCache`.
#[cfg_attr(any(test, feature = "testsuite"), mockall::automock)]
#[async_trait]
pub trait Cache: Send + Sync + 'static {
    /// Try to get a slice from the cache.
    async fn get(&self, path: &Path, byte_range: Range<usize>) -> Option<Bytes>;
    /// Try to get the entire file.
    async fn get_all(&self, path: &Path) -> Option<Bytes>;
    /// Put a slice of data into the cache.
    async fn put(&self, path: PathBuf, byte_range: Range<usize>, bytes: Bytes);
    /// Put an entire file into the cache.
    async fn put_all(&self, path: PathBuf, bytes: Bytes);
}
