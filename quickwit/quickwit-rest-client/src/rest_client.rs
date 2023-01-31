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

use std::time::{Duration, Instant};

use bytes::Bytes;
use quickwit_common::FileEntry;
use quickwit_config::SourceConfig;
use quickwit_metastore::{IndexMetadata, Split};
use quickwit_search::SearchResponseRest;
use quickwit_serve::{ListSplitsQueryParams, SearchRequestQueryString};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_TYPE};
use reqwest::{Client, Method, StatusCode, Url};
use serde::Serialize;
use serde_json::json;
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use crate::error::Error;
use crate::models::{ApiResponse, IngestSource};

pub static DEFAULT_ADDRESS: &str = "http://127.0.0.1:7280";
pub static DEFAULT_CONTENT_TYPE: &str = "application/json";
pub static INGEST_CHUNK_SIZE_IN_BYTES: usize = 4 * 1024 * 1024;

pub struct Transport {
    base_url: Url,
    client: Client,
}

impl Default for Transport {
    fn default() -> Self {
        let base_url = Url::parse(DEFAULT_ADDRESS).unwrap();
        Self::new(base_url)
    }
}

impl Transport {
    pub fn new(endpoint: Url) -> Self {
        let base_url = endpoint
            .join("api/v1/")
            .expect("Endpoint shoud not be malformed.");
        Self {
            base_url,
            client: Client::new(),
        }
    }

    pub fn base_url(&self) -> &Url {
        &self.base_url
    }

    /// Creates an asynchronous request that can be awaited
    pub async fn send<Q: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        query_string: Option<&Q>,
        body: Option<Bytes>,
    ) -> Result<ApiResponse, Error> {
        let url = self
            .base_url
            .join(path.trim_start_matches('/'))
            .map_err(|error| Error::UrlParse(error.to_string()))?;
        let mut request_builder = self.client.request(method, url);
        request_builder = request_builder.timeout(Duration::from_secs(10));
        let mut request_headers = HeaderMap::new();
        request_headers.insert(CONTENT_TYPE, HeaderValue::from_static(DEFAULT_CONTENT_TYPE));
        request_builder = request_builder.headers(request_headers);
        if let Some(bytes) = body {
            request_builder = request_builder.body(bytes);
        };
        if let Some(qs) = query_string {
            request_builder = request_builder.query(qs);
        }
        let response = request_builder.send().await?;

        Ok(ApiResponse::new(response))
    }
}

/// Root client for top level APIs.
pub struct QuickwitClient {
    transport: Transport,
}

impl QuickwitClient {
    pub fn new(transport: Transport) -> Self {
        Self { transport }
    }

    pub async fn search(
        &self,
        index_id: &str,
        search_query: SearchRequestQueryString,
    ) -> Result<SearchResponseRest, Error> {
        let path = format!("{index_id}/search");
        let bytes = serde_json::to_string(&search_query)
            .unwrap()
            .as_bytes()
            .to_vec();
        let body = Bytes::from(bytes);
        let response = self
            .transport
            .send::<()>(Method::POST, &path, None, Some(body))
            .await?;
        let search_response = response.deserialize().await?;
        Ok(search_response)
    }

    pub fn indexes(&self) -> IndexClient {
        IndexClient::new(&self.transport)
    }

