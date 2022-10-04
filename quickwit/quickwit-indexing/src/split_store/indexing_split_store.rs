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

#[cfg(any(test, feature = "testsuite"))]
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};
use std::time::Instant;

use anyhow::Context;
use quickwit_config::IndexerConfig;
use quickwit_metastore::SplitMetadata;
use quickwit_storage::{PutPayload, Storage, StorageErrorKind, StorageResult};
use tantivy::Directory;
use tokio::sync::Mutex;
use tracing::{info, info_span, instrument, warn, Instrument};

use super::LocalSplitStore;
use crate::merge_policy::default_merge_policy;
use crate::split_store::SPLIT_CACHE_DIR_NAME;
use crate::{get_tantivy_directory_from_split_bundle, MergePolicy, SplitFolder};

/// A struct for keeping in check multiple SplitStore.
#[derive(Debug)]
pub struct SplitStoreSpaceQuota {
    /// Current number of splits in the cache.
    num_splits_in_cache: usize,
    /// Current size in bytes of splits in the cache.
    size_in_bytes_in_cache: usize,
    /// Maximum number of files allowed in the cache.
    max_num_splits: usize,
    /// Maximum size in bytes allowed in the cache.
    max_num_bytes: usize,
    /// Known split store local storage roots.
    // A SplitStore can be opened several time for the lifetime
    // of an indexing server. This set helps in avoiding adding
    // the same SplitStore opening size again.
    opened_split_store_roots: HashSet<PathBuf>,
}

impl Default for SplitStoreSpaceQuota {
    fn default() -> Self {
        Self {
            num_splits_in_cache: 0,
            size_in_bytes_in_cache: 0,
            max_num_bytes: IndexerConfig::default_split_store_max_num_bytes().get_bytes() as usize,
            max_num_splits: IndexerConfig::default_split_store_max_num_splits(),
            opened_split_store_roots: HashSet::default(),
        }
    }
}

impl SplitStoreSpaceQuota {
    pub fn new(max_num_splits: usize, max_num_bytes: usize) -> Self {
        Self {
            max_num_splits,
            max_num_bytes,
            ..Default::default()
        }
    }

    pub fn can_fit_split(&self, split_size_in_bytes: usize) -> bool {
        // Avoid storing in the cache when the maximum number of cached files is reached.
        if self.num_splits() >= self.max_num_splits {
            warn!("Failed to cache file: maximum number of files exceeded.");
            return false;
        }

        // Ignore storing a file that cannot fit in remaining space in the cache.
        if split_size_in_bytes > self.available_bytes() {
            warn!("Failed to cache file: maximum size in bytes of cache exceeded.");
            return false;
        }
        true
    }

    pub fn add_split(&mut self, split_size_in_bytes: usize) {
        self.num_splits_in_cache += 1;
        self.size_in_bytes_in_cache += split_size_in_bytes;
    }

    pub fn add_initial_splits(
        &mut self,
        local_storage_root: &Path,
        num_splits: usize,
        splits_size_in_bytes: usize,
    ) -> StorageResult<()> {
        if self.opened_split_store_roots.contains(local_storage_root) {
            return Ok(());
        }

        let total_splits = self.num_splits_in_cache + num_splits;
        if total_splits > self.max_num_splits {
            return Err(StorageErrorKind::InternalError.with_error(anyhow::anyhow!(
                "Initial number of files ({}) exceeds the maximum number ({}) of files allowed.",
                total_splits,
                self.max_num_splits
            )));
        }

        let total_size_in_bytes = self.size_in_bytes_in_cache + splits_size_in_bytes;
        if total_size_in_bytes > self.max_num_bytes {
            return Err(StorageErrorKind::InternalError.with_error(anyhow::anyhow!(
                "Initial cache size ({}) exceeds the maximum size ({}) in bytes allowed.",
                total_size_in_bytes,
                self.max_num_bytes
            )));
        }

        self.opened_split_store_roots
            .insert(local_storage_root.to_path_buf());
        self.num_splits_in_cache = total_splits;
        self.size_in_bytes_in_cache = total_size_in_bytes;
        Ok(())
    }

    pub fn remove_splits(&mut self, num_splits: usize, splits_size_in_bytes: usize) {
        self.num_splits_in_cache -= num_splits;
        self.size_in_bytes_in_cache -= splits_size_in_bytes;
    }

