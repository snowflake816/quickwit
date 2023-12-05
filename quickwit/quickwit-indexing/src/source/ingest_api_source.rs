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

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use quickwit_actors::{ActorContext, ActorExitStatus, Mailbox};
use quickwit_ingest::{
    get_ingest_api_service, CreateQueueIfNotExistsRequest, DocCommand, FetchRequest, FetchResponse,
    GetPartitionId, IngestApiService, SuggestTruncateRequest,
};
use quickwit_metastore::checkpoint::{PartitionId, SourceCheckpoint};
use quickwit_proto::types::Position;
use serde::Serialize;
use serde_json::Value as JsonValue;

use super::{Source, SourceActor, SourceContext, TypedSourceFactory};
use crate::actors::DocProcessor;
use crate::models::RawDocBatch;
use crate::source::SourceRuntimeArgs;

/// Wait time for SourceActor before pooling for new documents.
/// TODO: Think of better way, maybe increment this (i.e wait longer) as time
/// goes on without receiving docs.
const INGEST_API_POLLING_COOL_DOWN: Duration = Duration::from_secs(1);

#[derive(Default, Clone, Debug, Eq, PartialEq, Serialize)]
pub struct IngestApiSourceCounters {
    /// Maintains the value of where we stopped in queue from
    /// a previous call on `emit_batch` and allows
    /// setting the lower-bound of the checkpoint delta.
    /// It has the same value as `current_offset` at the end of emit_batch.
    pub previous_offset: Option<u64>,
    /// Maintains the value of where we are in queue and allows
    /// setting the upper-bound of the checkpoint delta.
    pub current_offset: Option<u64>,
    pub num_docs_processed: u64,
}

pub struct IngestApiSource {
    runtime_args: Arc<SourceRuntimeArgs>,
    source_id: String,
    partition_id: PartitionId,
    ingest_api_service: Mailbox<IngestApiService>,
    counters: IngestApiSourceCounters,
}

impl fmt::Debug for IngestApiSource {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "IngestApiSource {{ source_id: {} }}", self.source_id)
    }
}

impl IngestApiSource {
    pub async fn try_new(
        runtime_args: Arc<SourceRuntimeArgs>,
        checkpoint: SourceCheckpoint,
    ) -> anyhow::Result<Self> {
        let source_id = runtime_args.source_id().to_string();
        let queues_dir_path = runtime_args.queues_dir_path.as_path();
        let ingest_api_service = get_ingest_api_service(queues_dir_path).await?;
        let partition_id: PartitionId = ingest_api_service.ask(GetPartitionId).await?.into();

        // Ensure a queue for this index exists.
        let create_queue_req = CreateQueueIfNotExistsRequest {
            queue_id: runtime_args.index_id().to_string(),
        };
        ingest_api_service.ask_for_res(create_queue_req).await?;

        let previous_offset: Option<u64> = checkpoint
            .position_for_partition(&partition_id)
            .map(|position| position.as_u64().expect("offset should be stored as u64"));
        let current_offset = previous_offset;
        let ingest_api_source = IngestApiSource {
            runtime_args,
            source_id,
            partition_id,
            ingest_api_service,
            counters: IngestApiSourceCounters {
                previous_offset,
                current_offset,
                num_docs_processed: 0,
            },
        };
        Ok(ingest_api_source)
    }

    async fn send_suggest_truncate_to_ingest_service(
        &self,
        up_to_position_included: u64,
        ctx: &ActorContext<SourceActor>,
    ) -> anyhow::Result<()> {
        let suggest_truncate_req = SuggestTruncateRequest {
            index_id: self.runtime_args.index_id().to_string(),
            up_to_position_included,
        };
        ctx.ask_for_res(&self.ingest_api_service, suggest_truncate_req)
            .await?;
        Ok(())
    }

    fn update_counters(&mut self, current_offset: u64, num_docs: u64) {
        self.counters.num_docs_processed += num_docs;
        self.counters.current_offset = Some(current_offset);
        self.counters.previous_offset = Some(current_offset);
    }
}

#[async_trait]
impl Source for IngestApiSource {
    async fn initialize(
        &mut self,
        _: &Mailbox<DocProcessor>,
        ctx: &SourceContext,
    ) -> Result<(), ActorExitStatus> {
        if let Some(position) = self.counters.previous_offset {
            self.send_suggest_truncate_to_ingest_service(position, ctx)
                .await?;
        }
        Ok(())
    }

