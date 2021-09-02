/*
 * Copyright (C) 2021 Quickwit Inc.
 *
 * Quickwit is offered under the AGPL v3.0 and as commercial software.
 * For commercial licensing, contact us at hello@quickwit.io.
 *
 * AGPL:
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as
 * published by the Free Software Foundation, either version 3 of the
 * License, or (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU Affero General Public License for more details.
 *
 * You should have received a copy of the GNU Affero General Public License
 * along with this program.  If not, see <http://www.gnu.org/licenses/>.
 */

mod args;
mod error;
mod grpc;
mod grpc_adapter;
mod http_handler;
mod quickwit_cache;
mod rest;

use quickwit_cache::QuickwitCache;
use std::io::Write;
use std::net::SocketAddr;
use std::sync::Arc;

use termcolor::{self, Color, ColorChoice, ColorSpec, StandardStream, WriteColor};
use tracing::debug;

use quickwit_cluster::cluster::{read_host_key, Cluster};
use quickwit_cluster::service::ClusterServiceImpl;
use quickwit_metastore::MetastoreUriResolver;
use quickwit_search::{
    http_addr_to_grpc_addr, http_addr_to_swim_addr, SearchClientPool, SearchServiceImpl,
};
use quickwit_storage::{
    LocalFileStorageFactory, RegionProvider, S3CompatibleObjectStorageFactory, StorageUriResolver,
    StorageWithCacheFactory,
};
use quickwit_telemetry::payload::{ServeEvent, TelemetryEvent};

pub use crate::args::ServeArgs;
pub use crate::error::ApiError;
use crate::grpc::start_grpc_service;
use crate::grpc_adapter::cluster_adapter::GrpcClusterAdapter;
use crate::grpc_adapter::search_adapter::GrpcSearchAdapter;

use crate::rest::start_rest_service;

fn display_help_message(
    rest_socket_addr: SocketAddr,
    example_index_name: &str,
) -> anyhow::Result<()> {
    // No-color if we are not in a terminal.
    let mut stdout = StandardStream::stdout(ColorChoice::Auto);
    write!(&mut stdout, "Server started on ")?;
    stdout.set_color(ColorSpec::new().set_fg(Some(Color::Green)))?;
    writeln!(&mut stdout, "http://{}/", &rest_socket_addr)?;
    stdout.set_color(&ColorSpec::new())?;
    writeln!(
        &mut stdout,
        "\nYou can test it using the following command:"
    )?;
    stdout.set_color(ColorSpec::new().set_fg(Some(Color::Blue)))?;
    writeln!(
        &mut stdout,
        "curl 'http://{}/api/v1/{}/search?query=my+query'",
        rest_socket_addr, example_index_name
    )?;
    stdout.set_color(&ColorSpec::new())?;
    // TODO add link to the documentation of the query language.
    Ok(())
}

/// Builds a storage uri resolver that handles
/// - s3:// uris. This storage comes with a cache that stores hotcache files.
/// - s3+localstack://
/// - file:// uris.
fn storage_uri_resolver() -> StorageUriResolver {
    let s3_storage = StorageWithCacheFactory::new(
        Arc::new(S3CompatibleObjectStorageFactory::default()),
        Arc::new(QuickwitCache::default()),
    );
    StorageUriResolver::builder()
        .register(LocalFileStorageFactory::default())
        .register(s3_storage)
        .register(S3CompatibleObjectStorageFactory::new(
            RegionProvider::Localstack,
            "s3+localstack",
        ))
        .build()
}

