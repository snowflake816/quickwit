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

use std::sync::atomic::Ordering;

use serde::Serialize;

use crate::actors::{PublisherCounters, UploaderCounters};

/// A Struct to hold all merge statistical data.
#[derive(Clone, Debug, Default, Serialize)]
pub struct MergeStatistics {
    /// Number of uploaded splits
    pub num_uploaded_splits: u64,
    /// Number of published splits
    pub num_published_splits: u64,
    /// Pipeline generation.
    pub generation: usize,
    /// Number of successive pipeline spawn attempts.
    pub num_spawn_attempts: usize,
    /// Number of merges currently in progress.
    pub num_ongoing_merges: usize,
}

impl MergeStatistics {
    pub fn add_actor_counters(
        mut self,
        uploader_counters: &UploaderCounters,
        publisher_counters: &PublisherCounters,
    ) -> Self {
        self.num_uploaded_splits += uploader_counters.num_uploaded_splits.load(Ordering::SeqCst);
        self.num_published_splits += publisher_counters.num_published_splits;
        self
    }

    pub fn set_num_spawn_attempts(mut self, num_spawn_attempts: usize) -> Self {
        self.num_spawn_attempts = num_spawn_attempts;
        self
    }

    pub fn set_generation(mut self, generation: usize) -> Self {
        self.generation = generation;
        self
    }

    pub fn set_ongoing_merges(mut self, n: usize) -> Self {
        self.num_ongoing_merges = n;
        self
    }
}
