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

use async_trait::async_trait;
use itertools::Itertools;
use quickwit_actors::{Actor, ActorContext, ActorExitStatus, Handler, Mailbox, QueueCapacity};
use quickwit_common::extract_time_range;
use quickwit_common::uri::Uri;
use quickwit_doc_mapper::tag_pruning::extract_tags_from_query;
use quickwit_indexing::actors::MergeSplitDownloader;
use quickwit_indexing::merge_policy::{MergeOperation, MergePolicy};
use quickwit_metastore::{
    split_tag_filter, split_time_range_filter, Metastore, MetastoreResult, Split,
};
use quickwit_proto::metastore_api::DeleteTask;
use quickwit_proto::SearchRequest;
use quickwit_search::{jobs_to_leaf_request, SearchJob, SearchJobPlacer};
use serde::Serialize;
use tantivy::Inventory;
use tracing::{debug, info};

use crate::metrics::JANITOR_METRICS;

const PLANNER_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
const NUM_STALE_SPLITS_TO_FETCH: usize = 1000;

/// The `DeleteTaskPlanner` plans delete operations on splits for a given index.
/// For each split, the planner checks if there is some documents to delete:
/// - If this is the case, it sends a [`MergeOperation`] to the `MergeExecutor` `MergeOperation` to
///   the `MergeExecutor`.
/// - If there is no document to delete, it updates the split `delete_opstamp` to the latest delete
///   task opstamp.
///
/// Pseudo-algorithm for a given index:
/// 1. Fetches the delete tasks and deduce the last `opstamp`.
/// 2. Fetches the last `N` stale splits ordered by their `delete_opstamp`.
///    A stale split is a split a `delete_opstamp` inferior to the last `opstamp`
///    In theory, this works but... there is one difficulty:
///    - Delete operations do not run on immature splits and they are excluded after fetching stale
///      splits from the metastore as the metastore has no knowledge about the merge policy. If
///      there are more than `N` immature stale splits, the planner will plan no operations.
///      However, this is mitigated by the fact that a merge policy should consider "old split" as
///      mature and an index should not have many immature splits.
///      See tracked issue <https://github.com/quickwit-oss/quickwit/issues/2147>.
/// 3. If there is no stale splits, stop.
/// 4. If there are stale splits, for each split, do:
///    - Get the list of delete queries to apply to this split.
///    - Keep only delete queries that match the split metadata (time range and tags).
///    - If no delete queries remains, then update the split `delete_opstamp` to the latest
///      `opstamp`.
///    - If there are delete queries that match the metadata, do: + Execute delete queries
///      (`leaf_request`) one by one to check if there is a match. + As soon as a hit is returned
///      for a given query, the split is sent to the `MergeExecutor`. + If no delete queries match
///      documents, update the split `delete_opstamp` to the last `opstamp`.
#[derive(Clone)]
pub struct DeleteTaskPlanner {
    index_id: String,
    index_uri: Uri,
    doc_mapper_str: String,
    metastore: Arc<dyn Metastore>,
    search_job_placer: SearchJobPlacer,
    merge_policy: Arc<dyn MergePolicy>,
    merge_split_downloader_mailbox: Mailbox<MergeSplitDownloader>,
    /// Inventory of ongoing delete operations. If everything goes well,
    /// a merge operation is dropped after the publish of the split that underwent
    /// the delete operation.
    /// The inventory is used to avoid sending twice the same delete operation.
    ongoing_delete_operations_inventory: Inventory<MergeOperation>,
}

#[async_trait]
impl Actor for DeleteTaskPlanner {
    type ObservableState = DeleteTaskPlannerState;

    fn observable_state(&self) -> Self::ObservableState {
        let ongoing_delete_operations = self
            .ongoing_delete_operations_inventory
            .list()
            .iter()
            .map(|tracked_operation| tracked_operation.as_ref().clone())
            .collect_vec();
        DeleteTaskPlannerState {
            ongoing_delete_operations,
        }
    }