    async fn emit_batches(
        &mut self,
        batch_sink: &Mailbox<DocProcessor>,
        ctx: &SourceContext,
    ) -> Result<Duration, ActorExitStatus> {
        let fetch_req = FetchRequest {
            index_id: self.runtime_args.index_id().to_string(),
            start_after: self.counters.current_offset,
            num_bytes_limit: None,
        };
        let FetchResponse {
            first_position: first_position_opt,
            doc_batch: doc_batch_opt,
        } = ctx
            .ask_for_res(&self.ingest_api_service, fetch_req)
            .await
            .map_err(anyhow::Error::from)?;

        // The `first_position_opt` being none means the doc_batch is empty and there is
        // no more document available, at least for the time being.
        // That is, we have consumed all pending docs in the queue and need to
        // make the client wait a bit before pooling again.
        let (first_position, doc_batch) = if let Some(first_position) = first_position_opt {
            (first_position, doc_batch_opt.unwrap())
        } else {
            return Ok(INGEST_API_POLLING_COOL_DOWN);
        };

        // TODO use a timestamp (in the raw doc batch) given by at ingest time to be more accurate.
        let mut raw_doc_batch = RawDocBatch::with_capacity(doc_batch.num_docs());
        for doc in doc_batch.iter() {
            match doc {
                DocCommand::Ingest { payload } => raw_doc_batch.docs.push(payload),
                DocCommand::Commit => raw_doc_batch.force_commit = true,
            }
        }
        let current_offset = first_position + doc_batch.num_docs() as u64 - 1;
        let partition_id = self.partition_id.clone();
        raw_doc_batch
            .checkpoint_delta
            .record_partition_delta(
                partition_id,
                self.counters
                    .previous_offset
                    .map(Position::offset)
                    .unwrap_or_default(),
                Position::offset(current_offset),
            )
            .map_err(anyhow::Error::from)?;

        self.update_counters(current_offset, raw_doc_batch.docs.len() as u64);
        ctx.send_message(batch_sink, raw_doc_batch).await?;
        Ok(Duration::default())
    }

    async fn suggest_truncate(
        &mut self,
        checkpoint: SourceCheckpoint,
        ctx: &ActorContext<SourceActor>,
    ) -> anyhow::Result<()> {
        if let Some(Position::Offset(offset)) =
            checkpoint.position_for_partition(&self.partition_id)
        {
            let up_to_position_included = offset.as_u64().expect("offset should be stored as u64");
            self.send_suggest_truncate_to_ingest_service(up_to_position_included, ctx)
                .await?;
        }
        Ok(())
    }

    fn name(&self) -> String {
        "IngestApiSource".to_string()
    }

    fn observable_state(&self) -> JsonValue {
        serde_json::to_value(&self.counters).unwrap()
    }
}

pub struct IngestApiSourceFactory;

#[async_trait]
impl TypedSourceFactory for IngestApiSourceFactory {
    type Source = IngestApiSource;
    type Params = ();

