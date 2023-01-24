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

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use quickwit_common::uri::Uri;
use quickwit_config::SearcherConfig;
use quickwit_doc_mapper::DocMapper;
use quickwit_metastore::Metastore;
use quickwit_proto::{
    FetchDocsRequest, FetchDocsResponse, LeafSearchRequest, LeafSearchResponse,
    LeafSearchStreamRequest, LeafSearchStreamResponse, SearchRequest, SearchResponse,
    SearchStreamRequest,
};
use quickwit_storage::{Cache, MemorySizedCache, QuickwitCache, StorageUriResolver};
use tokio::sync::Semaphore;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::info;

use crate::search_stream::{leaf_search_stream, root_search_stream};
use crate::{fetch_docs, leaf_search, root_search, ClusterClient, SearchError, SearchJobPlacer};

#[derive(Clone)]
/// The search service implementation.
pub struct SearchServiceImpl {
    metastore: Arc<dyn Metastore>,
    storage_uri_resolver: StorageUriResolver,
    cluster_client: ClusterClient,
    search_job_placer: SearchJobPlacer,
    searcher_context: Arc<SearcherContext>,
}

/// Trait representing a search service.
///
/// It mirrors the gRPC service `SearchService`, but with a more concrete
/// error type that can be converted into an API Error.
/// The REST API relies directly on the `SearchService`.
/// Also, it is mockable.
#[mockall::automock]
#[async_trait]
pub trait SearchService: 'static + Send + Sync {
    /// Root search API.
    /// This RPC identifies the set of splits on which the query should run on,
    /// and dispatches the multiple calls to `LeafSearch`.
    ///
    /// It is also in charge of merging back the responses.
    async fn root_search(&self, request: SearchRequest) -> crate::Result<SearchResponse>;

    /// Performs a leaf search on a given set of splits.
    ///
    /// It is like a regular search except that:
    /// - the node should perform the search locally instead of dispatching
    /// it to other nodes.
    /// - it should be applied on the given subset of splits
    /// - hit content is not fetched, and we instead return a so-called `PartialHit`.
    async fn leaf_search(&self, request: LeafSearchRequest) -> crate::Result<LeafSearchResponse>;

    /// Fetches the documents contents from the document store.
    /// This methods takes `PartialHit`s and returns `Hit`s.
    async fn fetch_docs(&self, request: FetchDocsRequest) -> crate::Result<FetchDocsResponse>;

    /// Performs a root search returning a receiver for streaming
    async fn root_search_stream(
        &self,
        request: SearchStreamRequest,
    ) -> crate::Result<Pin<Box<dyn futures::Stream<Item = crate::Result<Bytes>> + Send>>>;

    /// Performs a leaf search on a given set of splits and returns a stream.
    async fn leaf_search_stream(
        &self,
        request: LeafSearchStreamRequest,
    ) -> crate::Result<UnboundedReceiverStream<crate::Result<LeafSearchStreamResponse>>>;
}

impl SearchServiceImpl {
    /// Creates a new search service.
    pub fn new(
        metastore: Arc<dyn Metastore>,
        storage_uri_resolver: StorageUriResolver,
        cluster_client: ClusterClient,
        search_job_placer: SearchJobPlacer,
        searcher_config: SearcherConfig,
    ) -> Self {
        let searcher_context = Arc::new(SearcherContext::new(searcher_config));
        SearchServiceImpl {
            metastore,
            storage_uri_resolver,
            cluster_client,
            search_job_placer,
            searcher_context,
        }
    }
}

fn deserialize_doc_mapper(doc_mapper_str: &str) -> crate::Result<Arc<dyn DocMapper>> {
    let doc_mapper = serde_json::from_str::<Arc<dyn DocMapper>>(doc_mapper_str).map_err(|err| {
        SearchError::InternalError(format!("Failed to deserialize doc mapper: `{err}`"))
    })?;
    Ok(doc_mapper)
}

#[async_trait]
impl SearchService for SearchServiceImpl {
    async fn root_search(&self, search_request: SearchRequest) -> crate::Result<SearchResponse> {
        let search_result = root_search(
            &search_request,
            self.metastore.as_ref(),
            &self.cluster_client,
            &self.search_job_placer,
        )
        .await?;

        Ok(search_result)
    }