    fn name(&self) -> String {
        "DeleteTaskPlanner".to_string()
    }

    fn queue_capacity(&self) -> QueueCapacity {
        QueueCapacity::Bounded(0)
    }

    async fn initialize(&mut self, ctx: &ActorContext<Self>) -> Result<(), ActorExitStatus> {
        self.handle(PlanDeleteLoop, ctx).await
    }
}

impl DeleteTaskPlanner {
    pub fn new(
        index_id: String,
        index_uri: Uri,
        doc_mapper_str: String,
        metastore: Arc<dyn Metastore>,
        search_job_placer: SearchJobPlacer,
        merge_policy: Arc<dyn MergePolicy>,
        merge_split_downloader_mailbox: Mailbox<MergeSplitDownloader>,
    ) -> Self {
        Self {
            index_id,
            index_uri,
            doc_mapper_str,
            metastore,
            search_job_placer,
            merge_policy,
            merge_split_downloader_mailbox,
            ongoing_delete_operations_inventory: Inventory::new(),
        }
    }

    /// Send delete operations for a given `index_id`.
    async fn send_delete_operations(&mut self, ctx: &ActorContext<Self>) -> anyhow::Result<()> {
        // Loop until there is no more stale splits.
        loop {
            let last_delete_opstamp = self.metastore.last_delete_opstamp(&self.index_id).await?;
            let stale_splits = self
                .get_relevant_stale_splits(&self.index_id, last_delete_opstamp, ctx)
                .await?;
            ctx.record_progress();
            info!(
                index_id = self.index_id,
                last_delete_opstamp = last_delete_opstamp,
                num_stale_splits = stale_splits.len()
            );

            if stale_splits.is_empty() {
                break;
            }

            let (splits_with_deletes, splits_without_deletes) =
                self.partition_splits_by_deletes(&stale_splits, ctx).await?;
            info!(
                "{} splits with deletes, {} splits without deletes.",
                splits_with_deletes.len(),
                splits_without_deletes.len()
            );
            ctx.record_progress();

            // Updates `delete_opstamp` of splits that won't undergo delete operations.
            let split_ids_without_delete = splits_without_deletes
                .iter()
                .map(|split| split.split_id())
                .collect_vec();
            ctx.protect_future(self.metastore.update_splits_delete_opstamp(
                &self.index_id,
                &split_ids_without_delete,
                last_delete_opstamp,
            ))
            .await?;

            // Sends delete operations.
            for split_with_deletes in splits_with_deletes {
                let delete_operation = MergeOperation::new_delete_and_merge_operation(
                    split_with_deletes.split_metadata,
                );
                info!(delete_operation=?delete_operation, "Planned delete operation.");
                let tracked_delete_operation = self
                    .ongoing_delete_operations_inventory
                    .track(delete_operation);
                ctx.send_message(
                    &self.merge_split_downloader_mailbox,
                    tracked_delete_operation,
                )
                .await?;
                JANITOR_METRICS
                    .ongoing_num_delete_operations_total
                    .with_label_values([&self.index_id])
                    .set(self.ongoing_delete_operations_inventory.list().len() as i64);
            }
        }

        Ok(())
    }

