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

#[cfg(test)]
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use quickwit_metastore::SplitMetadata;
use quickwit_storage::{PutPayload, Storage, StorageResult};
use tokio::sync::Mutex;
use tracing::info;

use super::LocalSplitStore;
use crate::split_store::INTERNAL_CACHE_DIR_NAME;
use crate::{MergePolicy, StableMultitenantWithTimestampMergePolicy};

/// `IndexingSplitStoreParams` encapsulates the various contraints of the cache.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IndexingSplitStoreParams {
    /// Maximum number of files allowed in the cache.
    pub max_num_splits: usize,
    /// Maximum size in bytes allowed in the cache.
    pub max_num_bytes: usize,
}

impl Default for IndexingSplitStoreParams {
    fn default() -> Self {
        Self {
            max_num_splits: 1000,
            max_num_bytes: 100_000_000_000, // 100GB
        }
    }
}

/// IndexingSplitStore is a wrapper around a regular `Storage` to upload and
/// download splits while allowing for efficient caching.
///
/// We typically index with a limited amount of RAM or some constraints on the
/// expected time-to-search.
/// Because of these constraints, the indexer produces splits that are smaller
/// than optimal and need to be merged.
///
/// A split therefore typically undergoes a few merges relatively shortly after
/// its creation.
///
/// In order to alleviate the disk IO as well as the network bandwidth,
/// we save new splits into a split store.
///
/// The role of the `IndexingSplitStore` is to act as a cache to avoid
/// unnecessary download of fresh splits. Its behavior are however very different
/// from a usual cache as we have a strong knowledge of the split lifecycle.
///
/// The splits are stored on the local filesystem.
#[derive(Clone)]
pub struct IndexingSplitStore {
    /// The remote storage.
    remote_storage: Arc<dyn Storage>,

    local_split_store: Option<Arc<Mutex<LocalSplitStore>>>,

    /// The merge policy is useful to identify whether a split
    /// should be stored in the local storage or not.
    /// (mature splits do not need to be stored).
    merge_policy: Arc<dyn MergePolicy>,
}

impl IndexingSplitStore {
    /// Create an instance of [`IndexingSplitStore`]
    ///
    /// It needs the remote storage to work with.
    pub fn create_with_local_store(
        remote_storage: Arc<dyn Storage>,
        scratch_dir: &Path,
        cache_params: IndexingSplitStoreParams,
        merge_policy: Arc<dyn MergePolicy>,
    ) -> StorageResult<Self> {
        let local_storage_root = scratch_dir.join(INTERNAL_CACHE_DIR_NAME);
        std::fs::create_dir_all(&local_storage_root)?;
        let local_split_store = LocalSplitStore::open(local_storage_root, cache_params)?;
        Ok(Self {
            remote_storage,
            local_split_store: Some(Arc::new(Mutex::new(local_split_store))),
            merge_policy,
        })
    }

    /// Create a storage with upload cache in a temp directory for tests.
    pub fn create_with_no_local_store(remote_storage: Arc<dyn Storage>) -> Self {
        IndexingSplitStore {
            remote_storage,
            local_split_store: None,
            merge_policy: Arc::new(StableMultitenantWithTimestampMergePolicy::default()),
        }
    }

    /// Stores a split.
    ///
    /// If a split is identified as mature by the merge policy,
    /// it will not be cached into the local storage.
    ///
    /// In order to limit the write IO, the file might be moved (and not copied into
    /// the store).
    /// In other words, after calling this function the file will not be available
    /// at `split_path` anymore.
    pub async fn store_split(
        &self,
        split: &SplitMetadata,
        split_path: &Path,
    ) -> anyhow::Result<()> {
        let start = Instant::now();

        let split_num_bytes = tokio::fs::metadata(split_path).await?.len() as usize;

        info!(split_path=%split_path.display(), split_id=%split.split_id, "store-split-remote");
        let key = PathBuf::from(quickwit_common::split_file(&split.split_id));
        let payload = PutPayload::from(split_path.to_path_buf());
        self.remote_storage
            .put(&key, payload)
            .await
            .with_context(|| {
                format!(
                    "Failed uploading key {} in bucket {}",
                    key.display(),
                    self.remote_storage.uri()
                )
            })?;
        let elapsed_secs = start.elapsed().as_secs_f32();
        let split_size_in_megabytes = split_num_bytes / 1_000_000;
        let throughput_mb_s = split_size_in_megabytes as f32 / elapsed_secs;
        info!(
            split_id = %split.split_id,
            elapsed_secs = %elapsed_secs,
            split_size_in_megabytes = %split_size_in_megabytes,
            throughput_mb_s = %throughput_mb_s,
            "store-split-remote-end"
        );

        let is_mature = self.merge_policy.is_mature(split);
        if !is_mature {
            if let Some(split_store) = self.local_split_store.as_ref() {
                let mut split_store_lock = split_store.lock().await;
                if split_store_lock
                    .move_into_cache(&split.split_id, split_path, split_num_bytes)
                    .await?
                {
                    return Ok(());
                }
            }
        }

        tokio::fs::remove_file(split_path).await?;

        Ok(())
    }

