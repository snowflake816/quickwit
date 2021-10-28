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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use tracing::warn;

#[derive(Clone)]
pub struct KillSwitch {
    alive: Arc<AtomicBool>,
}

impl Default for KillSwitch {
    fn default() -> Self {
        KillSwitch {
            alive: Arc::new(AtomicBool::new(true)),
        }
    }
}

impl KillSwitch {
    pub fn kill(&self) {
        warn!("kill-switch-activated");
        self.alive.store(false, Ordering::Relaxed);
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    pub fn is_dead(&self) -> bool {
        !self.is_alive()
    }
}
#[cfg(test)]
mod tests {
    use super::KillSwitch;

    #[test]
    fn test_kill_switch() {
        let kill_switch = KillSwitch::default();
        assert!(kill_switch.is_alive());
        assert!(!kill_switch.is_dead());
        kill_switch.kill();
        assert!(!kill_switch.is_alive());
        assert!(kill_switch.is_dead());
        kill_switch.kill();
        assert!(!kill_switch.is_alive());
        assert!(kill_switch.is_dead());
    }
}