    /// Identifies splits that contain documents to delete and
    /// splits that do not and returns the two groups.
    async fn partition_splits_by_deletes(
        &self,
        stale_splits: &[Split],
        ctx: &ActorContext<Self>,
    ) -> anyhow::Result<(Vec<Split>, Vec<Split>)> {
        let mut splits_without_deletes: Vec<Split> = Vec::new();
        let mut splits_with_deletes: Vec<Split> = Vec::new();

        for stale_split in stale_splits {
            let pending_tasks = ctx
                .protect_future(
                    self.metastore.list_delete_tasks(
                        &self.index_id,
                        stale_split.split_metadata.delete_opstamp,
                    ),
                )
                .await?;

            // Keep only delete tasks that matches the split metadata.
            let pending_and_matching_metadata_tasks = pending_tasks
                .into_iter()
                .filter(|delete_task| {
                    let delete_query = delete_task
                        .delete_query
                        .as_ref()
                        .expect("Delete task must have a delete query.");
                    let time_range = extract_time_range(
                        delete_query.start_timestamp,
                        delete_query.end_timestamp,
                    );
                    // TODO: validate the query at the beginning and return an appropriate error.
                    let tags_filter = extract_tags_from_query(&delete_query.query)
                        .expect("Delete query must have been validated upfront.");
                    split_time_range_filter(stale_split, time_range.as_ref())
                        && split_tag_filter(stale_split, tags_filter.as_ref())
                })
                .collect_vec();

            // If there is no matching delete tasks,
            // there is no document to delete on this split.
            if pending_and_matching_metadata_tasks.is_empty() {
                splits_without_deletes.push(stale_split.clone());
                continue;
            }

            let has_split_docs_to_delete = self
                .has_split_docs_to_delete(
                    stale_split,
                    &pending_and_matching_metadata_tasks,
                    &self.doc_mapper_str,
                    self.index_uri.as_str(),
                    ctx,
                )
                .await?;
            ctx.record_progress();

            if has_split_docs_to_delete {
                splits_with_deletes.push(stale_split.clone());
            } else {
                splits_without_deletes.push(stale_split.clone());
            }
        }

        Ok((splits_with_deletes, splits_without_deletes))
    }

