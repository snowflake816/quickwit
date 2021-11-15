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

use std::fmt::{self, Debug};
use std::io;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use futures::{stream, StreamExt};
use once_cell::sync::OnceCell;
use quickwit_common::{chunk_range, into_u64_range};
use regex::Regex;
use rusoto_core::credential::{AutoRefreshingProvider, ChainProvider};
use rusoto_core::{ByteStream, HttpClient, HttpConfig, Region, RusotoError};
use rusoto_s3::{
    AbortMultipartUploadRequest, CompleteMultipartUploadRequest, CompletedMultipartUpload,
    CompletedPart, CreateMultipartUploadError, CreateMultipartUploadRequest, DeleteObjectRequest,
    GetObjectRequest, HeadObjectError, HeadObjectRequest, PutObjectError, PutObjectRequest,
    S3Client, UploadPartRequest, S3,
};
use tokio::fs::File;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tracing::warn;

use super::error::RusotoErrorWrapper;
use crate::object_storage::MultiPartPolicy;
use crate::retry::{retry, IsRetryable, Retry};
use crate::{OwnedBytes, Storage, StorageError, StorageErrorKind, StorageResult};

/// A credential timeout.
const CREDENTIAL_TIMEOUT: u64 = 5;

/// An timeout for idle sockets being kept-alive.
const POOL_IDLE_TIMEOUT: u64 = 10;

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
    let mut chain_provider = ChainProvider::new();
    chain_provider.set_timeout(Duration::from_secs(CREDENTIAL_TIMEOUT));
    let credentials_provider = AutoRefreshingProvider::new(chain_provider)
        .with_context(|| "Failed to fetch credentials for the object storage.")?;
    let mut http_config: HttpConfig = HttpConfig::default();
    // We experience an issue similar to https://github.com/hyperium/hyper/issues/2312.
    // It seems like the setting below solved it.
    http_config.pool_idle_timeout(std::time::Duration::from_secs(POOL_IDLE_TIMEOUT));
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
        let (bucket, path) = parse_uri(uri).ok_or_else(|| {
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

pub fn parse_uri(uri: &str) -> Option<(String, PathBuf)> {
    static URI_PTN: OnceCell<Regex> = OnceCell::new();
    URI_PTN
        .get_or_init(|| {
            // s3://bucket/path/to/object or s3+localstack://bucket/path/to/object
            Regex::new(r"s3(\+[^:]+)?://(?P<bucket>[^/]+)(/(?P<path>.+))?").unwrap()
        })
        .captures(uri)
        .and_then(|cap| {
            cap.name("bucket").map(|bucket_match| {
                (
                    bucket_match.as_str().to_string(),
                    cap.name("path").map_or_else(
                        || PathBuf::from(""),
                        |path_match| PathBuf::from(path_match.as_str()),
                    ),
                )
            })
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

const MD5_CHUNK_SIZE: usize = 1_000_000;
async fn compute_md5<T: AsyncRead + std::marker::Unpin>(mut read: T) -> io::Result<md5::Digest> {
    let mut checksum = md5::Context::new();
    let mut buf = vec![0; MD5_CHUNK_SIZE];
    loop {
        let read_len = read.read(&mut buf).await?;
        checksum.consume(&buf[..read_len]);
        if read_len == 0 {
            return Ok(checksum.compute());
        }
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

    async fn put_single_part_single_try<'a>(
        &'a self,
        key: &'a str,
        payload: Box<dyn crate::PutPayload>,
        len: u64,
    ) -> Result<(), RusotoErrorWrapper<PutObjectError>> {
        let body = payload.byte_stream().await?;
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

    async fn put_single_part<'a>(
        &'a self,
        key: &'a str,
        payload: Box<dyn crate::PutPayload>,
        len: u64,
    ) -> StorageResult<()> {
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
        payload: Box<dyn crate::PutPayload>,
        len: u64,
        part_len: u64,
    ) -> io::Result<Vec<Part>> {
        assert!(len > 0);
        let multipart_ranges = chunk_range(0..len as usize, part_len as usize)
            .map(into_u64_range)
            .collect::<Vec<_>>();

        let mut parts = Vec::with_capacity(multipart_ranges.len());

        for (multipart_id, multipart_range) in multipart_ranges.into_iter().enumerate() {
            let read = payload
                .range_byte_stream(multipart_range.clone())
                .await?
                .into_async_read();
            let md5 = compute_md5(read).await?;

            let part = Part {
                part_number: multipart_id + 1, // parts are 1-indexed
                range: multipart_range,
                md5,
            };
            parts.push(part);
        }
        Ok(parts)
    }

    async fn upload_part<'a>(
        &'a self,
        upload_id: MultipartUploadId,
        key: &'a str,
        part: Part,
        payload: Box<dyn crate::PutPayload>,
    ) -> Result<CompletedPart, Retry<StorageError>> {
        let byte_stream = payload
            .range_byte_stream(part.range.clone())
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

    async fn put_multi_part<'a>(
        &'a self,
        key: &'a str,
        payload: Box<dyn crate::PutPayload>,
        part_len: u64,
        total_len: u64,
    ) -> StorageResult<()> {
        let upload_id = self
            .create_multipart_upload(key)
            .await
            .map_err(RusotoErrorWrapper::from)?;
        let parts = self
            .create_multipart_requests(payload.clone(), total_len, part_len)
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
        let cap = range_opt.as_ref().map(Range::len).unwrap_or(0);
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
        let mut buf: Vec<u8> = Vec::with_capacity(cap);
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
    async fn put(
        &self,
        path: &Path,
        payload: Box<dyn crate::PutPayload>,
    ) -> crate::StorageResult<()> {
        let key = self.key(path);
        let total_len = payload.len();
        let part_num_bytes = self.multipart_policy.part_num_bytes(total_len);
        if part_num_bytes >= total_len {
            self.put_single_part(&key, payload, total_len).await?;
        } else {
            self.put_multi_part(&key, payload, part_num_bytes, total_len)
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

    async fn get_slice(&self, path: &Path, range: Range<usize>) -> StorageResult<OwnedBytes> {
        self.get_to_vec(path, Some(range.clone()))
            .await
            .map(OwnedBytes::new)
            .map_err(|err| {
                err.add_context(format!(
                    "Failed to fetch slice {:?} for object: {}",
                    range,
                    self.uri(path)
                ))
            })
    }

    async fn get_all(&self, path: &Path) -> StorageResult<OwnedBytes> {
        self.get_to_vec(path, None)
            .await
            .map(OwnedBytes::new)
            .map_err(|err| err.add_context(format!("Failed to fetch object: {}", self.uri(path))))
    }

    async fn file_num_bytes(&self, path: &Path) -> StorageResult<u64> {
        let key = self.key(path);
        let head_object_req = HeadObjectRequest {
            bucket: self.bucket.clone(),
            key,
            ..Default::default()
        };
        let head_object_output_res = retry(|| async {
            self.s3_client
                .head_object(head_object_req.clone())
                .await
                .map_err(RusotoErrorWrapper::from)
        })
        .await;

        match head_object_output_res {
            Ok(head_object_output) => {
                let content_length = head_object_output
                    .content_length
                    .and_then(|num_bytes| {
                        if num_bytes >= 0 {
                            Some(num_bytes as u64)
                        } else {
                            None
                        }
                    })
                    .ok_or_else(|| {
                        StorageErrorKind::Service.with_error(anyhow::anyhow!(
                            "Head output did not contain a valid content length."
                        ))
                    })?;
                Ok(content_length)
            }
            Err(RusotoErrorWrapper(RusotoError::Service(HeadObjectError::NoSuchKey(_)))) => {
                Err(StorageErrorKind::DoesNotExist
                    .with_error(anyhow::anyhow!("Missing key in S3 `{}`", path.display())))
            }
            // Also catching 404 until this issue is fixed: https://github.com/rusoto/rusoto/issues/716
            Err(RusotoErrorWrapper(RusotoError::Unknown(http_resp))) if http_resp.status == 404 => {
                Err(StorageErrorKind::DoesNotExist.with_error(anyhow::anyhow!(
                    "S3 returned a 404 for key `{}`",
                    path.display()
                )))
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
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_md5_calc() -> std::io::Result<()> {
        let data = (0..1_500_000).map(|el| el as u8).collect::<Vec<_>>();
        let md5 = compute_md5(data.as_slice()).await?;
        assert_eq!(md5, md5::compute(data));

        Ok(())
    }

    #[test]
    fn test_split_range_into_chunks_inexact() {
        assert_eq!(
            chunk_range(0..11, 3).collect::<Vec<_>>(),
            vec![0..3, 3..6, 6..9, 9..11]
        );
    }
    #[test]
    fn test_split_range_into_chunks_exact() {
        assert_eq!(
            chunk_range(0..9, 3).collect::<Vec<_>>(),
            vec![0..3, 3..6, 6..9]
        );
    }

    #[test]
    fn test_split_range_empty() {
        assert_eq!(chunk_range(0..0, 1).collect::<Vec<_>>(), vec![]);
    }

    use quickwit_common::chunk_range;

    use super::{compute_md5, parse_uri};

    #[test]
    fn test_parse_uri() {
        assert_eq!(
            parse_uri("s3://bucket/path/to/object"),
            Some(("bucket".to_string(), PathBuf::from("path/to/object")))
        );
        assert_eq!(
            parse_uri("s3://bucket/path"),
            Some(("bucket".to_string(), PathBuf::from("path")))
        );
        assert_eq!(
            parse_uri("s3+localstack://bucket/path/to/object"),
            Some(("bucket".to_string(), PathBuf::from("path/to/object")))
        );
        assert_eq!(
            parse_uri("s3://bucket/"),
            Some(("bucket".to_string(), PathBuf::from("")))
        );
        assert_eq!(
            parse_uri("s3://bucket"),
            Some(("bucket".to_string(), PathBuf::from("")))
        );
        assert_eq!(parse_uri("mem://bucket/path/to"), None);
    }
}
