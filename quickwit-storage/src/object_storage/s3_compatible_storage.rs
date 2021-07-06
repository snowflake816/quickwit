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

use super::error::RusotoErrorWrapper;
use anyhow::Context;
use async_trait::async_trait;
use bytes::Bytes;
use futures::stream;
use futures::StreamExt;
use once_cell::sync::OnceCell;
use regex::Regex;
use rusoto_core::credential::DefaultCredentialsProvider;
use std::fmt::{self, Debug};
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use tokio::fs::File;
use tokio_util::io::ReaderStream;
use tracing::warn;

use crate::retry::{retry, IsRetryable, Retry};
use rusoto_core::{ByteStream, HttpClient, HttpConfig, Region, RusotoError};
use rusoto_s3::{
    AbortMultipartUploadRequest, CompleteMultipartUploadRequest, CompletedMultipartUpload,
    CompletedPart, CreateMultipartUploadError, CreateMultipartUploadRequest, DeleteObjectRequest,
    GetObjectRequest, HeadObjectError, HeadObjectRequest, PutObjectError, PutObjectRequest,
    S3Client, UploadPartRequest, S3,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

use crate::object_storage::file_slice_stream::FileSliceStream;
use crate::object_storage::MultiPartPolicy;
use crate::{PutPayload, Storage, StorageErrorKind};
use crate::{StorageError, StorageResult};

/// S3 Compatible object storage implementation.
pub struct S3CompatibleObjectStorage {
    s3_client: S3Client,
    bucket: String,
    prefix: PathBuf,
    multipart_policy: MultiPartPolicy,
}

impl fmt::Debug for S3CompatibleObjectStorage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "S3CompatibleObjectStorage(bucket={},prefix={:?})",
            &self.bucket, &self.prefix
        )
    }
}

fn create_s3_client(region: Region) -> anyhow::Result<S3Client> {
    let credentials_provider = DefaultCredentialsProvider::new()
        .with_context(|| "Failed to fetch credentials for the object storage.")?;
    let mut http_config: HttpConfig = HttpConfig::default();
    // We experience an issue similar to https://github.com/hyperium/hyper/issues/2312.
    // It seems like the setting below solved it.
    http_config.pool_idle_timeout(std::time::Duration::from_secs(10));
    let http_client = HttpClient::new_with_config(http_config)
        .with_context(|| "failed to create request dispatcher")?;
    Ok(S3Client::new_with(
        http_client,
        credentials_provider,
        region,
    ))
}

impl S3CompatibleObjectStorage {
    /// Creates an object storage given a region and a bucket name.
    pub fn new(region: Region, bucket: &str) -> anyhow::Result<S3CompatibleObjectStorage> {
        let s3_client = create_s3_client(region)?;
        Ok(S3CompatibleObjectStorage {
            s3_client,
            bucket: bucket.to_string(),
            prefix: PathBuf::new(),
            multipart_policy: MultiPartPolicy::default(),
        })
    }

    /// Creates an object storage given a region and an uri.
    pub fn from_uri(region: Region, uri: &str) -> crate::StorageResult<S3CompatibleObjectStorage> {
        let (bucket, path) = parse_split_uri(uri).ok_or_else(|| {
            crate::StorageErrorKind::Io.with_error(anyhow::anyhow!("Invalid uri: {}", uri))
        })?;
        let s3_compatible_storage = S3CompatibleObjectStorage::new(region, &bucket)
            .map_err(|err| crate::StorageErrorKind::Service.with_error(anyhow::anyhow!(err)))?;
        Ok(s3_compatible_storage.with_prefix(&path))
    }

    /// Sets a specific for all buckets.
    ///
    /// This method overrides any existing prefix. (It does NOT
    /// append the argument to any existing prefix.)
    pub fn with_prefix(self, prefix: &Path) -> Self {
        S3CompatibleObjectStorage {
            s3_client: self.s3_client,
            bucket: self.bucket,
            prefix: prefix.to_path_buf(),
            multipart_policy: self.multipart_policy,
        }
    }