    pub fn splits<'a, 'b: 'a>(&'a self, index_id: &'b str) -> SplitClient {
        SplitClient::new(&self.transport, index_id)
    }

    pub fn sources<'a, 'b: 'a>(&'a self, index_id: &'b str) -> SourceClient {
        SourceClient::new(&self.transport, index_id)
    }

    pub async fn ingest(&self, index_id: &str, ingest_source: IngestSource) -> Result<(), Error> {
        let ingest_path = format!("{index_id}/ingest");
        let reader: Box<dyn AsyncRead + Send + Sync + Unpin> = match ingest_source {
            IngestSource::File(filepath) => {
                let file = File::open(&filepath).await?;
                Box::new(file)
            }
            IngestSource::Stdin => Box::new(tokio::io::stdin()),
        };
        let mut buf_reader = BufReader::new(reader);

        let start = Instant::now();
        let mut num_bytes_sent = 0;

        loop {
            let mut buffer = Vec::with_capacity(INGEST_CHUNK_SIZE_IN_BYTES);
            for _ in 0..5_000 {
                if buf_reader.read_until(b'\n', &mut buffer).await? == 0 {
                    break;
                }
            }

            if buffer.is_empty() {
                break;
            }
            let body = Bytes::from(buffer);

            loop {
                let response = self
                    .transport
                    .send::<()>(Method::POST, &ingest_path, None, Some(body.clone()))
                    .await?;

                if response.status_code() == StatusCode::TOO_MANY_REQUESTS {
                    println!("Rate limited, retrying in 1 second...");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                } else {
                    response.check().await?;
                    break;
                }
            }
            num_bytes_sent += body.len();

            let throughput =
                num_bytes_sent as f64 / start.elapsed().as_secs_f64() / 1024.0 / 1024.0;
            println!("Indexing throughput: {throughput} MB/s");
        }

        Ok(())
    }
}

/// Client for indexes APIs.
pub struct IndexClient<'a> {
    transport: &'a Transport,
}

impl<'a> IndexClient<'a> {
    pub fn new(transport: &'a Transport) -> Self {
        Self { transport }
    }

    pub async fn create(&self, body: Bytes, overwrite: bool) -> Result<IndexMetadata, Error> {
        let response = self
            .transport
            .send(
                Method::POST,
                "indexes",
                Some(&[("overwrite", overwrite)]),
                Some(body),
            )
            .await?;
        let index_metadata = response.deserialize().await?;
        Ok(index_metadata)
    }

    pub async fn list(&self) -> Result<Vec<IndexMetadata>, Error> {
        let response = self
            .transport
            .send::<()>(Method::GET, "indexes", None, None)
            .await?;
        let indexes_metadatas = response.deserialize().await?;
        Ok(indexes_metadatas)
    }

    pub async fn get(&self, index_id: &str) -> Result<IndexMetadata, Error> {
        let path = format!("indexes/{index_id}");
        let response = self
            .transport
            .send::<()>(Method::GET, &path, None, None)
            .await?;
        let index_metadata = response.deserialize().await?;
        Ok(index_metadata)
    }

    pub async fn clear(&self, index_id: &str) -> Result<(), Error> {
        let path = format!("indexes/{index_id}/clear");
        self.transport
            .send::<()>(Method::PUT, &path, None, None)
            .await?;
        Ok(())
    }

    pub async fn delete(&self, index_id: &str, dry_run: bool) -> Result<Vec<FileEntry>, Error> {
        let path = format!("indexes/{index_id}");
        let response = self
            .transport
            .send(Method::DELETE, &path, Some(&[("dry_run", dry_run)]), None)
            .await?;
        let file_entries = response.deserialize().await?;
        Ok(file_entries)
    }
}

/// Client for splits APIs.
pub struct SplitClient<'a, 'b> {
    transport: &'a Transport,
    index_id: &'b str,
}

impl<'a, 'b> SplitClient<'a, 'b> {
    pub fn new(transport: &'a Transport, index_id: &'b str) -> Self {
        Self {
            transport,
            index_id,
        }
    }

    fn splits_root_url(&self) -> String {
        format!("indexes/{}/splits", self.index_id)
    }

    pub async fn list(
        &self,
        list_splits_query_params: ListSplitsQueryParams,
    ) -> Result<Vec<Split>, Error> {
        let path = self.splits_root_url();
        let response = self
            .transport
            .send(Method::GET, &path, Some(&list_splits_query_params), None)
            .await?;

        let splits = response.deserialize().await?;
        Ok(splits)
    }

    pub async fn mark_for_deletion(&self, split_ids: Vec<String>) -> Result<(), Error> {
        let path = format!("{}/mark-for-deletion", self.splits_root_url());
        let body = Bytes::from(serde_json::to_vec(&json!({ "split_ids": split_ids }))?);
        self.transport
            .send::<()>(Method::PUT, &path, None, Some(body))
            .await?;
        Ok(())
    }
}