    /// Delete a split.
    pub async fn delete(&self, split_id: &str) -> StorageResult<()> {
        let split_filename = quickwit_common::split_file(split_id);
        let split_path = Path::new(&split_filename);
        self.remote_storage.delete(split_path).await?;
        if let Some(local_split_store) = self.local_split_store.as_ref() {
            let mut local_split_store_lock = local_split_store.lock().await;
            local_split_store_lock.remove_split(split_id).await?;
        }
        Ok(())
    }

    /// Gets a split from the split store, and makes it available to the given `output_path`.
    ///
    /// The output_path is expected to be a directory path.
    pub async fn fetch_split(&self, split_id: &str, output_dir_path: &Path) -> StorageResult<()> {
        let path = PathBuf::from(quickwit_common::split_file(split_id));
        if let Some(local_split_store) = self.local_split_store.as_ref() {
            let mut local_split_store_lock = local_split_store.lock().await;
            if local_split_store_lock
                .fetch_split(split_id, output_dir_path)
                .await?
            {
                return Ok(());
            }
        }
        let start_time = Instant::now();
        let dest_filepath = output_dir_path.join(&path);
        info!(split_id=%split_id, dest_filepath=?dest_filepath, "fetch-split-from-remote-storage");
        self.remote_storage
            .copy_to_file(&path, &dest_filepath)
            .await?;
        info!(split_id=%split_id, dest_filepath=?dest_filepath, elapsed=?start_time.elapsed(), "fetch-split-from_remote-storage-end");
        Ok(())
    }