    async fn typed_create_source(
        ctx: Arc<SourceRuntimeArgs>,
        _: (),
        checkpoint: SourceCheckpoint,
    ) -> anyhow::Result<Self::Source> {
        IngestApiSource::try_new(ctx, checkpoint).await
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::time::Duration;

    use quickwit_actors::Command::Nudge;
    use quickwit_actors::Universe;
    use quickwit_common::rand::append_random_suffix;
    use quickwit_config::{
        IngestApiConfig, SourceConfig, SourceInputFormat, SourceParams, INGEST_API_SOURCE_ID,
    };
    use quickwit_ingest::{init_ingest_api, CommitType, DocBatchBuilder, IngestRequest};
    use quickwit_metastore::checkpoint::{SourceCheckpoint, SourceCheckpointDelta};
    use quickwit_metastore::metastore_for_test;
    use quickwit_proto::types::IndexUid;

    use super::*;
    use crate::source::SourceActor;

    fn make_ingest_request(
        index_id: String,
        num_batch: u64,
        batch_size: usize,
        commit_type: CommitType,
    ) -> IngestRequest {
        let mut doc_batches = Vec::new();
        let mut doc_id = 0usize;
        for _ in 0..num_batch {
            let mut doc_batch_builder = DocBatchBuilder::new(index_id.clone());
            for _ in 0..batch_size {
                doc_batch_builder.ingest_doc(
                    format!("{doc_id:0>6} - The quick brown fox jumps over the lazy dog")
                        .as_bytes(),
                );
                doc_id += 1;
            }
            doc_batches.push(doc_batch_builder.build());
        }
        IngestRequest {
            doc_batches,
            commit: commit_type.into(),
        }
    }

    fn make_source_config() -> SourceConfig {
        SourceConfig {
            source_id: INGEST_API_SOURCE_ID.to_string(),
            desired_num_pipelines: NonZeroUsize::new(1).unwrap(),
            max_num_pipelines_per_indexer: NonZeroUsize::new(1).unwrap(),
            enabled: true,
            source_params: SourceParams::IngestApi,
            transform_config: None,
            input_format: SourceInputFormat::Json,
        }
    }

    #[tokio::test]
    async fn test_ingest_api_source() -> anyhow::Result<()> {
        let universe = Universe::with_accelerated_time();
        let metastore = metastore_for_test();
        let index_id = append_random_suffix("test-ingest-api-source");
        let index_uid = IndexUid::new_with_random_ulid(&index_id);
        let temp_dir = tempfile::tempdir()?;
        let queues_dir_path = temp_dir.path();

        let ingest_api_service =
            init_ingest_api(&universe, queues_dir_path, &IngestApiConfig::default()).await?;
        let (doc_processor_mailbox, doc_processor_inbox) = universe.create_test_mailbox();
        let source_config = make_source_config();
        let ctx = SourceRuntimeArgs::for_test(
            index_uid,
            source_config,
            metastore,
            queues_dir_path.to_path_buf(),
        );
        let ingest_api_source = IngestApiSource::try_new(ctx, SourceCheckpoint::default()).await?;
        let ingest_api_source_actor = SourceActor {
            source: Box::new(ingest_api_source),
            doc_processor_mailbox,
        };
        let (_ingest_api_source_mailbox, ingest_api_source_handle) =
            universe.spawn_builder().spawn(ingest_api_source_actor);

        let ingest_req = make_ingest_request(index_id.clone(), 2, 20_000, CommitType::Auto);
        ingest_api_service
            .ask_for_res(ingest_req)
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))?;
        universe.sleep(Duration::from_secs(2)).await;
        let counters = ingest_api_source_handle
            .process_pending_and_observe()
            .await
            .state;
        assert_eq!(
            counters,
            serde_json::json!({
                "previous_offset": 39999u64,
                "current_offset": 39999u64,
                "num_docs_processed": 40000u64
            })
        );
        let doc_batches: Vec<RawDocBatch> = doc_processor_inbox.drain_for_test_typed();
        assert_eq!(doc_batches.len(), 2);
        assert!(&doc_batches[1].docs[0].starts_with(b"037736"));
        // TODO: Source deadlocks and test hangs occasionally if we don't quit source first.
        ingest_api_source_handle.quit().await;
        universe.assert_quit().await;
        Ok(())
    }

    /// See #2310
    #[tokio::test]
    async fn test_ingest_api_source_partition_id_changes() -> anyhow::Result<()> {
        let universe = Universe::with_accelerated_time();
        let partition_id_before_lost_queue_dir = {
            let temp_dir = tempfile::tempdir()?;
            let queues_dir_path = temp_dir.path();
            let ingest_api_service =
                init_ingest_api(&universe, queues_dir_path, &IngestApiConfig::default()).await?;
            let partition_id: PartitionId = ingest_api_service.ask(GetPartitionId).await?.into();
            let partition_id2: PartitionId = ingest_api_service.ask(GetPartitionId).await?.into();
            assert_eq!(partition_id, partition_id2);
            drop(ingest_api_service);
            let ingest_api_service =
                init_ingest_api(&universe, queues_dir_path, &IngestApiConfig::default()).await?;
            let partition_id3: PartitionId = ingest_api_service.ask(GetPartitionId).await?.into();
            assert_eq!(partition_id, partition_id3);
            partition_id
        };
        let partition_id_after_lost_queue_dir = {
            let temp_dir = tempfile::tempdir()?;
            let queues_dir_path = temp_dir.path();
            let ingest_api_service =
                init_ingest_api(&universe, queues_dir_path, &IngestApiConfig::default()).await?;
            let partition_id: PartitionId = ingest_api_service.ask(GetPartitionId).await?.into();
            partition_id
        };
        assert_ne!(
            partition_id_before_lost_queue_dir,
            partition_id_after_lost_queue_dir
        );
        universe.assert_quit().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_ingest_api_source_resume_from_checkpoint() -> anyhow::Result<()> {
        let universe = Universe::with_accelerated_time();
        let metastore = metastore_for_test();
        let index_id = append_random_suffix("test-ingest-api-source");
        let index_uid = IndexUid::new_with_random_ulid(&index_id);
        let temp_dir = tempfile::tempdir()?;
        let queues_dir_path = temp_dir.path();

        let ingest_api_service =
            init_ingest_api(&universe, queues_dir_path, &IngestApiConfig::default()).await?;
        let create_queue_req = CreateQueueIfNotExistsRequest {
            queue_id: index_id.clone(),
        };
        ingest_api_service
            .ask_for_res(create_queue_req)
            .await
            .unwrap();

        let ingest_req = make_ingest_request(index_id.clone(), 4, 1000, CommitType::Auto);
        ingest_api_service
            .ask_for_res(ingest_req)
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))?;

        let (doc_processor_mailbox, doc_processor_inbox) = universe.create_test_mailbox();
        let mut checkpoint = SourceCheckpoint::default();
        let partition_id: PartitionId = ingest_api_service.ask(GetPartitionId).await?.into();
        let checkpoint_delta = SourceCheckpointDelta::from_partition_delta(
            partition_id.clone(),
            Position::offset(0u64),
            Position::offset(1200u64),
        )
        .unwrap();
        checkpoint.try_apply_delta(checkpoint_delta).unwrap();

        let source_config = make_source_config();
        let ctx = SourceRuntimeArgs::for_test(
            index_uid,
            source_config,
            metastore,
            queues_dir_path.to_path_buf(),
        );
        let ingest_api_source = IngestApiSource::try_new(ctx, checkpoint).await?;
        let ingest_api_source_actor = SourceActor {
            source: Box::new(ingest_api_source),
            doc_processor_mailbox,
        };
        let (_ingest_api_source_mailbox, ingest_api_source_handle) =
            universe.spawn_builder().spawn(ingest_api_source_actor);

        universe.sleep(Duration::from_secs(2)).await;
        let counters = ingest_api_source_handle
            .process_pending_and_observe()
            .await
            .state;
        assert_eq!(
            counters,
            serde_json::json!({
                "previous_offset": 3999u64,
                "current_offset": 3999u64,
                "num_docs_processed": 2799u64
            })
        );
        let doc_batches: Vec<RawDocBatch> = doc_processor_inbox.drain_for_test_typed();
        assert_eq!(doc_batches.len(), 1);
        assert!(&doc_batches[0].docs[0].starts_with(b"001201"));
        assert_eq!(doc_batches[0].checkpoint_delta.num_partitions(), 1);
        assert_eq!(
            doc_batches[0].checkpoint_delta.partitions().next().unwrap(),
            &partition_id
        );
        // TODO: Source deadlocks and test hangs occasionally if we don't quit source first.
        ingest_api_source_handle.quit().await;
        universe.assert_quit().await;

        Ok(())
    }

    #[tokio::test]
    async fn test_ingest_api_source_with_one_doc() -> anyhow::Result<()> {
        let universe = Universe::with_accelerated_time();
        let metastore = metastore_for_test();
        let index_id = append_random_suffix("test-ingest-api-source");
        let index_uid = IndexUid::new_with_random_ulid(&index_id);
        let temp_dir = tempfile::tempdir()?;
        let queues_dir_path = temp_dir.path();
        let ingest_api_service =
            init_ingest_api(&universe, queues_dir_path, &IngestApiConfig::default()).await?;

        let (doc_processor_mailbox, doc_processor_inbox) = universe.create_test_mailbox();
        let source_config = make_source_config();
        let ctx = SourceRuntimeArgs::for_test(
            index_uid,
            source_config,
            metastore,
            queues_dir_path.to_path_buf(),
        );
        let ingest_api_source = IngestApiSource::try_new(ctx, SourceCheckpoint::default()).await?;
        let ingest_api_source_actor = SourceActor {
            source: Box::new(ingest_api_source),
            doc_processor_mailbox,
        };
        let (_ingest_api_source_mailbox, ingest_api_source_handle) =
            universe.spawn_builder().spawn(ingest_api_source_actor);

        let ingest_req = make_ingest_request(index_id.clone(), 1, 1, CommitType::Auto);
        ingest_api_service
            .ask_for_res(ingest_req)
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))?;
        universe.sleep(Duration::from_secs(2)).await;
        let counters = ingest_api_source_handle
            .process_pending_and_observe()
            .await
            .state;
        assert_eq!(
            counters,
            serde_json::json!({
                "previous_offset": 0u64,
                "current_offset": 0u64,
                "num_docs_processed": 1u64
            })
        );
        let doc_batches: Vec<RawDocBatch> = doc_processor_inbox.drain_for_test_typed();
        assert_eq!(doc_batches.len(), 1);
        assert!(&doc_batches[0].docs[0].starts_with(b"000000"));
        // TODO: Source deadlocks and test hangs occasionally if we don't quit source first.
        ingest_api_source_handle.quit().await;
        universe.assert_quit().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_ingest_api_source_with_force_commit() -> anyhow::Result<()> {
        let universe = Universe::with_accelerated_time();
        let metastore = metastore_for_test();
        let index_id = append_random_suffix("test-ingest-api-source");
        let index_uid = IndexUid::new_with_random_ulid(&index_id);
        let temp_dir = tempfile::tempdir()?;
        let queues_dir_path = temp_dir.path();

        let ingest_api_service =
            init_ingest_api(&universe, queues_dir_path, &IngestApiConfig::default()).await?;
        let (doc_processor_mailbox, doc_processor_inbox) = universe.create_test_mailbox();
        let source_config = make_source_config();
        let ctx = SourceRuntimeArgs::for_test(
            index_uid,
            source_config,
            metastore,
            queues_dir_path.to_path_buf(),
        );
        let ingest_api_source = IngestApiSource::try_new(ctx, SourceCheckpoint::default()).await?;
        let ingest_api_source_actor = SourceActor {
            source: Box::new(ingest_api_source),
            doc_processor_mailbox,
        };
        let (_ingest_api_source_mailbox, ingest_api_source_handle) =
            universe.spawn_builder().spawn(ingest_api_source_actor);

        let ingest_req = make_ingest_request(index_id.clone(), 2, 20_000, CommitType::Force);
        let ingest_res = ingest_api_service
            .send_message(ingest_req)
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))?;
        universe.sleep(Duration::from_secs(2)).await;
        let counters = ingest_api_source_handle
            .process_pending_and_observe()
            .await
            .state;
        assert_eq!(
            counters,
            serde_json::json!({
                "previous_offset": 40001u64,
                "current_offset": 40001u64,
                "num_docs_processed": 40000u64
            })
        );
        let doc_batches: Vec<RawDocBatch> = doc_processor_inbox.drain_for_test_typed();
        assert_eq!(doc_batches.len(), 2);
        assert!(doc_batches[1].docs[0].starts_with(b"037736"));
        assert!(doc_batches[0].force_commit);
        assert!(doc_batches[1].force_commit);
        ingest_api_service
            .ask_for_res(SuggestTruncateRequest {
                index_id: index_id.clone(),
                up_to_position_included: 40001,
            })
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))?;
        let res = ingest_res
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))?
            .map_err(|err| anyhow::anyhow!(err.to_string()))?;
        assert_eq!(res.num_docs_for_processing, 40_000);
        ingest_api_source_handle.quit().await;
        universe.assert_quit().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_ingest_api_source_with_wait() -> anyhow::Result<()> {
        let universe = Universe::with_accelerated_time();
        let metastore = metastore_for_test();
        let index_id = append_random_suffix("test-ingest-api-source");
        let index_uid = IndexUid::new_with_random_ulid(&index_id);
        let temp_dir = tempfile::tempdir()?;
        let queues_dir_path = temp_dir.path();

        let ingest_api_service =
            init_ingest_api(&universe, queues_dir_path, &IngestApiConfig::default()).await?;
        let (doc_processor_mailbox, doc_processor_inbox) = universe.create_test_mailbox();
        let source_config = make_source_config();
        let ctx = SourceRuntimeArgs::for_test(
            index_uid,
            source_config,
            metastore,
            queues_dir_path.to_path_buf(),
        );
        let ingest_api_source = IngestApiSource::try_new(ctx, SourceCheckpoint::default()).await?;
        let ingest_api_source_actor = SourceActor {
            source: Box::new(ingest_api_source),
            doc_processor_mailbox,
        };
        let (_ingest_api_source_mailbox, ingest_api_source_handle) =
            universe.spawn_builder().spawn(ingest_api_source_actor);
        let ingest_req = make_ingest_request(index_id.clone(), 2, 20_000, CommitType::WaitFor);
        let ingest_res = ingest_api_service
            .send_message(ingest_req)
            .await
            .map_err(|err| anyhow::anyhow!(err.to_string()))?;
        universe.sleep(Duration::from_secs(2)).await;
        let counters = ingest_api_source_handle
            .process_pending_and_observe()
            .await
            .state;
        assert_eq!(
            counters,
            serde_json::json!({
                "previous_offset": 39999u64,
                "current_offset": 39999u64,
                "num_docs_processed": 40000u64
            })
        );
        let doc_batches: Vec<RawDocBatch> = doc_processor_inbox.drain_for_test_typed();
        assert_eq!(doc_batches.len(), 2);
        assert!(doc_batches[1].docs[0].starts_with(b"037736"));
        assert!(!doc_batches[0].force_commit);
        assert!(!doc_batches[1].force_commit);
        ingest_api_service
            .ask_for_res(SuggestTruncateRequest {
                index_id: index_id.clone(),
                up_to_position_included: 39999,
            })
            .await
            .unwrap();
        let res = ingest_res.await.unwrap().unwrap();
        assert_eq!(res.num_docs_for_processing, 40_000);
        ingest_api_source_handle.quit().await;
        universe.assert_quit().await;
        Ok(())
    }

    #[tokio::test]
    async fn test_ingest_api_source_truncate_on_initialize() -> anyhow::Result<()> {
        let universe = Universe::with_accelerated_time();
        let metastore = metastore_for_test();
        let index_id = append_random_suffix("test-ingest-api-source");
        let index_uid = IndexUid::new_with_random_ulid(&index_id);
        let temp_dir = tempfile::tempdir()?;
        let queues_dir_path = temp_dir.path();

        let ingest_api_service =
            init_ingest_api(&universe, queues_dir_path, &IngestApiConfig::default()).await?;
        let (doc_processor_mailbox, _doc_processor_inbox) = universe.create_test_mailbox();
        let source_config = make_source_config();
        let ctx = SourceRuntimeArgs::for_test(
            index_uid,
            source_config,
            metastore,
            queues_dir_path.to_path_buf(),
        );

        let create_queue_req = CreateQueueIfNotExistsRequest {
            queue_id: index_id.clone(),
        };
        ingest_api_service
            .ask_for_res(create_queue_req)
            .await
            .unwrap();

        let ingest_req = make_ingest_request(index_id.clone(), 2, 20_000, CommitType::Auto);
        ingest_api_service.ask(ingest_req).await.unwrap().unwrap();

        let fetch_request = FetchRequest {
            index_id: index_id.clone(),
            start_after: None,
            num_bytes_limit: None,
        };
        let FetchResponse { first_position, .. } = ingest_api_service
            .ask(fetch_request.clone())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first_position, Some(0));

        let partition_id = ingest_api_service.ask(GetPartitionId).await?.into();
        let mut source_checkpoint = SourceCheckpoint::default();
        source_checkpoint.add_partition(partition_id, Position::offset(10u64));
        let ingest_api_source = IngestApiSource::try_new(ctx, source_checkpoint).await?;
        let ingest_api_source_actor = SourceActor {
            source: Box::new(ingest_api_source),
            doc_processor_mailbox,
        };
        let (ingest_api_source_mailbox, ingest_api_source_handle) =
            universe.spawn_builder().spawn(ingest_api_source_actor);

        ingest_api_source_mailbox.ask(Nudge).await.unwrap();
        let FetchResponse { first_position, .. } = ingest_api_service
            .ask(fetch_request.clone())
            .await
            .unwrap()
            .unwrap();
        // We should have truncated to keep only message strictly after the source checkpoint.
        assert_eq!(first_position, Some(11u64));

        ingest_api_source_handle.quit().await;
        universe.assert_quit().await;
        Ok(())
    }
}