/// Client for source APIs.
pub struct SourceClient<'a, 'b> {
    transport: &'a Transport,
    index_id: &'b str,
}

impl<'a, 'b> SourceClient<'a, 'b> {
    pub fn new(transport: &'a Transport, index_id: &'b str) -> Self {
        Self {
            transport,
            index_id,
        }
    }

    fn sources_root_url(&self) -> String {
        format!("indexes/{}/sources", self.index_id)
    }

    pub async fn create(&self, body: Bytes) -> Result<SourceConfig, Error> {
        let response = self
            .transport
            .send::<()>(Method::POST, &self.sources_root_url(), None, Some(body))
            .await?;
        let source_config = response.deserialize().await?;
        Ok(source_config)
    }

    pub async fn get(&self, source_id: &str) -> Result<SourceConfig, Error> {
        let path = format!("{}/{source_id}", self.sources_root_url());
        let response = self
            .transport
            .send::<()>(Method::GET, &path, None, None)
            .await?;
        let source_config = response.deserialize().await?;
        Ok(source_config)
    }

    pub async fn toggle(&self, source_id: &str, enable: bool) -> Result<(), Error> {
        let path = format!("{}/{source_id}", self.sources_root_url());
        self.transport
            .send(Method::PUT, &path, Some(&[("enable", enable)]), None)
            .await?;
        Ok(())
    }

    pub async fn reset_checkpoint(&self, source_id: &str) -> Result<(), Error> {
        let path = format!("{}/{source_id}/reset-checkpoint", self.sources_root_url());
        self.transport
            .send::<()>(Method::PUT, &path, None, None)
            .await?;
        Ok(())
    }

    pub async fn list(&self) -> Result<Vec<SourceConfig>, Error> {
        let response = self
            .transport
            .send::<()>(Method::GET, &self.sources_root_url(), None, None)
            .await?;

        let source_configs = response.deserialize().await?;
        Ok(source_configs)
    }