    /// Takes a snapshot of the cache view (only used for testing).
    #[cfg(test)]
    async fn inspect_local_store(&self) -> HashMap<String, usize> {
        if let Some(split_store) = self.local_split_store.as_ref() {
            let split_store_lock = split_store.lock().await;
            split_store_lock.inspect().clone()
        } else {
            HashMap::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use quickwit_metastore::SplitMetadata;
    use quickwit_storage::{RamStorage, Storage, StorageError, StorageErrorKind};
    use tempfile::tempdir;
    use tokio::fs;

    use super::{IndexingSplitStore, IndexingSplitStoreParams};
    use crate::split_store::INTERNAL_CACHE_DIR_NAME;
    use crate::StableMultitenantWithTimestampMergePolicy;

    #[tokio::test]
    async fn test_create_should_error_with_wrong_num_files() -> anyhow::Result<()> {
        let local_dir = tempdir()?;
        let root_path = local_dir.path().join(INTERNAL_CACHE_DIR_NAME);
        fs::create_dir_all(root_path.to_path_buf()).await?;
        fs::write(root_path.join("a.split"), b"a").await?;
        fs::write(root_path.join("b.split"), b"b").await?;
        fs::write(root_path.join("c.split"), b"c").await?;

        let cache_params = IndexingSplitStoreParams {
            max_num_splits: 2,
            max_num_bytes: 10,
        };
        let remote_storage = Arc::new(RamStorage::default());
        let merge_policy = Arc::new(StableMultitenantWithTimestampMergePolicy::default());
        let result = IndexingSplitStore::create_with_local_store(
            remote_storage,
            local_dir.path(),
            cache_params,
            merge_policy,
        );
        assert!(matches!(
            result,
            Err(StorageError {
                kind: StorageErrorKind::InternalError,
                ..
            })
        ));
        Ok(())
    }

    #[tokio::test]
    async fn test_create_should_error_with_wrong_num_bytes() -> anyhow::Result<()> {
        let local_dir = tempdir()?;
        let root_path = local_dir.path().join(INTERNAL_CACHE_DIR_NAME);
        fs::create_dir_all(root_path.to_path_buf()).await?;
        fs::write(root_path.join("a.split"), b"abcdefgh").await?;
        fs::write(root_path.join("b.split"), b"abcdefgh").await?;

        let cache_params = IndexingSplitStoreParams {
            max_num_splits: 4,
            max_num_bytes: 10,
        };
        let remote_storage = Arc::new(RamStorage::default());
        let merge_policy = Arc::new(StableMultitenantWithTimestampMergePolicy::default());
        let result = IndexingSplitStore::create_with_local_store(
            remote_storage,
            local_dir.path(),
            cache_params,
            merge_policy,
        );
        assert!(matches!(
            result,
            Err(StorageError {
                kind: StorageErrorKind::InternalError,
                ..
            })
        ));
        Ok(())
    }
    #[tokio::test]
    async fn test_create_should_accept_a_file_size_exeeding_constraint() -> anyhow::Result<()> {
        let local_dir = tempdir()?;
        let root_path = local_dir.path().join(INTERNAL_CACHE_DIR_NAME);
        fs::create_dir_all(root_path.to_path_buf()).await?;
        fs::write(root_path.join("b.split"), b"abcd").await?;
        fs::write(root_path.join("a.split"), b"abcdefgh").await?;

        let cache_params = IndexingSplitStoreParams {
            max_num_splits: 100,
            max_num_bytes: 100,
        };
        let remote_storage = Arc::new(RamStorage::default());
        let merge_policy = Arc::new(StableMultitenantWithTimestampMergePolicy::default());
        let result = IndexingSplitStore::create_with_local_store(
            remote_storage,
            local_dir.path(),
            cache_params,
            merge_policy,
        );
        assert!(result.is_ok());
        Ok(())
    }

    fn create_test_split_metadata(split_id: &str) -> SplitMetadata {
        SplitMetadata {
            split_id: split_id.to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_local_store_cache_in_and_out() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let split_cache_dir = tempdir()?;
        let merge_policy = Arc::new(StableMultitenantWithTimestampMergePolicy::default());
        let remote_storage = Arc::new(RamStorage::default());
        let split_store = IndexingSplitStore::create_with_local_store(
            remote_storage,
            split_cache_dir.path(),
            IndexingSplitStoreParams::default(),
            merge_policy.clone(),
        )?;
        {
            let bundle_path = temp_dir.path().join("bundle");
            fs::write(&bundle_path, b"split1 content").await?;
            let split_metadata1 = create_test_split_metadata("split1");
            split_store
                .store_split(&split_metadata1, &bundle_path)
                .await?;
            assert!(!bundle_path.exists());
            assert!(split_cache_dir
                .path()
                .join("split-cache/split1.split")
                .exists());
            let local_store_stats = split_store.inspect_local_store().await;
            assert_eq!(local_store_stats.len(), 1);
            assert_eq!(local_store_stats.get("split1").cloned(), Some(14));
        }
        {
            let bundle_path = temp_dir.path().join("bundle");
            fs::write(&bundle_path, b"split2 larger content").await?;
            let split_metadata1 = create_test_split_metadata("split2");
            split_store
                .store_split(&split_metadata1, &bundle_path)
                .await?;
            assert!(!bundle_path.exists());
            assert!(split_cache_dir
                .path()
                .join("split-cache/split2.split")
                .exists());
            let local_store_stats = split_store.inspect_local_store().await;
            assert_eq!(local_store_stats.len(), 2);
            assert_eq!(local_store_stats.get("split1").cloned(), Some(14));
            assert_eq!(local_store_stats.get("split2").cloned(), Some(21));
        }

        let output_dir = tempfile::tempdir()?;
        {
            split_store.fetch_split("split1", output_dir.path()).await?;
            let content = tokio::fs::read(output_dir.path().join("split1.split")).await?;
            assert_eq!(&content[..], b"split1 content");
        }
        {
            split_store.fetch_split("split2", output_dir.path()).await?;
            let content = tokio::fs::read(output_dir.path().join("split2.split")).await?;
            assert_eq!(&content[..], b"split2 larger content");
        }
        let local_store_stats = split_store.inspect_local_store().await;
        assert!(local_store_stats.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn test_put_should_not_store_in_cache_when_max_num_files_reached() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let bundle_path = temp_dir.path().join("bundle");

        let split_cache_dir = tempdir()?;
        let merge_policy = Arc::new(StableMultitenantWithTimestampMergePolicy::default());
        let remote_storage = Arc::new(RamStorage::default());
        let split_store = IndexingSplitStore::create_with_local_store(
            remote_storage,
            split_cache_dir.path(),
            IndexingSplitStoreParams {
                max_num_splits: 1,
                max_num_bytes: 1_000_000,
            },
            merge_policy.clone(),
        )?;

        {
            fs::write(&bundle_path, b"split1 content").await?;
            let split_metadata1 = create_test_split_metadata("split1");
            split_store
                .store_split(&split_metadata1, &bundle_path)
                .await?;
            assert!(!bundle_path.exists());
            assert!(split_cache_dir
                .path()
                .join("split-cache/split1.split")
                .exists());
            let local_store_stats = split_store.inspect_local_store().await;
            assert_eq!(local_store_stats.len(), 1);
            assert_eq!(local_store_stats.get("split1").cloned(), Some(14));
        }
        {
            fs::write(&bundle_path, b"split2 content").await?;
            let split_metadata2 = create_test_split_metadata("split2");
            split_store
                .store_split(&split_metadata2, &bundle_path)
                .await?;
            assert!(!bundle_path.exists());
            assert!(!split_cache_dir
                .path()
                .join("split-cache/split2.split")
                .exists());
            let local_store_stats = split_store.inspect_local_store().await;
            assert_eq!(local_store_stats.len(), 1);
            assert_eq!(local_store_stats.get("split1").cloned(), Some(14));
        }
        {
            let output = tempfile::tempdir()?;
            split_store.fetch_split("split1", output.path()).await?;
            split_store.fetch_split("split2", output.path()).await?;
            assert!(output.path().join("split1.split").exists());
            assert!(output.path().join("split2.split").exists());
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_put_should_not_store_in_cache_when_max_num_bytes_reached() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let bundle_path = temp_dir.path().join("bundle");

        let split_cache_dir = tempdir()?;
        let merge_policy = Arc::new(StableMultitenantWithTimestampMergePolicy::default());
        let remote_storage = Arc::new(RamStorage::default());
        let split_store = IndexingSplitStore::create_with_local_store(
            remote_storage,
            split_cache_dir.path(),
            IndexingSplitStoreParams {
                max_num_splits: 10,
                max_num_bytes: 20,
            },
            merge_policy.clone(),
        )?;

        {
            fs::write(&bundle_path, b"split1 content").await?;
            let split_metadata1 = create_test_split_metadata("split1");
            split_store
                .store_split(&split_metadata1, &bundle_path)
                .await?;
            assert!(!bundle_path.exists());
            assert!(split_cache_dir
                .path()
                .join("split-cache/split1.split")
                .exists());
            let local_store_stats = split_store.inspect_local_store().await;
            assert_eq!(local_store_stats.len(), 1);
            assert_eq!(local_store_stats.get("split1").cloned(), Some(14));
        }

        {
            fs::write(&bundle_path, b"split2 content").await?;
            let split_metadata2 = create_test_split_metadata("split2");
            split_store
                .store_split(&split_metadata2, &bundle_path)
                .await?;
            assert!(!bundle_path.exists());
            assert!(!split_cache_dir
                .path()
                .join("split-cache/split2.split")
                .exists());
            let local_store_stats = split_store.inspect_local_store().await;
            assert_eq!(local_store_stats.len(), 1);
            assert_eq!(local_store_stats.get("split1").cloned(), Some(14));
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_delete_should_remove_from_both_storage() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let bundle_path = temp_dir.path().join("bundle");

        let split_cache_dir = tempdir()?;
        let merge_policy = Arc::new(StableMultitenantWithTimestampMergePolicy::default());
        let remote_storage = Arc::new(RamStorage::default());
        let split_store = IndexingSplitStore::create_with_local_store(
            remote_storage.clone(),
            split_cache_dir.path(),
            IndexingSplitStoreParams {
                max_num_splits: 10,
                max_num_bytes: 20,
            },
            merge_policy.clone(),
        )?;

        {
            fs::write(&bundle_path, b"split1 content").await?;
            let split_metadata1 = create_test_split_metadata("split1");
            split_store
                .store_split(&split_metadata1, &bundle_path)
                .await?;
            assert!(!bundle_path.exists());
            assert!(split_cache_dir
                .path()
                .join("split-cache/split1.split")
                .exists());
            let local_store_stats = split_store.inspect_local_store().await;
            assert_eq!(local_store_stats.len(), 1);
            assert_eq!(local_store_stats.get("split1").cloned(), Some(14));
        }

        let split1_bytes = remote_storage.get_all(Path::new("split1.split")).await?;
        assert_eq!(&split1_bytes, &b"split1 content"[..]);

        split_store.delete("split1").await?;

        let storage_err = remote_storage
            .get_all(Path::new("split1.split"))
            .await
            .unwrap_err();
        assert_eq!(storage_err.kind(), StorageErrorKind::DoesNotExist);

        Ok(())
    }
}
