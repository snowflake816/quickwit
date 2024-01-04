// Copyright (C) 2024 Quickwit, Inc.
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

use std::path::Path;

use async_trait::async_trait;
use quickwit_actors::{Actor, ActorContext, ActorExitStatus, Handler, Mailbox, QueueCapacity};
use quickwit_common::io::IoControls;
use quickwit_common::temp_dir::{self, TempDirectory};
use quickwit_metastore::SplitMetadata;
use tantivy::{Directory, TrackedObject};
use tracing::{debug, info, instrument};

use super::MergeExecutor;
use crate::merge_policy::MergeOperation;
use crate::models::MergeScratch;
use crate::split_store::IndexingSplitStore;

#[derive(Clone)]
pub struct MergeSplitDownloader {
    pub scratch_directory: TempDirectory,
    pub split_store: IndexingSplitStore,
    pub executor_mailbox: Mailbox<MergeExecutor>,
    pub io_controls: IoControls,
}

impl Actor for MergeSplitDownloader {
    type ObservableState = ();
    fn observable_state(&self) -> Self::ObservableState {}

    fn queue_capacity(&self) -> QueueCapacity {
        QueueCapacity::Bounded(1)
    }

    fn name(&self) -> String {
        "MergeSplitDownloader".to_string()
    }
}

#[async_trait]
impl Handler<TrackedObject<MergeOperation>> for MergeSplitDownloader {
    type Reply = ();

    #[instrument(
        name = "merge_split_downloader",
        parent = merge_operation.merge_parent_span.id(),
        skip_all,
    )]
    async fn handle(
        &mut self,
        merge_operation: TrackedObject<MergeOperation>,
        ctx: &ActorContext<Self>,
    ) -> Result<(), quickwit_actors::ActorExitStatus> {
        let merge_scratch_directory = temp_dir::Builder::default()
            .join("merge")
            .tempdir_in(self.scratch_directory.path())
            .map_err(|error| anyhow::anyhow!(error))?;
        info!(dir=%merge_scratch_directory.path().display(), "download-merge-splits");
        let downloaded_splits_directory = temp_dir::Builder::default()
            .join("downloaded-splits")
            .tempdir_in(merge_scratch_directory.path())
            .map_err(|error| anyhow::anyhow!(error))?;
        let tantivy_dirs = self
            .download_splits(
                merge_operation.splits_as_slice(),
                downloaded_splits_directory.path(),
                ctx,
            )
            .await?;
        let msg = MergeScratch {
            merge_operation,
            merge_scratch_directory,
            downloaded_splits_directory,
            tantivy_dirs,
        };
        ctx.send_message(&self.executor_mailbox, msg).await?;
        Ok(())
    }
}

impl MergeSplitDownloader {
    async fn download_splits(
        &self,
        splits: &[SplitMetadata],
        download_directory: &Path,
        ctx: &ActorContext<Self>,
    ) -> Result<Vec<Box<dyn Directory>>, quickwit_actors::ActorExitStatus> {
        // we download all of the split files in the scratch directory.
        let mut tantivy_dirs = Vec::new();
        for split in splits {
            if ctx.kill_switch().is_dead() {
                debug!(
                    split_id = split.split_id(),
                    "Kill switch was activated. Cancelling download."
                );
                return Err(ActorExitStatus::Killed);
            }
            let io_controls = self
                .io_controls
                .clone()
                .set_progress(ctx.progress().clone())
                .set_kill_switch(ctx.kill_switch().clone());
            let _protect_guard = ctx.protect_zone();
            let tantivy_dir = self
                .split_store
                .fetch_and_open_split(split.split_id(), download_directory, &io_controls)
                .await
                .map_err(|error| {
                    let split_id = split.split_id();
                    anyhow::anyhow!(error).context(format!("failed to download split `{split_id}`"))
                })?;
            tantivy_dirs.push(tantivy_dir);
        }
        Ok(tantivy_dirs)
    }
}

#[cfg(test)]
mod tests {
    use std::iter;
    use std::sync::Arc;

    use quickwit_actors::Universe;
    use quickwit_common::split_file;
    use quickwit_storage::{PutPayload, RamStorageBuilder, SplitPayloadBuilder};
    use tantivy::Inventory;

    use super::*;
    use crate::new_split_id;

    #[tokio::test]
    async fn test_merge_split_downloader() -> anyhow::Result<()> {
        let scratch_directory = TempDirectory::for_test();
        let splits_to_merge: Vec<SplitMetadata> = iter::repeat_with(|| {
            let split_id = new_split_id();
            SplitMetadata {
                split_id,
                ..Default::default()
            }
        })
        .take(10)
        .collect();

        let split_store = {
            let mut storage_builder = RamStorageBuilder::default();
            for split in &splits_to_merge {
                let buffer = SplitPayloadBuilder::get_split_payload(&[], &[], &[1, 2, 3])?
                    .read_all()
                    .await?;
                storage_builder = storage_builder.put(&split_file(split.split_id()), &buffer);
            }
            let ram_storage = storage_builder.build();
            IndexingSplitStore::create_without_local_store_for_test(Arc::new(ram_storage))
        };

        let universe = Universe::with_accelerated_time();
        let (merge_executor_mailbox, merge_executor_inbox) = universe.create_test_mailbox();
        let merge_split_downloader = MergeSplitDownloader {
            scratch_directory,
            split_store,
            executor_mailbox: merge_executor_mailbox,
            io_controls: IoControls::default(),
        };
        let (merge_split_downloader_mailbox, merge_split_downloader_handler) =
            universe.spawn_builder().spawn(merge_split_downloader);
        let inventory = Inventory::new();
        let merge_operation = inventory.track(MergeOperation::new_merge_operation(splits_to_merge));
        merge_split_downloader_mailbox
            .send_message(merge_operation)
            .await?;
        merge_split_downloader_handler
            .process_pending_and_observe()
            .await;
        let merge_scratches = merge_executor_inbox.drain_for_test();
        assert_eq!(merge_scratches.len(), 1);
        let merge_scratch = merge_scratches
            .into_iter()
            .next()
            .unwrap()
            .downcast::<MergeScratch>()
            .unwrap();
        assert_eq!(merge_scratch.merge_operation.splits_as_slice().len(), 10);
        for split in merge_scratch.merge_operation.splits_as_slice() {
            let split_filename = split_file(split.split_id());
            let split_filepath = merge_scratch
                .downloaded_splits_directory
                .path()
                .join(split_filename);
            assert!(split_filepath.try_exists().unwrap());
        }
        universe.assert_quit().await;
        Ok(())
    }
}
