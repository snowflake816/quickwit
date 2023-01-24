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

use bytes::Bytes;
use futures::{StreamExt, TryStreamExt};
use quickwit_common::uri::Uri;
use quickwit_config::{build_doc_mapper, IndexConfig};
use quickwit_metastore::Metastore;
use quickwit_proto::{LeafSearchStreamRequest, SearchRequest, SearchStreamRequest};
use tokio_stream::StreamMap;
use tracing::*;

use crate::cluster_client::ClusterClient;
use crate::root::SearchJob;
use crate::{list_relevant_splits, SearchError, SearchJobPlacer, SearchServiceClient};

/// Perform a distributed search stream.
#[instrument(skip(metastore, cluster_client, search_job_placer))]
pub async fn root_search_stream(
    search_stream_request: SearchStreamRequest,
    metastore: &dyn Metastore,
    cluster_client: ClusterClient,
    search_job_placer: &SearchJobPlacer,
) -> crate::Result<impl futures::Stream<Item = crate::Result<Bytes>>> {
    // TODO: building a search request should not be necessary for listing splits.
    // This needs some refactoring: relevant splits, metadata_map, jobs...

    let search_request = SearchRequest::from(search_stream_request.clone());
    let index_config: IndexConfig = metastore
        .index_metadata(&search_request.index_id)
        .await?
        .into_index_config();
    let split_metadatas = list_relevant_splits(&search_request, metastore).await?;
    let doc_mapper = build_doc_mapper(&index_config.doc_mapping, &index_config.search_settings)
        .map_err(|err| {
            SearchError::InternalError(format!("Failed to build doc mapper. Cause: {}", err))
        })?;

    // Validates the query by effectively building it against the current schema.
    doc_mapper.query(doc_mapper.schema(), &search_request)?;

    let doc_mapper_str = serde_json::to_string(&doc_mapper).map_err(|err| {
        SearchError::InternalError(format!("Failed to serialize doc mapper: Cause {}", err))
    })?;

    let index_uri: &Uri = &index_config.index_uri;
    let leaf_search_jobs: Vec<SearchJob> = split_metadatas.iter().map(SearchJob::from).collect();

    let assigned_leaf_search_jobs: Vec<(SearchServiceClient, Vec<SearchJob>)> =
        search_job_placer.assign_jobs(leaf_search_jobs, &HashSet::default())?;
    debug!(assigned_leaf_search_jobs=?assigned_leaf_search_jobs, "Assigned leaf search jobs.");

    let mut stream_map: StreamMap<usize, _> = StreamMap::new();
    for (leaf_ord, (client, client_jobs)) in assigned_leaf_search_jobs.into_iter().enumerate() {
        let leaf_request: LeafSearchStreamRequest = jobs_to_leaf_request(
            &search_stream_request,
            &doc_mapper_str,
            index_uri.as_ref(),
            client_jobs,
        );
        let leaf_stream = cluster_client
            .leaf_search_stream(leaf_request, client)
            .await;
        stream_map.insert(leaf_ord, leaf_stream);
    }
    Ok(stream_map
        .map(|(_leaf_ord, result)| result)
        .map_ok(|leaf_response| Bytes::from(leaf_response.data)))
}

