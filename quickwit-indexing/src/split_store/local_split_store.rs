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

use std::collections::{HashMap, HashSet};
use std::fs::DirEntry;
use std::path::{Path, PathBuf};
use std::{fs, io};

use quickwit_common::split_file;
use quickwit_directories::BundleDirectory;
use quickwit_storage::{StorageErrorKind, StorageResult};
use tantivy::directory::MmapDirectory;
use tantivy::Directory;
use tracing::{error, warn};

use super::IndexingSplitStoreParams;

#[derive(Clone, Debug)]
pub struct BundledSplitFile {
    pub path: PathBuf,
}
impl BundledSplitFile {
    pub fn new(path: PathBuf) -> Self {
        BundledSplitFile { path }
    }
}

impl BundledSplitFile {
    pub fn get_tantivy_directory(&self) -> StorageResult<Box<dyn Directory>> {
        let mmap_directory = MmapDirectory::open(self.path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("Couldn't find parent for {:?}", &self.path),
            )
        })?)?;
        let split_fileslice = mmap_directory.open_read(Path::new(&self.path))?;
        Ok(Box::new(BundleDirectory::open_split(split_fileslice)?))
    }

    /// Moves the underlying data to a new location.
    async fn move_to(&mut self, new_folder: &Path, split_id: &str) -> StorageResult<()> {
        let new_path = PathBuf::from(split_file(split_id));
        let to_full_path = new_folder.join(new_path);
        tokio::fs::rename(&self.path, &to_full_path).await?;
        self.path = to_full_path.to_path_buf();
        Ok(())
    }

    async fn delete(&self) -> io::Result<()> {
        missing_file_is_ok(tokio::fs::remove_file(&self.path).await)?;
        Ok(())
    }
}

fn missing_file_is_ok(io_result: io::Result<()>) -> io::Result<()> {
    match io_result {
        Ok(()) => Ok(()),
        Err(io_err) if io_err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(io_err) => Err(io_err),
    }
}

// TODO add folder support
fn split_id_from_filename(dir_entry: &DirEntry) -> Option<String> {
    if !dir_entry.path().is_file() {
        return None;
    }
    let split_filename = dir_entry.file_name().into_string().ok()?;
    split_filename
        .strip_suffix(".split")
        .map(ToString::to_string)
}

#[derive(Debug, Eq, PartialEq)]
struct SizeInCache {
    pub num_splits: usize,
    pub size_in_bytes: usize,
}

pub struct LocalSplitStore {
    /// The parameters of the cache.
    params: IndexingSplitStoreParams,
    /// Splits owned by the local split store, which reside in the split_store_folder.
    /// SplitId -> (Split Num Bytes, BundledSplitFile)
    split_files: HashMap<String, (usize, BundledSplitFile)>,
    /// The root folder where all data is moved into.
    split_store_folder: PathBuf,
}

impl LocalSplitStore {
    /// Try to open an existing local split directory.
    ///
    /// All files finishing by .split will be considered to be part of the directory.
    pub fn open(
        local_storage_root: PathBuf,
        params: IndexingSplitStoreParams,
    ) -> StorageResult<LocalSplitStore> {
        let mut split_files: HashMap<String, (usize, BundledSplitFile)> = HashMap::new();
        let mut total_size_in_bytes: usize = 0;
        for dir_entry_result in fs::read_dir(&local_storage_root)? {
            let dir_entry = dir_entry_result?;
            if let Some(split_id) = split_id_from_filename(&dir_entry) {
                // TODO support folder containing tantivy index
                let split_file = BundledSplitFile::new(dir_entry.path());
                let split_num_bytes = dir_entry.metadata()?.len() as usize;
                total_size_in_bytes += split_num_bytes;
                split_files.insert(split_id, (split_num_bytes, split_file));
            }
        }

        if split_files.len() > params.max_num_splits {
            return Err(StorageErrorKind::InternalError.with_error(anyhow::anyhow!(
                "Initial number of files exceeds the maximum number of files allowed.",
            )));
        }

        if total_size_in_bytes > params.max_num_bytes {
            return Err(StorageErrorKind::InternalError.with_error(anyhow::anyhow!(
                "Initial cache size exceeds the maximum size in bytes allowed.",
            )));
        }

        Ok(LocalSplitStore {
            split_store_folder: local_storage_root,
            params,
            split_files,
        })
    }

    /// Clean the split store.
    /// By only keeping the splits specified and removing other
    /// existing splits in this store.
    pub async fn retain_only(&mut self, split_ids: &[&str]) -> StorageResult<()> {
        let stored_ids_set: HashSet<String> = self.split_files.keys().cloned().collect();
        let to_retain_ids_set: HashSet<String> = split_ids
            .iter()
            .map(|split_id| split_id.to_string())
            .collect();
        let to_remove_ids_set = &stored_ids_set - &to_retain_ids_set;

        for split_id in to_remove_ids_set {
            self.remove_split(&split_id).await?;
        }
        Ok(())
    }

