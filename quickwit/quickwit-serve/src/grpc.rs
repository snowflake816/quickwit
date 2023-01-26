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

use std::collections::BTreeSet;
use std::net::SocketAddr;

use anyhow::Context;
use quickwit_config::service::QuickwitService;
use quickwit_control_plane::grpc_adapter::GrpcControlPlaneAdapter;
use quickwit_indexing::grpc_adapter::GrpcIndexingAdapter;
use quickwit_jaeger::JaegerService;
use quickwit_metastore::GrpcMetastoreAdapter;
use quickwit_opentelemetry::otlp::{OtlpGrpcLogsService, OtlpGrpcTraceService};
use quickwit_proto::control_plane_api::control_plane_service_server::ControlPlaneServiceServer;
use quickwit_proto::indexing_api::indexing_service_server::IndexingServiceServer;
use quickwit_proto::jaeger::storage::v1::span_reader_plugin_server::SpanReaderPluginServer;
use quickwit_proto::metastore_api::metastore_api_service_server::MetastoreApiServiceServer;
use quickwit_proto::opentelemetry::proto::collector::logs::v1::logs_service_server::LogsServiceServer;
use quickwit_proto::opentelemetry::proto::collector::trace::v1::trace_service_server::TraceServiceServer;
use quickwit_proto::search_service_server::SearchServiceServer;
use quickwit_proto::tonic;
use quickwit_proto::tonic::codegen::CompressionEncoding;
use tonic::transport::Server;
use tracing::*;

use crate::search_api::GrpcSearchAdapter;
use crate::QuickwitServices;

/// Starts gRPC services given a gRPC address.
pub(crate) async fn start_grpc_server(
    grpc_listen_addr: SocketAddr,
    services: &QuickwitServices,
) -> anyhow::Result<()> {
    let mut enabled_grpc_services = BTreeSet::new();
    let mut server = Server::builder();

    // Mount gRPC metastore service if `QuickwitService::Metastore` is enabled on node.
    let metastore_grpc_service = if services.services.contains(&QuickwitService::Metastore) {
        enabled_grpc_services.insert("metastore");
        let metastore = services.metastore.clone();
        let grpc_metastore = GrpcMetastoreAdapter::from(metastore);
        Some(MetastoreApiServiceServer::new(grpc_metastore))
    } else {
        None
    };
    // Mount gRPC indexing service if `QuickwitService::Indexer` is enabled on node.
    let indexing_grpc_service = if services.services.contains(&QuickwitService::Indexer) {
        if let Some(indexing_service) = services.indexing_service.as_ref() {
            enabled_grpc_services.insert("indexing");
            let grpc_indexing = GrpcIndexingAdapter::from(indexing_service.clone());
            Some(IndexingServiceServer::new(grpc_indexing))
        } else {
            None
        }
    } else {
        None
    };
    // Mount gRPC control plane service if `QuickwitService::ControlPlane` is enabled on node.
    let control_plane_grpc_service = if services.services.contains(&QuickwitService::ControlPlane) {
        if let Some(indexing_scheduler_service) = services.indexing_scheduler_service.as_ref() {
            enabled_grpc_services.insert("control_plane");
            let grpc_indexing = GrpcControlPlaneAdapter::from(indexing_scheduler_service.clone());
            Some(ControlPlaneServiceServer::new(grpc_indexing))
        } else {
            None
        }
    } else {
        None
    };
    // Mount gRPC OpenTelemetry OTLP trace service if `QuickwitService::Indexer` is enabled on node.
    let enable_opentelemetry_otlp_grpc_service =
        services.config.indexer_config.enable_otlp_endpoint;
    let otlp_trace_service = if enable_opentelemetry_otlp_grpc_service
        && services.services.contains(&QuickwitService::Indexer)
    {
        enabled_grpc_services.insert("otlp-trace");
        let ingest_api_service = services
            .ingest_api_service
            .clone()
            .context("Failed to instantiate OTLP trace service: the ingest API is disabled.")?;
        let trace_service = TraceServiceServer::new(OtlpGrpcTraceService::new(ingest_api_service))
            .accept_compressed(CompressionEncoding::Gzip);
        Some(trace_service)
    } else {
        None
    };
    let otlp_log_grpc_service = if enable_opentelemetry_otlp_grpc_service
        && services.services.contains(&QuickwitService::Indexer)
    {
        enabled_grpc_services.insert("otlp-log");
        let ingest_api_service = services
            .ingest_api_service
            .clone()
            .context("Failed to instantiate OTLP log service: the ingest API is disabled.")?;
        let logs_service = LogsServiceServer::new(OtlpGrpcLogsService::new(ingest_api_service))
            .accept_compressed(CompressionEncoding::Gzip);
        Some(logs_service)
    } else {
        None
    };
    // Mount gRPC search service if `QuickwitService::Searcher` is enabled on node.
    let search_grpc_service = if services.services.contains(&QuickwitService::Searcher) {
        enabled_grpc_services.insert("search");
        let search_service = services.search_service.clone();
        let grpc_search_service = GrpcSearchAdapter::from(search_service);
        Some(SearchServiceServer::new(grpc_search_service))
    } else {
        None
    };
    let enable_jaeger_endpoint = services.config.jaeger_config.enable_endpoint;
    let jaeger_grpc_service =
        if enable_jaeger_endpoint && services.services.contains(&QuickwitService::Searcher) {
            enabled_grpc_services.insert("jaeger");
            let search_service = services.search_service.clone();
            Some(SpanReaderPluginServer::new(JaegerService::new(
                services.config.jaeger_config.clone(),
                search_service,
            )))
        } else {
            None
        };
    let server_router = server
        .add_optional_service(metastore_grpc_service)
        .add_optional_service(control_plane_grpc_service)
        .add_optional_service(indexing_grpc_service)
        .add_optional_service(otlp_log_grpc_service)
        .add_optional_service(otlp_trace_service)
        .add_optional_service(search_grpc_service)
        .add_optional_service(jaeger_grpc_service);

    info!(enabled_grpc_services=?enabled_grpc_services, grpc_listen_addr=?grpc_listen_addr, "Starting gRPC server.");
    server_router.serve(grpc_listen_addr).await?;
    Ok(())
}