    pub async fn delete(&self, source_id: &str) -> Result<(), Error> {
        let path = format!("{}/{source_id}", self.sources_root_url());
        self.transport
            .send::<()>(Method::DELETE, &path, None, None)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod test {
    use std::path::PathBuf;
    use std::str::FromStr;

    use bytes::Bytes;
    use quickwit_config::SourceConfig;
    use quickwit_indexing::mock_split;
    use quickwit_metastore::IndexMetadata;
    use quickwit_search::SearchResponseRest;
    use quickwit_serve::{ListSplitsQueryParams, SearchRequestQueryString};
    use reqwest::{StatusCode, Url};
    use serde_json::json;
    use tokio::fs::File;
    use tokio::io::AsyncReadExt;
    use wiremock::matchers::{body_bytes, body_json, method, path, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::{QuickwitClient, Transport};
    use crate::error::Error;
    use crate::models::IngestSource;

    #[test]
    fn test_transport_urls() {
        let transport = Transport::default();
        assert_eq!(
            transport.base_url(),
            &Url::parse("http://127.0.0.1:7280/api/v1/").unwrap()
        )
    }

    #[tokio::test]
    async fn test_client_no_server() {
        let port = quickwit_common::net::find_available_tcp_port().unwrap();
        let server_url = Url::parse(&format!("http://127.0.0.1:{port}")).unwrap();
        let qw_client = QuickwitClient::new(Transport::new(server_url));
        let error = qw_client.indexes().list().await.unwrap_err();

        assert!(matches!(error, Error::Client(_)));
        assert!(error.to_string().contains("tcp connect error"));
    }

    #[tokio::test]
    async fn test_search_endpoint() {
        let mock_server = MockServer::start().await;
        let server_url = Url::parse(&mock_server.uri()).unwrap();
        let qw_client = QuickwitClient::new(Transport::new(server_url));
        // Search
        let search_query_params = SearchRequestQueryString {
            ..Default::default()
        };
        let expected_search_response = SearchResponseRest {
            num_hits: 0,
            hits: Vec::new(),
            snippets: None,
            aggregations: None,
            elapsed_time_micros: 100,
            errors: Vec::new(),
        };
        Mock::given(method("POST"))
            .and(path("/api/v1/my-index/search"))
            .respond_with(ResponseTemplate::new(StatusCode::OK).set_body_json(
                json!({"num_hits": 0, "hits": [], "elapsed_time_micros": 100, "errors": []}),
            ))
            .mount(&mock_server)
            .await;
        assert_eq!(
            qw_client
                .search("my-index", search_query_params)
                .await
                .unwrap(),
            expected_search_response
        );
    }

    fn get_ndjson_filepath(ndjson_dataset_filename: &str) -> String {
        format!(
            "{}/resources/tests/{}",
            env!("CARGO_MANIFEST_DIR"),
            ndjson_dataset_filename
        )
    }

    #[tokio::test]
    async fn test_ingest_endpoint() {
        let mock_server = MockServer::start().await;
        let server_url = Url::parse(&mock_server.uri()).unwrap();
        let qw_client = QuickwitClient::new(Transport::new(server_url));
        let ndjson_filepath = get_ndjson_filepath("documents_to_ingest.json");
        let mut buffer = Vec::new();
        File::open(&ndjson_filepath)
            .await
            .unwrap()
            .read_to_end(&mut buffer)
            .await
            .unwrap();
        Mock::given(method("POST"))
            .and(path("/api/v1/my-index/ingest"))
            .and(body_bytes(buffer.clone()))
            .respond_with(ResponseTemplate::new(StatusCode::TOO_MANY_REQUESTS))
            .up_to_n_times(2)
            .expect(2)
            .mount(&mock_server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/my-index/ingest"))
            .and(body_bytes(buffer))
            .respond_with(ResponseTemplate::new(StatusCode::OK))
            .mount(&mock_server)
            .await;
        let ingest_source = IngestSource::File(PathBuf::from_str(&ndjson_filepath).unwrap());
        qw_client.ingest("my-index", ingest_source).await.unwrap();
    }

    #[tokio::test]
    async fn test_ingest_endpoint_should_return_api_error() {
        let mock_server = MockServer::start().await;
        let server_url = Url::parse(&mock_server.uri()).unwrap();
        let qw_client = QuickwitClient::new(Transport::new(server_url));
        let ndjson_filepath = get_ndjson_filepath("documents_to_ingest.json");
        let mut buffer = Vec::new();
        File::open(&ndjson_filepath)
            .await
            .unwrap()
            .read_to_end(&mut buffer)
            .await
            .unwrap();
        Mock::given(method("POST"))
            .and(path("/api/v1/my-index/ingest"))
            .and(body_bytes(buffer.clone()))
            .respond_with(
                ResponseTemplate::new(500).set_body_json(json!({"error": "internal error"})),
            )
            .mount(&mock_server)
            .await;
        let ingest_source = IngestSource::File(PathBuf::from_str(&ndjson_filepath).unwrap());
        let error = qw_client
            .ingest("my-index", ingest_source)
            .await
            .unwrap_err();
        assert!(matches!(error, Error::Api(_)));
        assert!(error.to_string().contains("internal error"));
    }

    #[tokio::test]
    async fn test_indexes_endpoints() {
        let mock_server = MockServer::start().await;
        let server_url = Url::parse(&mock_server.uri()).unwrap();
        let qw_client = QuickwitClient::new(Transport::new(server_url));
        let index_metadata = IndexMetadata::for_test("test-index", "ram:///indexes/test-index");
        // GET indexes
        Mock::given(method("GET"))
            .and(path("/api/v1/indexes"))
            .respond_with(
                ResponseTemplate::new(StatusCode::OK).set_body_json(vec![index_metadata.clone()]),
            )
            .mount(&mock_server)
            .await;
        assert_eq!(
            qw_client.indexes().list().await.unwrap(),
            vec![index_metadata.clone()]
        );

        // POST create index
        let index_config_to_create = index_metadata.index_config.clone();
        Mock::given(method("POST"))
            .and(path("/api/v1/indexes"))
            .and(body_json(index_config_to_create.clone()))
            .respond_with(
                ResponseTemplate::new(StatusCode::OK).set_body_json(index_metadata.clone()),
            )
            .mount(&mock_server)
            .await;
        let post_body = Bytes::from(serde_json::to_vec(&index_config_to_create).unwrap());
        assert_eq!(
            qw_client.indexes().create(post_body, false).await.unwrap(),
            index_metadata
        );

        // PUT clear index
        Mock::given(method("PUT"))
            .and(path("/api/v1/indexes/my-index/clear"))
            .respond_with(ResponseTemplate::new(StatusCode::OK))
            .mount(&mock_server)
            .await;
        qw_client.indexes().clear("my-index").await.unwrap();

        // DELETE index
        Mock::given(method("DELETE"))
            .and(path("/api/v1/indexes/my-index"))
            .and(query_param("dry_run", "true"))
            .respond_with(
                ResponseTemplate::new(StatusCode::OK)
                    .set_body_json(json!([{"file_name": "filename", "file_size_in_bytes": 100}])),
            )
            .mount(&mock_server)
            .await;
        qw_client.indexes().delete("my-index", true).await.unwrap();
    }

    #[tokio::test]
    async fn test_splits_endpoints() {
        let mock_server = MockServer::start().await;
        let server_url = Url::parse(&mock_server.uri()).unwrap();
        let qw_client = QuickwitClient::new(Transport::new(server_url));
        let split = mock_split("split-1");
        // GET splits
        let list_splits_params = ListSplitsQueryParams {
            start_timestamp: Some(1),
            ..Default::default()
        };
        Mock::given(method("GET"))
            .and(path("/api/v1/indexes/my-index/splits"))
            .and(query_param("start_timestamp", "1"))
            .respond_with(ResponseTemplate::new(StatusCode::OK).set_body_json(vec![split.clone()]))
            .mount(&mock_server)
            .await;
        assert_eq!(
            qw_client
                .splits("my-index")
                .list(list_splits_params)
                .await
                .unwrap(),
            vec![split.clone()]
        );

        Mock::given(method("PUT"))
            .and(path("/api/v1/indexes/my-index/splits/mark-for-deletion"))
            .respond_with(
                ResponseTemplate::new(StatusCode::OK)
                    .set_body_json(json!({"split_ids": ["split-1"]})),
            )
            .mount(&mock_server)
            .await;
        qw_client
            .splits("my-index")
            .mark_for_deletion(vec!["split-1".to_string()])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_sources_endpoints() {
        let mock_server = MockServer::start().await;
        let server_url = Url::parse(&mock_server.uri()).unwrap();
        let qw_client = QuickwitClient::new(Transport::new(server_url));
        let source_config = SourceConfig::ingest_api_default();
        // GET sources
        Mock::given(method("GET"))
            .and(path("/api/v1/indexes/my-index/sources"))
            .respond_with(
                ResponseTemplate::new(StatusCode::OK).set_body_json(vec![source_config.clone()]),
            )
            .mount(&mock_server)
            .await;
        assert_eq!(
            qw_client.sources("my-index").list().await.unwrap(),
            vec![source_config.clone()]
        );

        // Toggle source
        Mock::given(method("PUT"))
            .and(path("/api/v1/indexes/my-index/sources/my-source/toggle"))
            .respond_with(
                ResponseTemplate::new(StatusCode::OK).set_body_json(json!({"enable": true})),
            )
            .mount(&mock_server)
            .await;
        qw_client
            .sources("my-index")
            .toggle("my-source", true)
            .await
            .unwrap();

        // PUT reset checkpoint
        Mock::given(method("PUT"))
            .and(path(
                "/api/v1/indexes/my-index/sources/my-source/reset-checkpoint",
            ))
            .respond_with(ResponseTemplate::new(StatusCode::OK))
            .mount(&mock_server)
            .await;
        qw_client
            .sources("my-index")
            .reset_checkpoint("my-source")
            .await
            .unwrap();

        // DELETE source
        Mock::given(method("DELETE"))
            .and(path("/api/v1/indexes/my-index/sources/my-source"))
            .respond_with(ResponseTemplate::new(StatusCode::OK))
            .mount(&mock_server)
            .await;
        qw_client
            .sources("my-index")
            .delete("my-source")
            .await
            .unwrap();
    }
}