    #[cfg(test)]
    pub fn inspect(&self) -> HashMap<String, usize> {
        self.split_files
            .iter()
            .map(|(k, v)| (k.to_string(), v.0))
            .collect()
    }

    pub async fn remove_split(&mut self, split_id: &str) -> StorageResult<()> {
        if !self.split_files.contains_key(split_id) {
            return Ok(());
        }
        if let Some((_, split_file)) = self.split_files.remove(split_id) {
            split_file.delete().await?;
        }
        Ok(())
    }

    /// Moves a split into the store.
    /// from here is an external path, and to is an internal path.
    pub async fn move_into(
        &self,
        split: &mut BundledSplitFile,
        to_full_path: &Path,
        split_id: &str,
    ) -> StorageResult<()> {
        split.move_to(to_full_path, split_id).await?;
        Ok(())
    }

    /// Moves a split within the store to an external path.
    /// from here is an internal path, and to is an external path.
    pub async fn move_out(&mut self, split_id: &str, to: &Path) -> StorageResult<BundledSplitFile> {
        let mut split_file = self
            .split_files
            .remove(split_id)
            .ok_or_else(|| {
                StorageErrorKind::DoesNotExist
                    .with_error(anyhow::anyhow!("Missing split_id `{}`", split_id))
            })?
            .1;
        split_file.move_to(to, split_id).await?;
        Ok(split_file)
    }

    pub async fn get_cached_split(
        &mut self,
        split_id: &str,
        output_dir_path: &Path,
    ) -> StorageResult<Option<BundledSplitFile>> {
        if !self.split_files.contains_key(split_id) {
            return Ok(None);
        }
        let split_file_res = self.move_out(split_id, output_dir_path).await;
        match split_file_res {
            Ok(split_file) => {
                self.split_files.remove(split_id);
                Ok(Some(split_file))
            }
            Err(storage_err) if storage_err.kind() == StorageErrorKind::DoesNotExist => {
                error!(split_id = split_id, error = ?storage_err, "Cached split file/folder is missing.");
                self.split_files.remove(split_id);
                Ok(None)
            }
            Err(storage_err) => Err(storage_err),
        }
    }

    fn size_in_store(&self) -> SizeInCache {
        let size_in_bytes = self
            .split_files
            .values()
            .map(|(size, _)| size)
            .cloned()
            .sum::<usize>();
        SizeInCache {
            num_splits: self.split_files.len(),
            size_in_bytes,
        }
    }

    /// Tries to move a `split` file into the cache.
    ///
    /// Move is not an image here. We are litterally moving files.
    ///
    /// If the cache capacity does not allow it, this function
    /// just logs a warning and returns Ok(false).
    ///
    /// Ok(true) means the file was effectively accepted.
    pub async fn move_into_cache<'a>(
        &'a mut self,
        split_id: &'a str,
        mut split_file: BundledSplitFile,
        split_num_bytes: usize,
    ) -> io::Result<bool> {
        let size_in_cache = self.size_in_store();

        // Avoid storing in the cache when the maximum number of cached files is reached.
        if size_in_cache.num_splits + 1 > self.params.max_num_splits {
            warn!("Failed to cache file: maximum number of files exceeded.");
            return Ok(false);
        }

        // Ignore storing a file that cannot fit in remaining space in the cache.
        if split_num_bytes + size_in_cache.size_in_bytes > self.params.max_num_bytes {
            warn!("Failed to cache file: maximum size in bytes of cache exceeded.");
            return Ok(false);
        }

        self.move_into(&mut split_file, &self.split_store_folder, split_id)
            .await?;

        self.split_files
            .insert(split_id.to_string(), (split_num_bytes, split_file));
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_local_split_store_load_existing_splits() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        tokio::fs::write(&temp_dir.path().join("split1.split"), b"split-content").await?;
        tokio::fs::write(&temp_dir.path().join("split2.split"), b"split-content2").await?;
        tokio::fs::write(&temp_dir.path().join("different-file"), b"split-content").await?;
        tokio::fs::create_dir(&temp_dir.path().join("not-a-split.split")).await?;
        let params = IndexingSplitStoreParams::default();
        let split_store = LocalSplitStore::open(temp_dir.path().to_path_buf(), params)?;
        let cache_content = split_store.inspect();
        assert_eq!(cache_content.len(), 2);
        assert_eq!(cache_content.get("split1").cloned(), Some(13));
        assert_eq!(cache_content.get("split2").cloned(), Some(14));
        assert_eq!(
            split_store.size_in_store(),
            SizeInCache {
                num_splits: 2,
                size_in_bytes: 27
            }
        );
        Ok(())
    }
}
