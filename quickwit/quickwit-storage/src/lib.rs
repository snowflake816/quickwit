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
#![allow(clippy::len_without_is_empty)]
#![deny(clippy::disallowed_methods)]

//! `quickwit-storage` is the abstraction used in quickwit to interface itself
//! to different storage:
//! - object storages (S3)
//! - local filesystem
//! - distributed filesystems.
//! etc.
//!
//! - The `BundleStorage` bundles together multiple files into a single file.
mod cache;
mod debouncer;
mod metrics;
mod storage;
pub use debouncer::AsyncDebouncer;
pub(crate) use debouncer::DebouncedStorage;

pub use self::metrics::STORAGE_METRICS;
pub use self::payload::PutPayload;
pub use self::storage::Storage;

mod bundle_storage;
mod error;
mod local_file_storage;
mod object_storage;
mod payload;
mod prefix_storage;
mod ram_storage;
mod split;
mod storage_resolver;

use quickwit_common::uri::Uri;
pub use tantivy::directory::OwnedBytes;

pub use self::bundle_storage::{BundleStorage, BundleStorageFileOffsets};
#[cfg(any(test, feature = "testsuite"))]
pub use self::cache::MockCache;
pub use self::cache::{
    wrap_storage_with_long_term_cache, ByteRangeCache, Cache, MemorySizedCache, QuickwitCache,
};
pub use self::local_file_storage::{LocalFileStorage, LocalFileStorageFactory};
#[cfg(feature = "azure")]
pub use self::object_storage::{AzureBlobStorage, AzureBlobStorageFactory};
pub use self::object_storage::{
    MultiPartPolicy, S3CompatibleObjectStorage, S3CompatibleObjectStorageFactory,
};
pub use self::ram_storage::{RamStorage, RamStorageBuilder};
pub use self::split::{SplitPayload, SplitPayloadBuilder};
#[cfg(any(test, feature = "testsuite"))]
pub use self::storage::MockStorage;
#[cfg(any(test, feature = "testsuite"))]
pub use self::storage_resolver::MockStorageFactory;
pub use self::storage_resolver::{
    quickwit_storage_uri_resolver, StorageFactory, StorageUriResolver,
};
#[cfg(feature = "testsuite")]
pub use self::test_suite::{
    storage_test_multi_part_upload, storage_test_single_part_upload, storage_test_suite,
};
pub use crate::error::{StorageError, StorageErrorKind, StorageResolverError, StorageResult};

/// Loads an entire local or remote file into memory.
pub async fn load_file(uri: &Uri) -> anyhow::Result<OwnedBytes> {
    let parent = uri
        .parent()
        .ok_or_else(|| anyhow::anyhow!("URI `{uri}` is not a valid file URI."))?;
    let storage = quickwit_storage_uri_resolver().resolve(&parent)?;
    let file_name = uri
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("URI `{uri}` is not a valid file URI."))?;
    let bytes = storage.get_all(file_name).await?;
    Ok(bytes)
}

#[cfg(any(test, feature = "testsuite"))]
mod for_test {
    use std::sync::Arc;

    use crate::{RamStorage, Storage};

    /// Returns a storage backed by an "in-memory file" for testing.
    pub fn storage_for_test() -> Arc<dyn Storage> {
        Arc::new(RamStorage::default())
    }
}

#[cfg(any(test, feature = "testsuite"))]
pub use for_test::storage_for_test;

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    #[tokio::test]
    async fn test_load_file() {
        let expected_bytes = tokio::fs::read_to_string("Cargo.toml").await.unwrap();
        assert_eq!(
            load_file(&Uri::from_str("Cargo.toml").unwrap())
                .await
                .unwrap()
                .as_slice(),
            expected_bytes.as_bytes()
        );
    }
}

#[cfg(any(test, feature = "testsuite"))]
pub(crate) mod test_suite {

    use std::path::Path;

    use anyhow::Context;

    use crate::{Storage, StorageErrorKind};

    async fn test_get_inexistent_file(storage: &mut dyn Storage) -> anyhow::Result<()> {
        let err = storage
            .get_slice(Path::new("missingfile"), 0..3)
            .await
            .map_err(|err| err.kind());
        assert!(matches!(err, Err(StorageErrorKind::DoesNotExist)));
        Ok(())
    }

    async fn test_write_and_get_slice(storage: &mut dyn Storage) -> anyhow::Result<()> {
        let test_path = Path::new("write_and_read_slice");
        storage
            .put(
                test_path,
                Box::new(b"abcdefghiklmnopqrstuvxyz"[..].to_vec()),
            )
            .await?;
        let payload = storage.get_slice(test_path, 3..6).await?;
        assert_eq!(&payload[..], b"def");
        Ok(())
    }

    async fn test_write_get_all(storage: &mut dyn Storage) -> anyhow::Result<()> {
        let test_path = Path::new("write_and_read_all");
        storage
            .put(test_path, Box::new(b"abcdef"[..].to_vec()))
            .await?;
        let payload = storage.get_all(test_path).await?;
        assert_eq!(&payload[..], &b"abcdef"[..]);
        Ok(())
    }