    async fn leaf_search(
        &self,
        leaf_search_request: LeafSearchRequest,
    ) -> crate::Result<LeafSearchResponse> {
        let search_request = leaf_search_request
            .search_request
            .ok_or_else(|| SearchError::InternalError("No search request.".to_string()))?;
        info!(index=?search_request.index_id, splits=?leaf_search_request.split_offsets, "leaf_search");
        let storage = self
            .storage_uri_resolver
            .resolve(&Uri::from_well_formed(leaf_search_request.index_uri))?;
        let split_ids = leaf_search_request.split_offsets;
        let doc_mapper = deserialize_doc_mapper(&leaf_search_request.doc_mapper)?;

        let leaf_search_response = leaf_search(
            self.searcher_context.clone(),
            &search_request,
            storage.clone(),
            &split_ids[..],
            doc_mapper,
        )
        .await?;

        Ok(leaf_search_response)
    }

    async fn fetch_docs(
        &self,
        fetch_docs_request: FetchDocsRequest,
    ) -> crate::Result<FetchDocsResponse> {
        let storage = self
            .storage_uri_resolver
            .resolve(&Uri::from_well_formed(fetch_docs_request.index_uri))?;
        let search_request_opt = fetch_docs_request.search_request.as_ref();
        let doc_mapper = deserialize_doc_mapper(&fetch_docs_request.doc_mapper)?;
        let fetch_docs_response = fetch_docs(
            self.searcher_context.clone(),
            fetch_docs_request.partial_hits,
            storage,
            &fetch_docs_request.split_offsets,
            doc_mapper,
            search_request_opt,
        )
        .await?;

        Ok(fetch_docs_response)
    }

    async fn root_search_stream(
        &self,
        stream_request: SearchStreamRequest,
    ) -> crate::Result<Pin<Box<dyn futures::Stream<Item = crate::Result<Bytes>> + Send>>> {
        let data = root_search_stream(
            stream_request,
            self.metastore.as_ref(),
            self.cluster_client.clone(),
            &self.search_job_placer,
        )
        .await?;
        Ok(Box::pin(data))
    }

    async fn leaf_search_stream(
        &self,
        leaf_stream_request: LeafSearchStreamRequest,
    ) -> crate::Result<UnboundedReceiverStream<crate::Result<LeafSearchStreamResponse>>> {
        let stream_request = leaf_stream_request
            .request
            .ok_or_else(|| SearchError::InternalError("No search request.".to_string()))?;
        info!(index=?stream_request.index_id, splits=?leaf_stream_request.split_offsets, "leaf_search");
        let storage = self
            .storage_uri_resolver
            .resolve(&Uri::from_well_formed(leaf_stream_request.index_uri))?;
        let doc_mapper = deserialize_doc_mapper(&leaf_stream_request.doc_mapper)?;
        let leaf_receiver = leaf_search_stream(
            self.searcher_context.clone(),
            stream_request,
            storage,
            leaf_stream_request.split_offsets,
            doc_mapper,
        )
        .await;
        Ok(leaf_receiver)
    }
}

/// [`SearcherContext`] provides a common set of variables
/// shared by a searcher instance (which instanciates a
/// [`SearchServiceImpl`]).
pub struct SearcherContext {
    /// Searcher config.
    pub searcher_config: SearcherConfig,
    /// Counting semaphore to limit concurrent leaf search split requests.
    pub leaf_search_split_semaphore: Semaphore,
    /// Counting semaphore to limit concurrent split stream requests.
    pub split_stream_semaphore: Semaphore,
    /// Split footer cache.
    pub split_footer_cache: MemorySizedCache<String>,
    /// Fast fields cache.
    pub fast_fields_cache: Arc<dyn Cache>,
}

impl SearcherContext {
    pub fn new(searcher_config: SearcherConfig) -> Self {
        let capacity_in_bytes = searcher_config.split_footer_cache_capacity.get_bytes() as usize;
        let global_split_footer_cache = MemorySizedCache::with_capacity_in_bytes(
            capacity_in_bytes,
            &quickwit_storage::STORAGE_METRICS.split_footer_cache,
        );
        let leaf_search_split_semaphore =
            Semaphore::new(searcher_config.max_num_concurrent_split_searches);
        let split_stream_semaphore =
            Semaphore::new(searcher_config.max_num_concurrent_split_streams);
        let fast_field_cache_capacity =
            searcher_config.fast_field_cache_capacity.get_bytes() as usize;
        let storage_long_term_cache = Arc::new(QuickwitCache::new(fast_field_cache_capacity));
        Self {
            searcher_config,
            split_footer_cache: global_split_footer_cache,
            leaf_search_split_semaphore,
            split_stream_semaphore,
            fast_fields_cache: storage_long_term_cache,
        }
    }
}