    /// Sets the multipart policy.
    ///
    /// See `MultiPartPolicy`.
    pub fn set_policy(&mut self, multipart_policy: MultiPartPolicy) {
        self.multipart_policy = multipart_policy;
    }
}

pub fn parse_split_uri(split_uri: &str) -> Option<(String, PathBuf)> {
    static SPLIT_URI_PTN: OnceCell<Regex> = OnceCell::new();
    SPLIT_URI_PTN
        .get_or_init(|| {
            // s3://bucket/path/to/split or s3+localstack://bucket/path/to/split
            Regex::new(r"s3(\+[^:]+)?://(?P<bucket>[^/]+)/(?P<path>.*)").unwrap()
        })
        .captures(split_uri)
        .and_then(|cap| match (cap.name("bucket"), cap.name("path")) {
            (Some(bucket_match), Some(path_match)) => Some((
                bucket_match.as_str().to_string(),
                PathBuf::from(path_match.as_str()),
            )),
            _ => None,
        })
}

#[derive(Debug, Clone)]
struct MultipartUploadId(pub String);

#[derive(Debug, Clone)]
struct Part {
    pub part_number: usize,
    pub range: Range<u64>,
    pub md5: md5::Digest,
}

impl Part {
    fn len(&self) -> u64 {
        self.range.end - self.range.start
    }
}

async fn range_byte_stream(payload: &PutPayload, range: Range<u64>) -> io::Result<ByteStream> {
    match payload {
        PutPayload::LocalFile(filepath) => {
            let file: tokio::fs::File = tokio::fs::File::open(&filepath).await?;
            let file_slice_stream = FileSliceStream::try_new(file, range).await?;
            Ok(ByteStream::new(file_slice_stream))
        }
        PutPayload::InMemory(data) => {
            let bytes: &[u8] = &data[range.start as usize..range.end as usize];
            Ok(ByteStream::from(bytes.to_vec()))
        }
    }
}

fn split_range_into_chunks(len: u64, chunk_size: u64) -> Vec<Range<u64>> {
    (0..len)
        .step_by(chunk_size as usize)
        .map(move |start| Range {
            start,
            end: (start + chunk_size).min(len),
        })
        .collect()
}

async fn byte_stream(payload: &PutPayload) -> io::Result<ByteStream> {
    match payload {
        PutPayload::LocalFile(filepath) => {
            let file: tokio::fs::File = tokio::fs::File::open(&filepath).await?;
            let reader_stream = ReaderStream::new(file);
            Ok(ByteStream::new(reader_stream))
        }
        PutPayload::InMemory(data) => Ok(ByteStream::from(data.to_vec())),
    }
}

impl S3CompatibleObjectStorage {
    fn uri(&self, relative_path: &Path) -> String {
        format!("s3://{}/{}", &self.bucket, self.key(relative_path))
    }

    fn key(&self, relative_path: &Path) -> String {
        let key_path = self.prefix.join(relative_path);
        key_path.to_string_lossy().to_string()
    }

    async fn put_single_part_single_try(
        &self,
        key: &str,
        payload: PutPayload,
        len: u64,
    ) -> Result<(), RusotoErrorWrapper<PutObjectError>> {
        let body = byte_stream(&payload).await?;
        let request = PutObjectRequest {
            bucket: self.bucket.clone(),
            key: key.to_string(),
            body: Some(body),
            content_length: Some(len as i64),
            ..Default::default()
        };
        self.s3_client.put_object(request).await?;
        Ok(())
    }

    async fn put_single_part(&self, key: &str, payload: PutPayload, len: u64) -> StorageResult<()> {
        retry(|| self.put_single_part_single_try(key, payload.clone(), len)).await?;
        Ok(())
    }