    pub fn num_splits(&self) -> usize {
        self.num_splits_in_cache
    }

    pub fn size_in_bytes(&self) -> usize {
        self.size_in_bytes_in_cache
    }

    fn available_bytes(&self) -> usize {
        self.max_num_bytes - self.size_in_bytes_in_cache
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
/// The splits are stored on the local filesystem in `LocalSplitStore`.
#[derive(Clone)]
pub struct IndexingSplitStore {
    inner: Arc<InnerIndexingSplitStore>,
}

struct InnerIndexingSplitStore {
    /// The remote storage.
    remote_storage: Arc<dyn Storage>,

    local_split_store: Option<Mutex<LocalSplitStore>>,

    /// The merge policy is useful to identify whether a split
    /// should be stored in the local storage or not.
    /// (mature splits do not need to be stored).
    merge_policy: Arc<dyn MergePolicy>,
}

pub struct WeakIndexingSplitStore {
    inner: Weak<InnerIndexingSplitStore>,
}

impl WeakIndexingSplitStore {
    pub fn upgrade(&self) -> Option<IndexingSplitStore> {
        self.inner
            .upgrade()
            .map(|inner| IndexingSplitStore { inner })
    }
}

impl IndexingSplitStore {
    /// Creates an instance of [`IndexingSplitStore`]
    ///
    /// It needs the remote storage to work with.
    pub async fn create_with_local_store(
        remote_storage: Arc<dyn Storage>,
        cache_directory: &Path,
        merge_policy: Arc<dyn MergePolicy>,
        split_store_space_quota: Arc<Mutex<SplitStoreSpaceQuota>>,
    ) -> StorageResult<Self> {
        let local_storage_root = cache_directory.join(SPLIT_CACHE_DIR_NAME);
        std::fs::create_dir_all(&local_storage_root)?;
        let local_split_store =
            LocalSplitStore::open(local_storage_root, split_store_space_quota).await?;

        let inner = InnerIndexingSplitStore {
            remote_storage,
            local_split_store: Some(Mutex::new(local_split_store)),
            merge_policy,
        };
        Ok(Self {
            inner: Arc::new(inner),
        })
    }

    pub fn create_without_local_store(remote_storage: Arc<dyn Storage>) -> Self {
        let inner = InnerIndexingSplitStore {
            remote_storage,
            local_split_store: None,
            merge_policy: default_merge_policy(),
        };
        Self {
            inner: Arc::new(inner),
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
    /// at `split_folder` anymore.
    #[instrument("store_split", skip_all)]
    pub async fn store_split<'a>(
        &'a self,
        split: &'a SplitMetadata,
        split_folder: &'a Path,
        put_payload: Box<dyn PutPayload>,
    ) -> anyhow::Result<()> {
        let start = Instant::now();
        let split_num_bytes = put_payload.len();

        let key = PathBuf::from(quickwit_common::split_file(split.split_id()));
        self.inner
            .remote_storage
            .put(&key, put_payload)
            .instrument(info_span!("storage_put"))
            .await
            .with_context(|| {
                format!(
                    "Failed uploading key {} in bucket {}",
                    key.display(),
                    self.inner.remote_storage.uri()
                )
            })?;
        let elapsed_secs = start.elapsed().as_secs_f32();
        let split_size_in_megabytes = split_num_bytes / 1_000_000;
        let throughput_mb_s = split_size_in_megabytes as f32 / elapsed_secs;
        let is_mature = self.inner.merge_policy.is_mature(split);

        info!(
            split_size_in_megabytes = %split_size_in_megabytes,
            num_docs = %split.num_docs,
            elapsed_secs = %elapsed_secs,
            throughput_mb_s = %throughput_mb_s,
            is_mature = is_mature,
            "store-split-remote-success"
        );

        if !is_mature {
            info!("store-in-cache");
            if let Some(split_store) = &self.inner.local_split_store {
                let mut split_store_guard = split_store.lock().await;
                let tantivy_dir = SplitFolder::new(split_folder.to_path_buf());
                if split_store_guard
                    .move_into_cache(split.split_id(), tantivy_dir, split_num_bytes as usize)
                    .await?
                {
                    return Ok(());
                }
            }
        }
        tokio::fs::remove_dir_all(split_folder).await?;
        Ok(())
    }

    /// Delete a split.
    pub async fn delete(&self, split_id: &str) -> StorageResult<()> {
        let split_filename = quickwit_common::split_file(split_id);
        let split_path = Path::new(&split_filename);
        self.inner.remote_storage.delete(split_path).await?;
        if let Some(local_split_store) = &self.inner.local_split_store {
            let mut local_split_store_guard = local_split_store.lock().await;
            local_split_store_guard.remove_split(split_id).await?;
        }
        Ok(())
    }

    /// Gets a split from the split store, and makes it available to the given `output_path`.
    ///
    /// The output_path is expected to be a directory path.
    pub async fn fetch_split(
        &self,
        split_id: &str,
        output_dir_path: &Path,
    ) -> StorageResult<Box<dyn Directory>> {
        let path = PathBuf::from(quickwit_common::split_file(split_id));
        if let Some(local_split_store) = &self.inner.local_split_store {
            let mut local_split_store_guard = local_split_store.lock().await;
            if let Some(split_folder) = local_split_store_guard
                .get_cached_split(split_id, output_dir_path)
                .await?
            {
                return split_folder.get_tantivy_directory();
            }
        }
        let start_time = Instant::now();
        let dest_filepath = output_dir_path.join(&path);
        info!(split_id = split_id, "fetch-split-from-remote-storage-start");
        self.inner
            .remote_storage
            .copy_to_file(&path, &dest_filepath)
            .await?;
        info!(split_id=split_id,elapsed=?start_time.elapsed(), "fetch-split-from_remote-storage-success");
        get_tantivy_directory_from_split_bundle(&dest_filepath)
    }

    /// Removes the danglings splits.
    /// After a restart, the store might contains splits that are not relevant anymore.
    /// For instance, if the failure happens right before its publication, the split will be in the
    /// split store but not in the metastore.
    pub async fn remove_dangling_splits(
        &self,
        published_splits: &[SplitMetadata],
    ) -> StorageResult<()> {
        if let Some(local_split_store) = &self.inner.local_split_store {
            let published_split_ids: Vec<&str> = published_splits
                .iter()
                .filter(|split| !self.inner.merge_policy.is_mature(split))
                .map(|split| split.split_id())
                .collect();

            return local_split_store
                .lock()
                .await
                .retain_only(&published_split_ids)
                .await;
        }

        Ok(())
    }

    // TODO: remove when merge_pipeline is refactored
    pub fn get_merge_policy(&self) -> Arc<dyn MergePolicy> {
        self.inner.merge_policy.clone()
    }

    pub fn downgrade(&self) -> WeakIndexingSplitStore {
        WeakIndexingSplitStore {
            inner: Arc::downgrade(&self.inner),
        }
    }

    /// Takes a snapshot of the cache view (only used for testing).
    #[cfg(any(test, feature = "testsuite"))]
    pub async fn inspect_local_store(&self) -> HashMap<String, usize> {
        if let Some(split_store) = &self.inner.local_split_store {
            let split_store_guard = split_store.lock().await;
            split_store_guard.inspect()
        } else {
            HashMap::default()
        }
    }
}

#[cfg(test)]
mod test_split_store {
    use std::path::Path;
    use std::sync::Arc;

    use quickwit_metastore::SplitMetadata;
    use quickwit_storage::{
        PutPayload, RamStorage, SplitPayloadBuilder, Storage, StorageError, StorageErrorKind,
    };
    use tempfile::tempdir;
    use tokio::fs;
    use tokio::sync::Mutex;

    use super::IndexingSplitStore;
    use crate::merge_policy::default_merge_policy;
    use crate::split_store::{SplitStoreSpaceQuota, SPLIT_CACHE_DIR_NAME};
    use crate::MergePolicy;

    #[tokio::test]
    async fn test_create_should_error_with_wrong_num_files() -> anyhow::Result<()> {
        let local_dir = tempdir()?;
        let root_path = local_dir.path().join(SPLIT_CACHE_DIR_NAME);
        fs::create_dir_all(&root_path).await?;

        fs::create_dir_all(&root_path.join("a.split")).await?;
        fs::create_dir_all(&root_path.join("b.split")).await?;
        fs::create_dir_all(&root_path.join("c.split")).await?;

        let split_store_space_quota = Arc::new(Mutex::new(SplitStoreSpaceQuota::new(2, 10)));
        let remote_storage = Arc::new(RamStorage::default());
        let result = IndexingSplitStore::create_with_local_store(
            remote_storage,
            local_dir.path(),
            default_merge_policy(),
            split_store_space_quota,
        )
        .await;
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
        let root_path = local_dir.path().join(SPLIT_CACHE_DIR_NAME);
        fs::create_dir_all(&root_path).await?;

        fs::create_dir_all(&root_path.join("a.split")).await?;
        fs::create_dir_all(&root_path.join("b.split")).await?;

        let split_store_space_quota = Arc::new(Mutex::new(SplitStoreSpaceQuota::new(4, 10)));
        let remote_storage = Arc::new(RamStorage::default());
        let result = IndexingSplitStore::create_with_local_store(
            remote_storage,
            local_dir.path(),
            default_merge_policy(),
            split_store_space_quota,
        )
        .await;
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
        let root_path = local_dir.path().join(SPLIT_CACHE_DIR_NAME);
        fs::create_dir_all(&root_path).await?;
        fs::write(root_path.join("b.split"), b"abcd").await?;
        fs::write(root_path.join("a.split"), b"abcdefgh").await?;

        let split_store_space_quota = Arc::new(Mutex::new(SplitStoreSpaceQuota::new(100, 100)));
        let remote_storage = Arc::new(RamStorage::default());
        let result = IndexingSplitStore::create_with_local_store(
            remote_storage,
            local_dir.path(),
            default_merge_policy(),
            split_store_space_quota,
        )
        .await;
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
        let remote_storage = Arc::new(RamStorage::default());
        let split_store = IndexingSplitStore::create_with_local_store(
            remote_storage,
            split_cache_dir.path(),
            default_merge_policy(),
            Arc::new(Mutex::new(SplitStoreSpaceQuota::default())),
        )
        .await?;
        {
            let split_path = temp_dir.path().join("split1");
            fs::create_dir_all(&split_path).await?;
            let split_metadata1 = create_test_split_metadata("split1");

            split_store
                .store_split(&split_metadata1, &split_path, Box::new(vec![1, 2, 3, 4]))
                .await?;
            assert!(!split_path.exists());
            assert!(split_cache_dir
                .path()
                .join(SPLIT_CACHE_DIR_NAME)
                .join("split1.split")
                .exists());
            let local_store_stats = split_store.inspect_local_store().await;
            assert_eq!(local_store_stats.len(), 1);
            assert_eq!(local_store_stats.get("split1").cloned(), Some(4));
        }
        {
            let split_path = temp_dir.path().join("split2");
            fs::create_dir_all(&split_path).await?;
            let split_metadata1 = create_test_split_metadata("split2");
            split_store
                .store_split(
                    &split_metadata1,
                    &split_path,
                    Box::new(SplitPayloadBuilder::get_split_payload(&[], &[5, 5, 5])?),
                )
                .await?;
            assert!(!split_path.exists());
            assert!(split_cache_dir
                .path()
                .join(SPLIT_CACHE_DIR_NAME)
                .join("split2.split")
                .exists());
            let local_store_stats = split_store.inspect_local_store().await;
            assert_eq!(local_store_stats.len(), 2);
            assert_eq!(local_store_stats.get("split1").cloned(), Some(4));
            assert_eq!(local_store_stats.get("split2").cloned(), Some(31));
        }

        Ok(())
    }

    #[tokio::test]
    async fn test_put_should_not_store_in_cache_when_max_num_files_reached() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;

        let split_cache_dir = tempdir()?;
        let remote_storage = Arc::new(RamStorage::default());
        let split_store = IndexingSplitStore::create_with_local_store(
            remote_storage,
            split_cache_dir.path(),
            default_merge_policy(),
            Arc::new(Mutex::new(SplitStoreSpaceQuota::new(1, 1_000_000))),
        )
        .await?;

        {
            let split_path = temp_dir.path().join("split1");
            fs::create_dir_all(&split_path).await?;
            let split_metadata1 = create_test_split_metadata("split1");
            split_store
                .store_split(
                    &split_metadata1,
                    &split_path,
                    Box::new(SplitPayloadBuilder::get_split_payload(&[], &[5, 5, 5])?),
                )
                .await?;
            assert!(!split_path.exists());
            assert!(split_cache_dir
                .path()
                .join(SPLIT_CACHE_DIR_NAME)
                .join("split1.split")
                .exists());
            let local_store_stats = split_store.inspect_local_store().await;
            assert_eq!(local_store_stats.len(), 1);
            assert_eq!(local_store_stats.get("split1").cloned(), Some(31));
        }
        {
            let split_path = temp_dir.path().join("split2");
            fs::create_dir_all(&split_path).await?;
            let split_metadata2 = create_test_split_metadata("split2");

            split_store
                .store_split(
                    &split_metadata2,
                    &split_path,
                    Box::new(SplitPayloadBuilder::get_split_payload(&[], &[5, 5, 5])?),
                )
                .await?;
            assert!(!split_path.exists());
            assert!(!split_cache_dir
                .path()
                .join(SPLIT_CACHE_DIR_NAME)
                .join("split2.split")
                .exists());
            let local_store_stats = split_store.inspect_local_store().await;
            assert_eq!(local_store_stats.len(), 1);
            assert_eq!(local_store_stats.get("split1").cloned(), Some(31));
        }
        {
            let output = tempfile::tempdir()?;
            // get from cache
            let _split1 = split_store.fetch_split("split1", output.path()).await?;
            // get from remote storage
            let _split2 = split_store.fetch_split("split2", output.path()).await?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_put_should_not_store_in_cache_when_max_num_bytes_reached() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;

        let split_cache_dir = tempdir()?;
        let remote_storage = Arc::new(RamStorage::default());
        let split_store = IndexingSplitStore::create_with_local_store(
            remote_storage,
            split_cache_dir.path(),
            default_merge_policy(),
            Arc::new(Mutex::new(SplitStoreSpaceQuota::new(10, 40))),
        )
        .await?;

        {
            let split_path = temp_dir.path().join("split1");
            fs::create_dir_all(&split_path).await?;
            let split_metadata1 = create_test_split_metadata("split1");
            split_store
                .store_split(
                    &split_metadata1,
                    &split_path,
                    Box::new(SplitPayloadBuilder::get_split_payload(&[], &[5, 5, 5])?),
                )
                .await?;
            assert!(!split_path.exists());
            assert!(split_cache_dir
                .path()
                .join(SPLIT_CACHE_DIR_NAME)
                .join("split1.split")
                .exists());
            let local_store_stats = split_store.inspect_local_store().await;
            assert_eq!(local_store_stats.len(), 1);
            assert_eq!(local_store_stats.get("split1").cloned(), Some(31));
        }

        {
            let split_path = temp_dir.path().join("split2");
            fs::create_dir_all(&split_path).await?;
            let split_metadata2 = create_test_split_metadata("split2");
            split_store
                .store_split(
                    &split_metadata2,
                    &split_path,
                    Box::new(SplitPayloadBuilder::get_split_payload(&[], &[5, 5, 5])?),
                )
                .await?;
            assert!(!split_path.exists());
            assert!(!split_cache_dir
                .path()
                .join(SPLIT_CACHE_DIR_NAME)
                .join("split2.split")
                .exists());
            let local_store_stats = split_store.inspect_local_store().await;
            assert_eq!(local_store_stats.len(), 1);
            assert_eq!(local_store_stats.get("split2").cloned(), None);
        }
        Ok(())
    }

    #[tokio::test]
    async fn test_delete_should_remove_from_both_storage() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;

        let split_cache_dir = tempdir()?;
        let remote_storage = Arc::new(RamStorage::default());
        let split_store = IndexingSplitStore::create_with_local_store(
            remote_storage.clone(),
            split_cache_dir.path(),
            default_merge_policy(),
            Arc::new(Mutex::new(SplitStoreSpaceQuota::new(10, 40))),
        )
        .await?;

        let split_streamer = SplitPayloadBuilder::get_split_payload(&[], &[5, 5, 5])?;
        {
            let split_path = temp_dir.path().join("split2");
            fs::create_dir_all(&split_path).await?;
            let split_metadata1 = create_test_split_metadata("split1");
            split_store
                .store_split(
                    &split_metadata1,
                    &split_path,
                    Box::new(split_streamer.clone()),
                )
                .await?;
            assert!(!split_path.exists());
            assert!(split_cache_dir
                .path()
                .join(SPLIT_CACHE_DIR_NAME)
                .join("split1.split")
                .exists());
            let local_store_stats = split_store.inspect_local_store().await;
            assert_eq!(local_store_stats.len(), 1);
            assert_eq!(local_store_stats.get("split1").cloned(), Some(31));
        }

        let split1_bytes = remote_storage.get_all(Path::new("split1.split")).await?;
        assert_eq!(split1_bytes, &split_streamer.read_all().await?);

        split_store.delete("split1").await?;

        let storage_err = remote_storage
            .get_all(Path::new("split1.split"))
            .await
            .unwrap_err();
        assert_eq!(storage_err.kind(), StorageErrorKind::DoesNotExist);

        Ok(())
    }

    #[tokio::test]
    async fn test_remove_danglings_splits_should_remove_files() -> anyhow::Result<()> {
        let local_dir = tempdir()?;
        let root_path = local_dir.path().join(SPLIT_CACHE_DIR_NAME);
        fs::create_dir_all(&root_path).await?;
        fs::create_dir_all(&root_path.join("a.split")).await?;
        fs::create_dir_all(&root_path.join("b.split")).await?;
        fs::create_dir_all(&root_path.join("c.split")).await?;
        fs::write(root_path.join("a.split").join("termdict"), b"a").await?;
        fs::write(root_path.join("b.split").join("termdict"), b"b").await?;
        fs::write(root_path.join("c.split").join("termdict"), b"c").await?;

        let split_store_space_quota = Arc::new(Mutex::new(SplitStoreSpaceQuota::new(100, 200)));
        let remote_storage = Arc::new(RamStorage::default());
        let split_store = IndexingSplitStore::create_with_local_store(
            remote_storage,
            local_dir.path(),
            default_merge_policy(),
            split_store_space_quota.clone(),
        )
        .await?;
        let initial_size = {
            let split_store_space_quota_guard = split_store_space_quota.lock().await;
            assert_eq!(split_store_space_quota_guard.num_splits(), 3);
            split_store_space_quota_guard.size_in_bytes()
        };
        let published_splits = vec![SplitMetadata {
            split_id: "b".to_string(),
            footer_offsets: 5..20,
            ..Default::default()
        }];
        split_store
            .remove_dangling_splits(&published_splits)
            .await?;
        assert!(!root_path.join("a.split").as_path().exists());
        assert!(!root_path.join("c.split").as_path().exists());
        assert!(root_path.join("b.split").as_path().exists());
        let split_store_space_quota_guard = split_store_space_quota.lock().await;
        assert_eq!(split_store_space_quota_guard.num_splits(), 1);
        assert!(split_store_space_quota_guard.size_in_bytes() < initial_size);
        Ok(())
    }

    #[tokio::test]
    async fn test_mature_splits() -> anyhow::Result<()> {
        #[derive(Debug)]
        struct SplitsAreMature {}
        impl MergePolicy for SplitsAreMature {
            fn operations(
                &self,
                _: &mut Vec<SplitMetadata>,
            ) -> Vec<crate::merge_policy::MergeOperation> {
                unimplemented!()
            }
            fn is_mature(&self, _: &SplitMetadata) -> bool {
                true
            }
        }
        let temp_dir = tempfile::tempdir()?;
        let split_cache_dir = tempdir()?;
        let remote_storage = Arc::new(RamStorage::default());
        let split_store = IndexingSplitStore::create_with_local_store(
            remote_storage,
            split_cache_dir.path(),
            Arc::new(SplitsAreMature {}),
            Arc::new(Mutex::new(SplitStoreSpaceQuota::default())),
        )
        .await?;
        {
            let split_path = temp_dir.path().join("split1");
            fs::create_dir_all(&split_path).await?;
            let file_in_split = split_path.join("myfile");
            fs::write(&file_in_split, b"abcdefgh").await?;
            let split_metadata1 = create_test_split_metadata("split1");

            split_store
                .store_split(
                    &split_metadata1,
                    &split_path,
                    Box::new(SplitPayloadBuilder::get_split_payload(
                        &[file_in_split.to_owned()],
                        &[1, 2, 3],
                    )?),
                )
                .await?;
            assert!(!split_path.exists());
            assert!(!split_cache_dir
                .path()
                .join(SPLIT_CACHE_DIR_NAME)
                .join("split1.split")
                .exists());
            let local_store_stats = split_store.inspect_local_store().await;
            assert_eq!(local_store_stats.len(), 0);
            assert_eq!(local_store_stats.get("split1").cloned(), None);
        }

        Ok(())
    }
}