    /// Executes a `LeafSearchRequet` on the split and returns true
    /// if it matches documents.
    async fn has_split_docs_to_delete(
        &self,
        stale_split: &Split,
        delete_tasks: &[DeleteTask],
        doc_mapper_str: &str,
        index_uri: &str,
        ctx: &ActorContext<Self>,
    ) -> anyhow::Result<bool> {
        let search_job = SearchJob::from(&stale_split.split_metadata);
        let mut search_client = self
            .search_job_placer
            .assign_job(search_job.clone(), &HashSet::new())?;
        for delete_task in delete_tasks {
            let delete_query = delete_task
                .delete_query
                .as_ref()
                .expect("Delete task must have a delete query.");
            let search_request = SearchRequest {
                index_id: delete_query.index_id.clone(),
                query: delete_query.query.clone(),
                start_timestamp: delete_query.start_timestamp,
                end_timestamp: delete_query.end_timestamp,
                search_fields: delete_query.search_fields.clone(),
                max_hits: 0,
                ..Default::default()
            };
            let leaf_search_request = jobs_to_leaf_request(
                &search_request,
                doc_mapper_str,
                index_uri,
                vec![search_job.clone()],
            );
            let response = search_client.leaf_search(leaf_search_request).await?;
            ctx.record_progress();
            if response.num_hits > 0 {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Fetches stale splits from [`Metastore`] and excludes immature splits and split already among
    /// ongoing delete operations.
    async fn get_relevant_stale_splits(
        &self,
        index_id: &str,
        last_delete_opstamp: u64,
        ctx: &ActorContext<Self>,
    ) -> MetastoreResult<Vec<Split>> {
        let stale_splits = ctx
            .protect_future(self.metastore.list_stale_splits(
                index_id,
                last_delete_opstamp,
                NUM_STALE_SPLITS_TO_FETCH,
            ))
            .await?;
        debug!(
            index_id = index_id,
            last_delete_opstamp = last_delete_opstamp,
            num_stale_splits_from_metastore = stale_splits.len()
        );
        // Keep only mature splits and splits that are not already part of ongoing delete
        // operations.
        let ongoing_delete_operations = self.ongoing_delete_operations_inventory.list();
        let filtered_splits = stale_splits
            .into_iter()
            .filter(|stale_split| self.merge_policy.is_mature(&stale_split.split_metadata))
            .filter(|stale_split| {
                !ongoing_delete_operations.iter().any(|operation| {
                    operation
                        .splits
                        .first()
                        .unwrap() // <- This is safe as we know for sure that an operation is on one split.
                        .split_id()
                        == stale_split.split_id()
                })
            })
            .collect_vec();
        Ok(filtered_splits)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DeleteTaskPlannerState {
    ongoing_delete_operations: Vec<MergeOperation>,
}

#[derive(Debug)]
struct PlanDeleteOperations;

#[async_trait]
impl Handler<PlanDeleteOperations> for DeleteTaskPlanner {
    type Reply = ();

    async fn handle(
        &mut self,
        _: PlanDeleteOperations,
        ctx: &ActorContext<Self>,
    ) -> Result<(), ActorExitStatus> {
        self.send_delete_operations(ctx).await?;
        Ok(())
    }
}

#[derive(Debug)]
struct PlanDeleteLoop;

#[async_trait]
impl Handler<PlanDeleteLoop> for DeleteTaskPlanner {
    type Reply = ();

    async fn handle(
        &mut self,
        _: PlanDeleteLoop,
        ctx: &ActorContext<Self>,
    ) -> Result<(), ActorExitStatus> {
        self.handle(PlanDeleteOperations, ctx).await?;
        ctx.schedule_self_msg(PLANNER_REFRESH_INTERVAL, PlanDeleteLoop)
            .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use quickwit_actors::Universe;
    use quickwit_config::build_doc_mapper;
    use quickwit_grpc_clients::service_client_pool::ServiceClientPool;
    use quickwit_indexing::merge_policy::{MergeOperation, NopMergePolicy};
    use quickwit_indexing::TestSandbox;
    use quickwit_metastore::SplitMetadata;
    use quickwit_proto::metastore_api::DeleteQuery;
    use quickwit_proto::{LeafSearchRequest, LeafSearchResponse};
    use quickwit_search::{MockSearchService, SearchServiceClient};
    use tantivy::TrackedObject;

    use super::*;

    #[tokio::test]
    async fn test_delete_task_planner() -> anyhow::Result<()> {
        quickwit_common::setup_logging_for_tests();
        let universe = Universe::with_accelerated_time();
        let index_id = "test-delete-task-planner";
        let doc_mapping_yaml = r#"
            field_mappings:
              - name: body
                type: text
              - name: ts
                type: i64
                fast: true
        "#;
        let test_sandbox = TestSandbox::create(index_id, doc_mapping_yaml, "{}", &["body"]).await?;
        let docs = [
            serde_json::json!({"body": "info", "ts": 0 }),
            serde_json::json!({"body": "info", "ts": 0 }),
            serde_json::json!({"body": "delete", "ts": 0 }),
        ];
        // Creates 3 splits
        for doc in docs {
            test_sandbox.add_documents(vec![doc]).await?;
        }
        let metastore = test_sandbox.metastore();
        let index_config = metastore
            .index_metadata(index_id)
            .await?
            .into_index_config();
        let split_metas: Vec<SplitMetadata> = metastore
            .list_all_splits(index_id)
            .await?
            .into_iter()
            .map(|split| split.split_metadata)
            .collect_vec();
        assert_eq!(split_metas.len(), 3);
        let doc_mapper =
            build_doc_mapper(&index_config.doc_mapping, &index_config.search_settings)?;
        let doc_mapper_str = serde_json::to_string(&doc_mapper)?;

        // Creates 2 delete tasks, one that will match 1 document,
        // the other that will match no document.

        metastore
            .create_delete_task(DeleteQuery {
                index_id: index_id.to_string(),
                start_timestamp: None,
                end_timestamp: None,
                query: "body:delete".to_string(),
                search_fields: Vec::new(),
            })
            .await?;
        metastore
            .create_delete_task(DeleteQuery {
                index_id: index_id.to_string(),
                start_timestamp: None,
                end_timestamp: None,
                query: "MatchNothing".to_string(),
                search_fields: Vec::new(),
            })
            .await?;
        let mut mock_search_service = MockSearchService::new();

        // We have 2 delete tasks. Each one will trigger a leaf request for each
        // of the 3 splits. This makes 6 requests.
        let split_id_with_doc_to_delete = split_metas[2].split_id().to_string();
        mock_search_service.expect_leaf_search().times(6).returning(
            move |request: LeafSearchRequest| {
                // Search on body:delete should return one hit only on the last split
                // that should contains the doc.
                if request.split_offsets[0].split_id == split_id_with_doc_to_delete
                    && request.search_request.as_ref().unwrap().query == "body:delete"
                {
                    return Ok(LeafSearchResponse {
                        num_hits: 1,
                        ..Default::default()
                    });
                }
                Ok(LeafSearchResponse {
                    num_hits: 0,
                    ..Default::default()
                })
            },
        );
        let client_pool =
            ServiceClientPool::for_clients_list(vec![SearchServiceClient::from_service(
                Arc::new(mock_search_service),
                ([127, 0, 0, 1], 1000).into(),
            )]);
        let search_job_placer = SearchJobPlacer::new(client_pool);
        let (downloader_mailbox, downloader_inbox) = universe.create_test_mailbox();
        let delete_planner_executor = DeleteTaskPlanner::new(
            index_id.to_string(),
            index_config.index_uri.clone(),
            doc_mapper_str,
            metastore.clone(),
            search_job_placer,
            Arc::new(NopMergePolicy),
            downloader_mailbox,
        );
        let (delete_planner_mailbox, delete_planner_handle) =
            universe.spawn_builder().spawn(delete_planner_executor);
        delete_planner_handle.process_pending_and_observe().await;
        let downloader_msgs: Vec<TrackedObject<MergeOperation>> =
            downloader_inbox.drain_for_test_typed();
        assert_eq!(downloader_msgs.len(), 1);
        // The last split will undergo a delete operation.
        assert_eq!(
            downloader_msgs[0].splits[0].split_id(),
            split_metas[2].split_id()
        );
        // Check planner state is inline.
        let delete_planner_state = delete_planner_handle.observe().await;
        assert_eq!(
            delete_planner_state.ongoing_delete_operations[0].splits[0].split_id(),
            split_metas[2].split_id()
        );
        // Trigger new plan evaluation and check that we don't have new merge operation.
        delete_planner_mailbox
            .ask(PlanDeleteOperations)
            .await
            .unwrap();
        assert!(downloader_inbox.drain_for_test().is_empty());
        // Now drop the current merge operation and check that the planner will plan a new
        // operation.
        drop(downloader_msgs.into_iter().next().unwrap());
        // Check planner state is inline.
        assert!(delete_planner_handle
            .observe()
            .await
            .ongoing_delete_operations
            .is_empty());

        // Trigger operations planning.
        delete_planner_mailbox
            .ask(PlanDeleteOperations)
            .await
            .unwrap();
        let downloader_last_msgs =
            downloader_inbox.drain_for_test_typed::<TrackedObject<MergeOperation>>();
        assert_eq!(downloader_last_msgs.len(), 1);
        assert_eq!(
            downloader_last_msgs[0].splits[0].split_id(),
            split_metas[2].split_id()
        );
        // The other splits has just their delete opstamps updated to the last opstamps which is 2
        // as there are 2 delete tasks. The last split
        let all_splits = metastore.list_all_splits(index_id).await?;
        assert_eq!(all_splits[0].split_metadata.delete_opstamp, 2);
        assert_eq!(all_splits[1].split_metadata.delete_opstamp, 2);
        // The last split has not yet its delete opstamp updated.
        assert_eq!(all_splits[2].split_metadata.delete_opstamp, 0);
        Ok(())
    }
}