    async fn test_write_and_cp(storage: &mut dyn Storage) -> anyhow::Result<()> {
        let test_path = Path::new("write_and_cp");
        let payload_bytes = b"abcdefghijklmnopqrstuvwxyz".as_ref();
        storage
            .put(test_path, Box::new(payload_bytes.to_vec()))
            .await?;
        let tempdir = tempfile::tempdir()?;
        let dest_path = tempdir.path().to_path_buf();
        let local_copy = dest_path.join("local_copy");
        storage.copy_to_file(test_path, &local_copy).await?;
        let payload = std::fs::read(&local_copy)?;
        assert_eq!(&payload[..], payload_bytes);
        Ok(())
    }

    async fn test_write_and_delete(storage: &mut dyn Storage) -> anyhow::Result<()> {
        let test_path = Path::new("write_and_delete");
        let payload_bytes = b"abcdefghijklmnopqrstuvwxyz".as_ref();
        storage
            .put(test_path, Box::new(payload_bytes.to_vec()))
            .await?;
        storage.delete(test_path).await?;
        assert!(!storage.exists(test_path).await?);
        Ok(())
    }

    async fn test_write_and_bulk_delete(storage: &mut dyn Storage) -> anyhow::Result<()> {
        let test_paths = [
            Path::new("foo"),
            Path::new("bar"),
            Path::new("file-does-not-exist"),
        ];
        for test_path in &test_paths[0..2] {
            storage
                .put(Path::new(test_path), Box::new(b"123".to_vec()))
                .await?;
            assert!(storage.exists(test_path).await?);
        }
        storage.bulk_delete(&test_paths).await?;

        for test_path in test_paths {
            assert!(!storage.exists(test_path).await?);
        }
        Ok(())
    }

    async fn test_file_size(storage: &mut dyn Storage) -> anyhow::Result<()> {
        let test_path = Path::new("write_for_filesize");
        let payload_bytes = b"abcdefghijklmnopqrstuvwxyz".as_ref();
        storage
            .put(test_path, Box::new(payload_bytes.to_vec()))
            .await?;
        assert_eq!(storage.file_num_bytes(test_path).await?, 26u64);
        storage.delete(test_path).await?;
        Ok(())
    }

    async fn test_exists(storage: &mut dyn Storage) -> anyhow::Result<()> {
        let test_path = Path::new("exists");
        assert!(!storage.exists(test_path).await.unwrap());
        storage
            .put(test_path, Box::<std::vec::Vec<u8>>::default())
            .await?;
        assert!(storage.exists(test_path).await.unwrap());
        storage.delete(test_path).await.unwrap();
        Ok(())
    }

    async fn test_delete_missing_file(storage: &mut dyn Storage) -> anyhow::Result<()> {
        let test_path = Path::new("missing_file");
        assert!(!storage.exists(test_path).await.unwrap());
        assert!(storage.delete(test_path).await.is_ok());
        Ok(())
    }

    async fn test_write_and_delete_with_dir_separator(
        storage: &mut dyn Storage,
    ) -> anyhow::Result<()> {
        let test_path = Path::new("foo/bar/write_and_delete_with_separator");
        let payload_bytes = b"abcdefghijklmnopqrstuvwxyz".as_ref();
        storage
            .put(test_path, Box::new(payload_bytes.to_vec()))
            .await?;
        assert!(matches!(
            storage.exists(Path::new("foo/bar")).await,
            Ok(false)
        ));
        storage.delete(test_path).await?;

        assert!(matches!(
            storage.exists(Path::new("foo/bar")).await,
            Ok(false)
        ));
        assert!(matches!(storage.exists(Path::new("foo")).await, Ok(false)));
        Ok(())
    }

    /// Generic test suite for a storage.
    pub async fn storage_test_suite(storage: &mut dyn Storage) -> anyhow::Result<()> {
        test_get_inexistent_file(storage)
            .await
            .context("get_inexistent_file")?;
        test_write_and_get_slice(storage)
            .await
            .context("write_and_get_slice")?;
        test_write_get_all(storage)
            .await
            .context("write_and_get_all")?;
        test_write_and_cp(storage).await.context("write_and_cp")?;
        test_write_and_delete(storage)
            .await
            .context("write_and_delete")?;
        test_write_and_bulk_delete(storage)
            .await
            .context("write_and_bulk_delete")?;
        test_exists(storage).await.context("exists")?;
        test_write_and_delete_with_dir_separator(storage)
            .await
            .context("write_and_delete_with_separator")?;
        test_file_size(storage).await.context("file_size")?;
        test_delete_missing_file(storage)
            .await
            .context("delete_missing_file")?;
        Ok(())
    }

    /// Generic single-part upload test.
    pub async fn storage_test_single_part_upload(storage: &mut dyn Storage) -> anyhow::Result<()> {
        let test_path = Path::new("hello_small.txt");
        let data = b"hello, happy tax payer!".to_vec();
        let data_size = data.len() as u64;
        storage.put(test_path, Box::new(data)).await?;
        assert_eq!(storage.file_num_bytes(test_path).await?, data_size);
        Ok(())
    }

    /// Generic multi-part upload test.
    pub async fn storage_test_multi_part_upload(storage: &mut dyn Storage) -> anyhow::Result<()> {
        let test_path = Path::new("hello_large.txt");
        let test_buffer = vec![0u8; 15_000_000];
        storage.put(test_path, Box::new(test_buffer)).await?;
        assert_eq!(storage.file_num_bytes(test_path).await?, 15_000_000);
        Ok(())
    }
}
