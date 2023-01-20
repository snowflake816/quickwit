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

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chitchat::transport::ChannelTransport;
use quickwit_cluster::create_cluster_for_test;
use quickwit_common::uri::Uri;
use quickwit_config::service::QuickwitService;
use quickwit_metastore::{IndexMetadata, MockMetastore};
use quickwit_proto::SearchRequest;

use crate::test_utils::ClusterSandbox;
use crate::{check_cluster_configuration, node_readiness_reporting_task};

#[tokio::test]
async fn test_check_cluster_configuration() {
    let services = HashSet::from_iter([QuickwitService::Metastore]);
    let peer_seeds = ["192.168.0.12:7280".to_string()];
    let mut metastore = MockMetastore::new();

    metastore
        .expect_uri()
        .return_const(Uri::for_test("file:///qwdata/indexes"));

    metastore.expect_list_indexes_metadatas().return_once(|| {
        Ok(vec![IndexMetadata::for_test(
            "test-index",
            "file:///qwdata/indexes/test-index",
        )])
    });

    check_cluster_configuration(&services, &peer_seeds, Arc::new(metastore))
        .await
        .unwrap();
}

#[tokio::test]
async fn test_standalone_server_no_indexer() {
    quickwit_common::setup_logging_for_tests();
    let sandbox = ClusterSandbox::start_standalone_node().await.unwrap();

    let mut search_client = sandbox.get_random_search_client();
    search_client
        .root_search(SearchRequest {
            index_id: sandbox.index_id_for_test.clone(),
            query: "*".to_string(),
            search_fields: Vec::new(),
            snippet_fields: Vec::new(),
            start_timestamp: None,
            end_timestamp: None,
            aggregation_request: None,
            max_hits: 10,
            sort_by_field: None,
            sort_order: None,
            start_offset: 0,
        })
        .await
        .unwrap();
    sandbox.rest_client.cluster_snapshot().await.unwrap();

    assert!(sandbox.rest_client.is_ready().await.unwrap());

    // The indexing service is no longer running.
    sandbox
        .rest_client
        .indexing_service_counters()
        .await
        .unwrap_err();
}

#[tokio::test]
async fn test_multi_nodes_cluster() {
    quickwit_common::setup_logging_for_tests();
    let nodes_services = vec![
        HashSet::from_iter([QuickwitService::Searcher]),
        HashSet::from_iter([QuickwitService::Metastore]),
        HashSet::from_iter([QuickwitService::Indexer]),
        HashSet::from_iter([QuickwitService::ControlPlane]),
    ];
    let sandbox = ClusterSandbox::start_cluster_nodes(&nodes_services)
        .await
        .unwrap();
    sandbox.wait_for_cluster_num_ready_nodes(3).await.unwrap();
    let mut search_client = sandbox.get_random_search_client();
    search_client
        .root_search(SearchRequest {
            index_id: sandbox.index_id_for_test.clone(),
            query: "*".to_string(),
            search_fields: Vec::new(),
            start_timestamp: None,
            end_timestamp: None,
            aggregation_request: None,
            max_hits: 10,
            sort_by_field: None,
            sort_order: None,
            start_offset: 0,
            snippet_fields: Vec::new(),
        })
        .await
        .unwrap();

    assert!(sandbox.rest_client.is_live().await.unwrap());

    // Wait until indexing pipelines are started.
    // TODO(fmassot): try to reduce the duration or use a wait until condition.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let indexing_service_counters = sandbox
        .rest_client
        .indexing_service_counters()
        .await
        .unwrap();
    assert_eq!(indexing_service_counters.num_running_pipelines, 1);
}

#[tokio::test]
async fn test_readiness_updates() -> anyhow::Result<()> {
    let transport = ChannelTransport::default();
    let cluster = Arc::new(
        create_cluster_for_test(Vec::new(), &[], &transport, false)
            .await
            .unwrap(),
    );
    cluster.set_self_node_ready(true).await;
    let mut counter = 2;
    let mut metastore = MockMetastore::new();
    metastore
        .expect_check_connectivity()
        .times(4)
        .returning(move || {
            counter -= 1;
            if counter == 1 {
                anyhow::bail!("error")
            }
            Ok(())
        });
    tokio::spawn(node_readiness_reporting_task(
        cluster.clone(),
        Arc::new(metastore),
    ));
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(!cluster.is_self_node_ready().await);
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(cluster.is_self_node_ready().await);
    Ok(())
}
