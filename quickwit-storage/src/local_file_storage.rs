/*
    Quickwit
    Copyright (C) 2021 Quickwit Inc.

    Quickwit is offered under the AGPL v3.0 and as commercial software.
    For commercial licensing, contact us at hello@quickwit.io.

    AGPL:
    This program is free software: you can redistribute it and/or modify
    it under the terms of the GNU Affero General Public License as
    published by the Free Software Foundation, either version 3 of the
    License, or (at your option) any later version.

    This program is distributed in the hope that it will be useful,
    but WITHOUT ANY WARRANTY; without even the implied warranty of
    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
    GNU Affero General Public License for more details.

    You should have received a copy of the GNU Affero General Public License
    along with this program.  If not, see <http://www.gnu.org/licenses/>.
*/

use crate::{PutPayload, Storage, StorageErrorKind, StorageFactory, StorageResult};
use async_trait::async_trait;
use std::fmt;
use std::io::{ErrorKind, SeekFrom};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

/// File system compatible storage implementation.
#[derive(Clone)]
pub struct LocalFileStorage {
    root: PathBuf,
}

impl fmt::Debug for LocalFileStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LocalFileStorage(root={:?})", &self.root)
    }
}

impl LocalFileStorage {
    /// Creates a file storage instance given a root path.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Creates a file storage instance given a uri
    pub fn from_uri(uri: &str) -> StorageResult<LocalFileStorage> {
        let root_path = uri.split("://").nth(1).ok_or_else(|| {
            StorageErrorKind::DoesNotExist.with_error(anyhow::anyhow!("Invalid root path: {}", uri))
        })?;
        Ok(LocalFileStorage::new(PathBuf::from(root_path)))
    }
}

#[async_trait]
impl Storage for LocalFileStorage {
    async fn put(&self, path: &Path, payload: PutPayload) -> crate::StorageResult<()> {
        let full_path = self.root.join(path);
        match payload {
            PutPayload::InMemory(data) => {
                fs::write(full_path, data).await?;
            }
            PutPayload::LocalFile(filepath) => {
                fs::copy(filepath, full_path).await?;
            }
        };
        Ok(())
    }

    async fn copy_to_file(&self, path: &Path, output_path: &Path) -> StorageResult<()> {
        let full_path = self.root.join(path);
        fs::copy(full_path, output_path).await?;
        Ok(())
    }

    async fn get_slice(&self, path: &Path, range: Range<usize>) -> StorageResult<Vec<u8>> {
        let full_path = self.root.join(path);
        let mut file = fs::File::open(full_path).await?;
        file.seek(SeekFrom::Start(range.start as u64)).await?;
        let mut content_bytes = vec![0u8; range.len()];
        file.read_exact(&mut content_bytes).await?;
        Ok(content_bytes)
    }

    async fn delete(&self, path: &Path) -> StorageResult<()> {
        let full_path = self.root.join(path);
        fs::remove_file(full_path).await?;
        Ok(())
    }

    async fn get_all(&self, path: &Path) -> StorageResult<Vec<u8>> {
        let full_path = self.root.join(path);
        let content_bytes = fs::read(full_path).await?;
        Ok(content_bytes)
    }

    fn uri(&self) -> String {
        format!("file://{}", self.root.to_string_lossy())
    }

    async fn exists(&self, path: &Path) -> StorageResult<bool> {
        let full_path = self.root.join(path);
        match fs::metadata(full_path).await {
            Ok(_) => Ok(true),
            Err(err) => {
                if err.kind() == ErrorKind::NotFound {
                    Ok(false)
                } else {
                    Err(err.into())
                }
            }
        }
    }
}

/// A File storage resolver
#[derive(Debug, Clone)]
pub struct LocalFileStorageFactory {}

impl Default for LocalFileStorageFactory {
    fn default() -> Self {
        LocalFileStorageFactory {}
    }
}

impl StorageFactory for LocalFileStorageFactory {
    fn protocol(&self) -> String {
        "file".to_string()
    }

    fn resolve(&self, uri: &str) -> StorageResult<Arc<dyn Storage>> {
        if !uri.starts_with("file://") {
            let err_msg = anyhow::anyhow!(
                "{:?} is an invalid file storage uri. Only file:// is accepted.",
                uri
            );
            return Err(StorageErrorKind::DoesNotExist.with_error(err_msg));
        }

        let storage = LocalFileStorage::from_uri(uri)?;
        Ok(Arc::new(storage))
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::tests::storage_test_suite;

    #[tokio::test]
    async fn test_storage() -> anyhow::Result<()> {
        let path_root = tempdir()?;
        let mut file_storage = LocalFileStorage::new(path_root.into_path());
        storage_test_suite(&mut file_storage).await?;
        Ok(())
    }

    #[test]
    fn test_file_storage_factory() -> anyhow::Result<()> {
        let file_storage_factory = LocalFileStorageFactory::default();
        let storage = file_storage_factory.resolve("file://foo/bar")?;
        assert_eq!(storage.uri().as_str(), "file://foo/bar");

        let err = file_storage_factory
            .resolve("test://foo/bar")
            .err()
            .unwrap();
        assert_eq!(err.kind(), StorageErrorKind::DoesNotExist);

        let err = file_storage_factory.resolve("test://").err().unwrap();
        assert_eq!(err.kind(), StorageErrorKind::DoesNotExist);
        Ok(())
    }
}
