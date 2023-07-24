// Copyright (C) 2023 Quickwit, Inc.
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
use std::sync::Arc;

use quickwit_common::tower::BoxFutureInfaillible;
use quickwit_config::service::QuickwitService;
use quickwit_ingest::ingest_service_grpc_server::IngestServiceGrpcServer;
use quickwit_ingest::IngestServiceGrpcServerAdapter;
use quickwit_jaeger::JaegerService;
use quickwit_metastore::GrpcMetastoreAdapter;
use quickwit_opentelemetry::otlp::{OtlpGrpcLogsService, OtlpGrpcTracesService};
use quickwit_proto::control_plane::control_plane_service_grpc_server::ControlPlaneServiceGrpcServer;
use quickwit_proto::control_plane::ControlPlaneServiceGrpcServerAdapter;
use quickwit_proto::indexing::indexing_service_grpc_server::IndexingServiceGrpcServer;
use quickwit_proto::indexing::{IndexingServiceClient, IndexingServiceGrpcServerAdapter};
use quickwit_proto::jaeger::storage::v1::span_reader_plugin_server::SpanReaderPluginServer;
use quickwit_proto::metastore::metastore_service_server::MetastoreServiceServer;
use quickwit_proto::opentelemetry::proto::collector::logs::v1::logs_service_server::LogsServiceServer;
use quickwit_proto::opentelemetry::proto::collector::trace::v1::trace_service_server::TraceServiceServer;
use quickwit_proto::search_service_server::SearchServiceServer;
use quickwit_proto::tonic::codegen::CompressionEncoding;
use quickwit_proto::tonic::transport::Server;
use tracing::*;

use crate::search_api::GrpcSearchAdapter;
use crate::QuickwitServices;

/// Starts gRPC services given a gRPC address.
pub(crate) async fn start_grpc_server(
    grpc_listen_addr: SocketAddr,
    services: Arc<QuickwitServices>,
    readiness_trigger: BoxFutureInfaillible<()>,
    shutdown_signal: BoxFutureInfaillible<()>,
) -> anyhow::Result<()> {
    let mut enabled_grpc_services = BTreeSet::new();
    let mut server = Server::builder();

    // Mount gRPC metastore service if `QuickwitService::Metastore` is enabled on node.
    let metastore_grpc_service = if services.services.contains(&QuickwitService::Metastore) {
        enabled_grpc_services.insert("metastore");
        let metastore = services.metastore.clone();
        let grpc_metastore = GrpcMetastoreAdapter::from(metastore);
        Some(MetastoreServiceServer::new(grpc_metastore))
    } else {
        None
    };
    // Mount gRPC indexing service if `QuickwitService::Indexer` is enabled on node.
    let indexing_grpc_service = if services.services.contains(&QuickwitService::Indexer) {
        if let Some(indexing_service) = services.indexing_service.clone() {
            enabled_grpc_services.insert("indexing");
            let indexing_service = IndexingServiceClient::from_mailbox(indexing_service);
            let indexing_service_adapter = IndexingServiceGrpcServerAdapter::new(indexing_service);
            Some(IndexingServiceGrpcServer::new(indexing_service_adapter))
        } else {
            None
        }
    } else {
        None
    };
    // Mount gRPC ingest service if `QuickwitService::Indexer` is enabled on node.
    let ingest_api_grpc_service = if services.services.contains(&QuickwitService::Indexer) {
        enabled_grpc_services.insert("ingest_api");
        let ingest_service_adapter =
            IngestServiceGrpcServerAdapter::new(services.ingest_service.clone());
        Some(IngestServiceGrpcServer::new(ingest_service_adapter))
    } else {
        None
    };
    // Mount gRPC control plane service if `QuickwitService::ControlPlane` is enabled on node.
    let control_plane_grpc_service = if services.services.contains(&QuickwitService::ControlPlane) {
        if let Some(control_plane_client) = &services.control_plane_service {
            enabled_grpc_services.insert("control-plane");
            let adapter = ControlPlaneServiceGrpcServerAdapter::new(control_plane_client.clone());
            Some(ControlPlaneServiceGrpcServer::new(adapter))
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
        let ingest_service = services.ingest_service.clone();
        let commit_type_opt = None;
        let trace_service =
            TraceServiceServer::new(OtlpGrpcTracesService::new(ingest_service, commit_type_opt))
                .accept_compressed(CompressionEncoding::Gzip);
        Some(trace_service)
    } else {
        None
    };
    let otlp_log_grpc_service = if enable_opentelemetry_otlp_grpc_service
        && services.services.contains(&QuickwitService::Indexer)
    {
        enabled_grpc_services.insert("otlp-logs");
        let ingest_service = services.ingest_service.clone();
        let logs_service = LogsServiceServer::new(OtlpGrpcLogsService::new(ingest_service))
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
        .add_optional_service(ingest_api_grpc_service)
        .add_optional_service(otlp_log_grpc_service)
        .add_optional_service(otlp_trace_service)
        .add_optional_service(search_grpc_service)
        .add_optional_service(jaeger_grpc_service);

    info!(
        enabled_grpc_services=?enabled_grpc_services,
        grpc_listen_addr=?grpc_listen_addr,
        "Starting gRPC server listening on {grpc_listen_addr}."
    );
    let serve_fut = server_router.serve_with_shutdown(grpc_listen_addr, shutdown_signal);
    let (serve_res, _trigger_res) = tokio::join!(serve_fut, readiness_trigger);
    serve_res?;
    Ok(())
}
