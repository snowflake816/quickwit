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

use quickwit_config::SourceConfig;
use quickwit_proto::indexing::IndexingPipelineId;
use quickwit_proto::types::PipelineUid;

use crate::actors::MergePipelineId;

#[derive(Clone, Debug)]
pub struct SpawnPipeline {
    pub index_id: String,
    pub source_config: SourceConfig,
    pub pipeline_uid: PipelineUid,
}

#[derive(Clone, Debug)]
pub struct ShutdownPipelines {
    pub index_id: String,
    pub source_id: Option<String>,
    // TODO
    // pub pipeline_ord: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct ShutdownPipeline {
    pub pipeline_id: IndexingPipelineId,
}

/// Detaches a pipeline from the indexing service. The pipeline is no longer managed by the
/// server. This is mostly useful for ad-hoc indexing pipelines launched with `quickwit index
/// ingest ..` and testing.
#[derive(Debug)]
pub struct DetachIndexingPipeline {
    pub pipeline_id: IndexingPipelineId,
}

#[derive(Debug)]
/// Detaches a merge pipeline from the indexing service. The pipeline is no longer managed by the
/// server. This is mostly useful for preventing the server killing an existing merge pipeline
/// if a indexing pipeline is detached.
pub struct DetachMergePipeline {
    pub pipeline_id: MergePipelineId,
}

#[derive(Debug)]
pub struct ObservePipeline {
    pub pipeline_id: IndexingPipelineId,
}