fn jobs_to_leaf_request(
    request: &SearchStreamRequest,
    doc_mapper_str: &str,
    index_uri: &str, // TODO make Uri
    jobs: Vec<SearchJob>,
) -> LeafSearchStreamRequest {
    LeafSearchStreamRequest {
        request: Some(request.clone()),
        split_offsets: jobs.into_iter().map(Into::into).collect(),
        doc_mapper: doc_mapper_str.to_string(),
        index_uri: index_uri.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use quickwit_grpc_clients::service_client_pool::ServiceClientPool;
    use quickwit_indexing::mock_split;
    use quickwit_metastore::{IndexMetadata, MockMetastore};
    use quickwit_proto::OutputFormat;
    use tokio_stream::wrappers::UnboundedReceiverStream;

    use super::*;
    use crate::MockSearchService;

    #[tokio::test]
    async fn test_root_search_stream_single_split() -> anyhow::Result<()> {
        let request = quickwit_proto::SearchStreamRequest {
            index_id: "test-index".to_string(),
            query: "test".to_string(),
            search_fields: vec!["body".to_string()],
            snippet_fields: vec![],
            start_timestamp: None,
            end_timestamp: None,
            fast_field: "timestamp".to_string(),
            output_format: OutputFormat::Csv as i32,
            partition_by_field: None,
        };
        let mut metastore = MockMetastore::new();
        metastore
            .expect_index_metadata()
            .returning(|_index_id: &str| {
                Ok(IndexMetadata::for_test(
                    "test-index",
                    "ram:///indexes/test-index",
                ))
            });
        metastore
            .expect_list_splits()
            .returning(|_filter| Ok(vec![mock_split("split1")]));
        let mut mock_search_service = MockSearchService::new();
        let (result_sender, result_receiver) = tokio::sync::mpsc::unbounded_channel();
        result_sender.send(Ok(quickwit_proto::LeafSearchStreamResponse {
            data: b"123".to_vec(),
            split_id: "split_1".to_string(),
        }))?;
        result_sender.send(Ok(quickwit_proto::LeafSearchStreamResponse {
            data: b"456".to_vec(),
            split_id: "split_1".to_string(),
        }))?;
        mock_search_service.expect_leaf_search_stream().return_once(
            |_leaf_search_req: quickwit_proto::LeafSearchStreamRequest| {
                Ok(UnboundedReceiverStream::new(result_receiver))
            },
        );
        // The test will hang on indefinitely if we don't drop the receiver.
        drop(result_sender);
        let client_pool =
            ServiceClientPool::for_clients_list(vec![SearchServiceClient::from_service(
                Arc::new(mock_search_service),
                ([127, 0, 0, 1], 1000).into(),
            )]);
        let search_job_placer = SearchJobPlacer::new(client_pool);

        let cluster_client = ClusterClient::new(search_job_placer.clone());
        let result: Vec<Bytes> =
            root_search_stream(request, &metastore, cluster_client, &search_job_placer)
                .await?
                .try_collect()
                .await?;
        assert_eq!(result.len(), 2);
        assert_eq!(&result[0], &b"123"[..]);
        assert_eq!(&result[1], &b"456"[..]);
        Ok(())
    }

    #[tokio::test]
    async fn test_root_search_stream_single_split_partitionned() -> anyhow::Result<()> {
        let request = quickwit_proto::SearchStreamRequest {
            index_id: "test-index".to_string(),
            query: "test".to_string(),
            search_fields: vec!["body".to_string()],
            snippet_fields: vec![],
            start_timestamp: None,
            end_timestamp: None,
            fast_field: "timestamp".to_string(),
            output_format: OutputFormat::Csv as i32,
            partition_by_field: Some("timestamp".to_string()),
        };
        let mut metastore = MockMetastore::new();
        metastore
            .expect_index_metadata()
            .returning(|_index_id: &str| {
                Ok(IndexMetadata::for_test(
                    "test-index",
                    "ram:///indexes/test-index",
                ))
            });
        metastore
            .expect_list_splits()
            .returning(|_filter| Ok(vec![mock_split("split1")]));
        let mut mock_search_service = MockSearchService::new();
        let (result_sender, result_receiver) = tokio::sync::mpsc::unbounded_channel();
        result_sender.send(Ok(quickwit_proto::LeafSearchStreamResponse {
            data: b"123".to_vec(),
            split_id: "1".to_string(),
        }))?;
        result_sender.send(Ok(quickwit_proto::LeafSearchStreamResponse {
            data: b"456".to_vec(),
            split_id: "2".to_string(),
        }))?;
        mock_search_service.expect_leaf_search_stream().return_once(
            |_leaf_search_req: quickwit_proto::LeafSearchStreamRequest| {
                Ok(UnboundedReceiverStream::new(result_receiver))
            },
        );
        // The test will hang on indefinitely if we don't drop the sender.
        drop(result_sender);
        let client_pool =
            ServiceClientPool::for_clients_list(vec![SearchServiceClient::from_service(
                Arc::new(mock_search_service),
                ([127, 0, 0, 1], 1000).into(),
            )]);
        let search_job_placer = SearchJobPlacer::new(client_pool);
        let cluster_client = ClusterClient::new(search_job_placer.clone());
        let stream =
            root_search_stream(request, &metastore, cluster_client, &search_job_placer).await?;
        let result: Vec<_> = stream.try_collect().await?;
        assert_eq!(result.len(), 2);
        assert_eq!(&result[0], &b"123"[..]);
        assert_eq!(&result[1], &b"456"[..]);
        Ok(())
    }

    #[tokio::test]
    async fn test_root_search_stream_single_split_with_error() -> anyhow::Result<()> {
        let request = quickwit_proto::SearchStreamRequest {
            index_id: "test-index".to_string(),
            query: "test".to_string(),
            search_fields: vec!["body".to_string()],
            snippet_fields: vec![],
            start_timestamp: None,
            end_timestamp: None,
            fast_field: "timestamp".to_string(),
            output_format: OutputFormat::Csv as i32,
            partition_by_field: None,
        };
        let mut metastore = MockMetastore::new();
        metastore
            .expect_index_metadata()
            .returning(|_index_id: &str| {
                Ok(IndexMetadata::for_test(
                    "test-index",
                    "ram:///indexes/test-index",
                ))
            });
        metastore
            .expect_list_splits()
            .returning(|_filter| Ok(vec![mock_split("split1"), mock_split("split2")]));
        let mut mock_search_service = MockSearchService::new();
        let (result_sender, result_receiver) = tokio::sync::mpsc::unbounded_channel();
        result_sender.send(Ok(quickwit_proto::LeafSearchStreamResponse {
            data: b"123".to_vec(),
            split_id: "split1".to_string(),
        }))?;
        result_sender.send(Err(SearchError::InternalError("error".to_string())))?;
        mock_search_service
            .expect_leaf_search_stream()
            .withf(|request| request.split_offsets.len() == 2) // First request.
            .return_once(
                |_leaf_search_req: quickwit_proto::LeafSearchStreamRequest| {
                    Ok(UnboundedReceiverStream::new(result_receiver))
                },
            );
        mock_search_service
            .expect_leaf_search_stream()
            .withf(|request| request.split_offsets.len() == 1) // Retry request on the failed split.
            .return_once(
                |_leaf_search_req: quickwit_proto::LeafSearchStreamRequest| {
                    Err(SearchError::InternalError("error".to_string()))
                },
            );
        // The test will hang on indefinitely if we don't drop the sender.
        drop(result_sender);
        let client_pool =
            ServiceClientPool::for_clients_list(vec![SearchServiceClient::from_service(
                Arc::new(mock_search_service),
                ([127, 0, 0, 1], 1000).into(),
            )]);
        let search_job_placer = SearchJobPlacer::new(client_pool);
        let cluster_client = ClusterClient::new(search_job_placer.clone());
        let stream =
            root_search_stream(request, &metastore, cluster_client, &search_job_placer).await?;
        let result: Result<Vec<_>, SearchError> = stream.try_collect().await;
        assert_eq!(result.is_err(), true);
        assert_eq!(result.unwrap_err().to_string(), "Internal error: `error`.");
        Ok(())
    }

    #[tokio::test]
    async fn test_root_search_stream_with_invalid_query() -> anyhow::Result<()> {
        let mut metastore = MockMetastore::new();
        metastore
            .expect_index_metadata()
            .returning(|_index_id: &str| {
                Ok(IndexMetadata::for_test(
                    "test-index",
                    "ram:///indexes/test-index",
                ))
            });
        metastore
            .expect_list_splits()
            .returning(|_filter| Ok(vec![mock_split("split")]));

        let client_pool =
            ServiceClientPool::for_clients_list(vec![SearchServiceClient::from_service(
                Arc::new(MockSearchService::new()),
                ([127, 0, 0, 1], 1000).into(),
            )]);
        let search_job_placer = SearchJobPlacer::new(client_pool);

        assert!(root_search_stream(
            quickwit_proto::SearchStreamRequest {
                index_id: "test-index".to_string(),
                query: r#"invalid_field:"test""#.to_string(),
                search_fields: vec!["body".to_string()],
                snippet_fields: vec![],
                start_timestamp: None,
                end_timestamp: None,
                fast_field: "timestamp".to_string(),
                output_format: OutputFormat::Csv as i32,
                partition_by_field: Some("timestamp".to_string()),
            },
            &metastore,
            ClusterClient::new(search_job_placer.clone()),
            &search_job_placer,
        )
        .await
        .is_err());

        assert!(root_search_stream(
            quickwit_proto::SearchStreamRequest {
                index_id: "test-index".to_string(),
                query: "test".to_string(),
                search_fields: vec!["invalid_field".to_string()],
                snippet_fields: vec![],
                start_timestamp: None,
                end_timestamp: None,
                fast_field: "timestamp".to_string(),
                output_format: OutputFormat::Csv as i32,
                partition_by_field: Some("timestamp".to_string()),
            },
            &metastore,
            ClusterClient::new(search_job_placer.clone()),
            &search_job_placer
        )
        .await
        .is_err());

        Ok(())
    }
}
