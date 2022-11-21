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

use std::sync::Arc;

use async_trait::async_trait;
use futures::TryStreamExt;
use quickwit_proto::{
    convert_to_grpc_result, search_service_server as grpc, set_parent_span_from_request_metadata,
    tonic, LeafSearchStreamRequest, LeafSearchStreamResponse, ServiceError,
};
use quickwit_search::SearchService;
use tracing::instrument;

#[derive(Clone)]
pub struct GrpcSearchAdapter(Arc<dyn SearchService>);

impl From<Arc<dyn SearchService>> for GrpcSearchAdapter {
    fn from(search_service_arc: Arc<dyn SearchService>) -> Self {
        GrpcSearchAdapter(search_service_arc)
    }
}

#[async_trait]
impl grpc::SearchService for GrpcSearchAdapter {
    #[instrument(skip(self, request))]
    async fn root_search(
        &self,
        request: tonic::Request<quickwit_proto::SearchRequest>,
    ) -> Result<tonic::Response<quickwit_proto::SearchResponse>, tonic::Status> {
        set_parent_span_from_request_metadata(request.metadata());
        let search_request = request.into_inner();
        let search_res = self.0.root_search(search_request).await;
        convert_to_grpc_result(search_res)
    }

    #[instrument(skip(self, request))]
    async fn leaf_search(
        &self,
        request: tonic::Request<quickwit_proto::LeafSearchRequest>,
    ) -> Result<tonic::Response<quickwit_proto::LeafSearchResponse>, tonic::Status> {
        set_parent_span_from_request_metadata(request.metadata());
        let leaf_search_request = request.into_inner();
        let leaf_search_res = self.0.leaf_search(leaf_search_request).await;
        convert_to_grpc_result(leaf_search_res)
    }

    #[instrument(skip(self, request))]
    async fn fetch_docs(
        &self,
        request: tonic::Request<quickwit_proto::FetchDocsRequest>,
    ) -> Result<tonic::Response<quickwit_proto::FetchDocsResponse>, tonic::Status> {
        set_parent_span_from_request_metadata(request.metadata());
        let fetch_docs_request = request.into_inner();
        let fetch_docs_res = self.0.fetch_docs(fetch_docs_request).await;
        convert_to_grpc_result(fetch_docs_res)
    }

    type LeafSearchStreamStream = std::pin::Pin<
        Box<
            dyn futures::Stream<Item = Result<LeafSearchStreamResponse, tonic::Status>>
                + Send
                + Sync,
        >,
    >;
    #[instrument(name = "search_adapter:leaf_search_stream", skip(self, request))]
    async fn leaf_search_stream(
        &self,
        request: tonic::Request<LeafSearchStreamRequest>,
    ) -> Result<tonic::Response<Self::LeafSearchStreamStream>, tonic::Status> {
        set_parent_span_from_request_metadata(request.metadata());
        let leaf_search_request = request.into_inner();
        let leaf_search_result = self
            .0
            .leaf_search_stream(leaf_search_request)
            .await
            .map_err(|err| err.grpc_error())?
            .map_err(|err| err.grpc_error());
        Ok(tonic::Response::new(Box::pin(leaf_search_result)))
    }
}