    async fn create_multipart_upload(
        &self,
        key: &str,
    ) -> Result<MultipartUploadId, RusotoErrorWrapper<CreateMultipartUploadError>> {
        let create_upload_req = CreateMultipartUploadRequest {
            bucket: self.bucket.clone(),
            key: key.to_string(),
            ..Default::default()
        };
        let upload_id = retry(|| async {
            self.s3_client
                .create_multipart_upload(create_upload_req.clone())
                .await
                .map_err(RusotoErrorWrapper::from)
        })
        .await?
        .upload_id
        .ok_or_else(|| {
            RusotoError::ParseError("The returned multipart upload id was null.".to_string())
        })?;
        Ok(MultipartUploadId(upload_id))
    }

    async fn create_multipart_requests(
        &self,
        payload: PutPayload,
        len: u64,
        part_len: u64,
    ) -> io::Result<Vec<Part>> {
        assert!(len > 0);
        let chunks = split_range_into_chunks(len, part_len);
        // Note that it should really be the first chunk, but who knows... and it is very cheap to compute this anyway.
        let largest_chunk_num_bytes = chunks
            .iter()
            .map(|chunk| chunk.end - chunk.start)
            .max()
            .expect("The policy should never emit an empty list of chunk.");
        match payload {
            PutPayload::LocalFile(file_path) => {
                let mut parts = Vec::with_capacity(chunks.len());
                let mut file: tokio::fs::File = tokio::fs::File::open(&file_path).await?;
                let mut buf = vec![0u8; largest_chunk_num_bytes as usize];
                for (chunk_id, chunk) in chunks.into_iter().enumerate() {
                    let chunk_len = (chunk.end - chunk.start) as usize;
                    file.read_exact(&mut buf[..chunk_len]).await?;
                    let md5 = md5::compute(&buf[..chunk_len]);
                    let part = Part {
                        part_number: chunk_id + 1, // parts are 1-indexed
                        range: chunk,
                        md5,
                    };
                    parts.push(part);
                }
                Ok(parts)
            }
            PutPayload::InMemory(buffer) => Ok(chunks
                .into_iter()
                .enumerate()
                .map(|(chunk_id, range)| {
                    let md5 = md5::compute(&buffer[range.start as usize..range.end as usize]);
                    Part {
                        part_number: chunk_id + 1, // parts are 1-indexed
                        range,
                        md5,
                    }
                })
                .collect()),
        }
    }

    async fn upload_part(
        &self,
        upload_id: MultipartUploadId,
        key: &str,
        part: Part,
        payload: PutPayload,
    ) -> Result<CompletedPart, Retry<StorageError>> {
        let byte_stream = range_byte_stream(&payload, part.range.clone())
            .await
            .map_err(StorageError::from)
            .map_err(Retry::NotRetryable)?;
        let md5 = base64::encode(part.md5.0);
        let upload_part_req = UploadPartRequest {
            bucket: self.bucket.clone(),
            key: key.to_string(),
            body: Some(byte_stream),
            content_length: Some(part.len() as i64),
            content_md5: Some(md5),
            part_number: part.part_number as i64,
            upload_id: upload_id.0,
            ..Default::default()
        };
        let upload_part_output = self
            .s3_client
            .upload_part(upload_part_req)
            .await
            .map_err(RusotoErrorWrapper::from)
            .map_err(|rusoto_err| {
                if rusoto_err.is_retryable() {
                    Retry::Retryable(StorageError::from(rusoto_err))
                } else {
                    Retry::NotRetryable(StorageError::from(rusoto_err))
                }
            })?;
        Ok(CompletedPart {
            e_tag: upload_part_output.e_tag,
            part_number: Some(part.part_number as i64),
        })
    }

