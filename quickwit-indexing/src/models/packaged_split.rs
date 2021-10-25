// Copyright (C) 2021 Quickwit, Inc.
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
use std::ops::{Range, RangeInclusive};
use std::time::Instant;

use quickwit_metastore::checkpoint::CheckpointDelta;

use crate::models::ScratchDirectory;

#[derive(Debug)]
pub struct PackagedSplit {
    pub split_id: String,
    pub replaced_split_ids: Vec<String>,
    pub index_id: String,
    pub checkpoint_deltas: Vec<CheckpointDelta>,
    pub time_range: Option<RangeInclusive<i64>>,
    pub size_in_bytes: u64,
    pub footer_offsets: Range<u64>,
    pub split_scratch_directory: ScratchDirectory,
    pub num_docs: u64,
    pub demux_num_ops: usize,
    pub tags: HashSet<String>,
    pub split_date_of_birth: Instant,
}

#[derive(Debug)]
pub struct PackagedSplitBatch {
    splits: Vec<PackagedSplit>,
}

impl PackagedSplitBatch {
    /// Instantiate a consistent [`PackagedSplitBatch`] that
    /// satisfies two constraints:
    /// - a batch must have at least one split
    /// - all splits must be on the same `index_id`.
    pub fn new(splits: Vec<PackagedSplit>) -> Self {
        assert!(!splits.is_empty());
        assert_eq!(
            splits
                .iter()
                .map(|split| split.index_id.clone())
                .collect::<HashSet<_>>()
                .len(),
            1,
            "All splits must be on the same `index_id`."
        );
        Self { splits }
    }

    pub fn index_id(&self) -> String {
        self.splits
            .get(0)
            .map(|split| split.index_id.clone())
            .unwrap()
    }

    pub fn split_ids(&self) -> Vec<String> {
        self.splits
            .iter()
            .map(|split| split.split_id.clone())
            .collect::<Vec<_>>()
    }
}

impl IntoIterator for PackagedSplitBatch {
    type Item = PackagedSplit;

    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.splits.into_iter()
    }
}