/// Start Quickwit search node.
pub async fn serve_cli(args: ServeArgs) -> anyhow::Result<()> {
    debug!(args=?args, "serve-cli");
    quickwit_telemetry::send_telemetry_event(TelemetryEvent::Serve(ServeEvent {
        has_seed: !args.peer_socket_addrs.is_empty(),
    }))
    .await;
    let storage_resolver = storage_uri_resolver();
    let metastore_resolver = MetastoreUriResolver::default();
    let example_index_name = "my_index".to_string();
    let metastore = metastore_resolver.resolve(&args.metastore_uri).await?;

    let host_key = read_host_key(args.host_key_path.as_path())?;
    let swim_addr = http_addr_to_swim_addr(args.rest_socket_addr);
    let cluster = Arc::new(Cluster::new(host_key, swim_addr)?);
    for peer_socket_addr in args
        .peer_socket_addrs
        .iter()
        .filter(|peer_rest_addr| peer_rest_addr != &&args.rest_socket_addr)
    {
        // If the peer address is specified,
        // it joins the cluster in which that node participates.
        let peer_swim_addr = http_addr_to_swim_addr(*peer_socket_addr);
        debug!(peer_swim_addr=?peer_swim_addr, "Add peer node.");
        cluster.add_peer_node(peer_swim_addr).await;
    }

    let client_pool = Arc::new(SearchClientPool::new(cluster.clone()).await?);

    let search_service = Arc::new(SearchServiceImpl::new(
        metastore,
        storage_resolver,
        client_pool,
    ));

    let cluster_service = Arc::new(ClusterServiceImpl::new(cluster.clone()));

    let grpc_socket_addr = http_addr_to_grpc_addr(args.rest_socket_addr);
    let grpc_search_service = GrpcSearchAdapter::from(search_service.clone());
    let grpc_cluster_service = GrpcClusterAdapter::from(cluster_service.clone());
    let grpc_server =
        start_grpc_service(grpc_socket_addr, grpc_search_service, grpc_cluster_service);

    let rest_server = start_rest_service(args.rest_socket_addr, search_service, cluster_service);

    display_help_message(args.rest_socket_addr, &example_index_name)?;

    tokio::try_join!(rest_server, grpc_server)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{array::IntoIter, collections::HashMap, ops::Range, sync::Arc};

    use quickwit_core::mock_split_meta;
    use quickwit_index_config::WikipediaIndexConfig;
    use quickwit_metastore::{checkpoint::Checkpoint, IndexMetadata, MockMetastore, SplitState};
    use quickwit_proto::{search_service_server::SearchServiceServer, OutputFormat};
    use quickwit_search::{
        create_search_service_client, root_search_stream, MockSearchService, SearchError,
        SearchService,
    };
    use tokio::sync::RwLock;
    use tokio_stream::wrappers::UnboundedReceiverStream;
    use tonic::transport::Server;

    use super::*;

    async fn start_test_server(
        address: SocketAddr,
        search_service: Arc<dyn SearchService>,
    ) -> anyhow::Result<()> {
        let search_grpc_adpater = GrpcSearchAdapter::from_mock(search_service);
        let _ = tokio::spawn(async move {
            Server::builder()
                .add_service(SearchServiceServer::new(search_grpc_adpater))
                .serve(address)
                .await?;
            Result::<_, anyhow::Error>::Ok(())
        });
        Ok(())
    }

    #[tokio::test]
    async fn test_serve_search_stream_with_a_leaf_error_on_leaf_node() -> anyhow::Result<()> {
        // This test aims at checking the client grpc implementation.
        let request = quickwit_proto::SearchStreamRequest {
            index_id: "test-idx".to_string(),
            query: "test".to_string(),
            search_fields: vec!["body".to_string()],
            start_timestamp: None,
            end_timestamp: None,
            fast_field: "timestamp".to_string(),
            output_format: OutputFormat::Csv as i32,
            tags: vec![],
        };
        let mut metastore = MockMetastore::new();
        metastore
            .expect_index_metadata()
            .returning(|_index_id: &str| {
                Ok(IndexMetadata {
                    index_id: "test-idx".to_string(),
                    index_uri: "file:///path/to/index/test-idx".to_string(),
                    index_config: Arc::new(WikipediaIndexConfig::new()),
                    checkpoint: Checkpoint::default(),
                })
            });
        metastore.expect_list_splits().returning(
            |_index_id: &str,
             _split_state: SplitState,
             _time_range: Option<Range<i64>>,
             _tags: &[String]| { Ok(vec![mock_split_meta("split1")]) },
        );
        let mut mock_search_service = MockSearchService::new();
        let (result_sender, result_receiver) = tokio::sync::mpsc::unbounded_channel();
        result_sender.send(Ok(quickwit_proto::LeafSearchStreamResult {
            data: b"123".to_vec(),
        }))?;
        result_sender.send(Err(SearchError::InternalError("error".to_string())))?;
        mock_search_service.expect_leaf_search_stream().return_once(
            |_leaf_search_req: quickwit_proto::LeafSearchStreamRequest| {
                Ok(UnboundedReceiverStream::new(result_receiver))
            },
        );
        // The test will hang on indefinitely if we don't drop the sender.
        drop(result_sender);

        let grpc_addr: SocketAddr = format!("127.0.0.1:{}", 10000).parse()?;
        start_test_server(grpc_addr, Arc::new(mock_search_service)).await?;
        let client = create_search_service_client(grpc_addr).await?;
        let clients: HashMap<_, _> = IntoIter::new([(grpc_addr, client)]).collect();
        let client_pool = Arc::new(SearchClientPool {
            clients: Arc::new(RwLock::new(clients)),
        });
        let search_result = root_search_stream(&request, &metastore, &client_pool).await;
        assert!(search_result.is_err());
        assert_eq!(search_result.unwrap_err().to_string(), "Internal error: `[NodeSearchError { search_error: InternalError(\"error\"), split_ids: [\"split1\"] }]`.");
        Ok(())
    }
}