    async fn put_multi_part(
        &self,
        key: &str,
        payload: PutPayload,
        part_len: u64,
        len: u64,
    ) -> StorageResult<()> {
        let upload_id = self
            .create_multipart_upload(key)
            .await
            .map_err(RusotoErrorWrapper::from)?;
        let parts = self
            .create_multipart_requests(payload.clone(), len, part_len)
            .await?;
        let max_concurrent_upload = self.multipart_policy.max_concurrent_upload();
        let completed_parts_res: StorageResult<Vec<CompletedPart>> =
            stream::iter(parts.into_iter().map(|part| {
                let payload = payload.clone();
                let upload_id = upload_id.clone();
                retry(move || {
                    self.upload_part(upload_id.clone(), key, part.clone(), payload.clone())
                })
            }))
            .buffered(max_concurrent_upload)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .map(|res| res.map_err(|e| e.into_inner()))
            .collect();
        match completed_parts_res {
            Ok(completed_parts) => {
                self.complete_multipart_upload(key, completed_parts, &upload_id.0)
                    .await
            }
            Err(upload_error) => {
                let abort_multipart_upload_res: StorageResult<()> =
                    self.abort_multipart_upload(key, &upload_id.0).await;
                if let Err(abort_error) = abort_multipart_upload_res {
                    warn!(
                        key = %key,
                        error = %abort_error,
                        "Failed to abort multipart upload"
                    );
                }
                Err(upload_error)
            }
        }
    }

    async fn complete_multipart_upload(
        &self,
        key: &str,
        completed_parts: Vec<CompletedPart>,
        upload_id: &str,
    ) -> StorageResult<()> {
        let completed_upload = CompletedMultipartUpload {
            parts: Some(completed_parts),
        };
        let complete_upload_req = CompleteMultipartUploadRequest {
            bucket: self.bucket.clone(),
            key: key.to_string(),
            multipart_upload: Some(completed_upload),
            upload_id: upload_id.to_string(),
            ..Default::default()
        };
        retry(|| async {
            self.s3_client
                .complete_multipart_upload(complete_upload_req.clone())
                .await
                .map_err(RusotoErrorWrapper::from)
        })
        .await?;
        Ok(())
    }

    async fn abort_multipart_upload(&self, key: &str, upload_id: &str) -> StorageResult<()> {
        let abort_upload_req = AbortMultipartUploadRequest {
            bucket: self.bucket.clone(),
            key: key.to_string(),
            upload_id: upload_id.to_string(),
            ..Default::default()
        };
        retry(|| async {
            self.s3_client
                .abort_multipart_upload(abort_upload_req.clone())
                .await
                .map_err(RusotoErrorWrapper::from)
        })
        .await?;
        Ok(())
    }

    fn create_get_object_request(
        &self,
        path: &Path,
        range_opt: Option<Range<usize>>,
    ) -> GetObjectRequest {
        let key = self.key(path);
        let range_str = range_opt.map(|range| format!("bytes={}-{}", range.start, range.end - 1));
        GetObjectRequest {
            bucket: self.bucket.clone(),
            key,
            range: range_str,
            ..Default::default()
        }
    }

    async fn get_to_vec(
        &self,
        path: &Path,
        range_opt: Option<Range<usize>>,
    ) -> StorageResult<Vec<u8>> {
        let get_object_req = self.create_get_object_request(path, range_opt);
        let get_object_output = retry(|| async {
            self.s3_client
                .get_object(get_object_req.clone())
                .await
                .map_err(RusotoErrorWrapper::from)
        })
        .await?;
        let mut body = get_object_output.body.ok_or_else(|| {
            StorageErrorKind::Service.with_error(anyhow::anyhow!("Returned object body was empty."))
        })?;
        let mut buf: Vec<u8> = Vec::new();
        download_all(&mut body, &mut buf).await?;
        Ok(buf)
    }
}

async fn download_all(byte_stream: &mut ByteStream, output: &mut Vec<u8>) -> io::Result<()> {
    output.clear();
    while let Some(chunk_res) = byte_stream.next().await {
        let chunk = chunk_res?;
        output.extend(chunk.as_ref());
    }
    Ok(())
}

#[async_trait]
impl Storage for S3CompatibleObjectStorage {
    async fn put(&self, path: &Path, payload: PutPayload) -> StorageResult<()> {
        let key = self.key(path);
        let len = payload.len().await?;
        let part_num_bytes = self.multipart_policy.part_num_bytes(len);
        if part_num_bytes >= len {
            self.put_single_part(&key, payload, len).await?;
        } else {
            self.put_multi_part(&key, payload, part_num_bytes, len)
                .await?;
        }
        Ok(())
    }

    // TODO implement multipart
    async fn copy_to_file(&self, path: &Path, output_path: &Path) -> StorageResult<()> {
        let get_object_req = self.create_get_object_request(path, None);
        let get_object_output = retry(|| async {
            self.s3_client
                .get_object(get_object_req.clone())
                .await
                .map_err(RusotoErrorWrapper::from)
        })
        .await?;
        let body = get_object_output.body.ok_or_else(|| {
            StorageErrorKind::Service.with_error(anyhow::anyhow!("Returned object body was empty."))
        })?;
        let mut body_read = BufReader::new(body.into_async_read());
        let mut dest_file = File::create(output_path).await?;
        tokio::io::copy_buf(&mut body_read, &mut dest_file).await?;
        dest_file.flush().await?;
        Ok(())
    }

    async fn delete(&self, path: &Path) -> StorageResult<()> {
        let key = self.key(path);
        let delete_object_req = DeleteObjectRequest {
            bucket: self.bucket.clone(),
            key,
            ..Default::default()
        };
        retry(|| async {
            self.s3_client
                .delete_object(delete_object_req.clone())
                .await
                .map_err(RusotoErrorWrapper::from)
        })
        .await?;
        Ok(())
    }

    async fn get_slice(&self, path: &Path, range: Range<usize>) -> StorageResult<Bytes> {
        self.get_to_vec(path, Some(range.clone()))
            .await
            .map(Bytes::from)
            .map_err(|err| {
                err.add_context(format!(
                    "Failed to fetch slice {:?} for object: {}",
                    range,
                    self.uri(path)
                ))
            })
    }

    async fn get_all(&self, path: &Path) -> StorageResult<Bytes> {
        self.get_to_vec(path, None)
            .await
            .map(Bytes::from)
            .map_err(|err| err.add_context(format!("Failed to fetch object: {}", self.uri(path))))
    }

    async fn exists(&self, path: &Path) -> StorageResult<bool> {
        let key = self.key(path);
        let head_object_req = HeadObjectRequest {
            bucket: self.bucket.clone(),
            key,
            ..Default::default()
        };
        let head_object_output = retry(|| async {
            self.s3_client
                .head_object(head_object_req.clone())
                .await
                .map_err(RusotoErrorWrapper::from)
        })
        .await;

        match head_object_output {
            Ok(_) => Ok(true),
            Err(RusotoErrorWrapper(RusotoError::Service(HeadObjectError::NoSuchKey(_)))) => {
                Ok(false)
            }
            // Also catching 404 until this issue is fixed: https://github.com/rusoto/rusoto/issues/716
            Err(RusotoErrorWrapper(RusotoError::Unknown(http_resp))) if http_resp.status == 404 => {
                Ok(false)
            }
            Err(err) => Err(err.into()),
        }
    }

    fn uri(&self) -> String {
        format!("s3://{}/{}", self.bucket, self.prefix.to_string_lossy())
    }
}

#[cfg(test)]
mod tests {
    use crate::object_storage::s3_compatible_storage::split_range_into_chunks;
    use std::path::PathBuf;

    #[test]
    fn test_split_range_into_chunks_inexact() {
        assert_eq!(
            split_range_into_chunks(11, 3),
            vec![0..3, 3..6, 6..9, 9..11]
        );
    }
    #[test]
    fn test_split_range_into_chunks_exact() {
        assert_eq!(split_range_into_chunks(9, 3), vec![0..3, 3..6, 6..9]);
    }

    #[test]
    fn test_split_range_empty() {
        assert_eq!(split_range_into_chunks(0, 1), vec![]);
    }

    use super::parse_split_uri;

    #[test]
    fn test_parse_split_uri() {
        assert_eq!(
            parse_split_uri("s3://bucket/path/to/object"),
            Some(("bucket".to_string(), PathBuf::from("path/to/object")))
        );
    }
}
