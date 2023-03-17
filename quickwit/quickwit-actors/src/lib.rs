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

#![deny(clippy::disallowed_methods)]

//! quickwit-actors is a simplified actor framework for quickwit.
//!
//! It solves the following problem:
//! - have sync and async tasks communicate together.
//! - make these task observable
//! - make these task modular and testable
//! - detect when some task is stuck and does not progress anymore

use std::fmt;

use quickwit_proto::{ServiceError, ServiceErrorCode};
use tokio::time::Duration;
mod actor;
mod actor_context;
mod actor_handle;
mod actor_state;
#[doc(hidden)]
pub mod channel_with_priority;
mod command;
mod envelope;
mod mailbox;
mod observation;
mod registry;
pub(crate) mod scheduler;
mod spawn_builder;
mod supervisor;

pub use scheduler::{start_scheduler, SchedulerClient};

#[cfg(test)]
pub(crate) mod tests;
mod universe;

pub use actor::{Actor, ActorExitStatus, DeferableReplyHandler, Handler};
pub use actor_handle::{ActorHandle, Health, Healthz, Supervisable};
pub use command::Command;
pub use observation::{Observation, ObservationType};
use quickwit_common::KillSwitch;
pub use spawn_builder::SpawnContext;
use thiserror::Error;
pub use universe::Universe;

pub use self::actor_context::ActorContext;
pub use self::actor_state::ActorState;
pub use self::channel_with_priority::{QueueCapacity, RecvError, SendError, TrySendError};
pub use self::mailbox::{Inbox, Mailbox};
pub use self::registry::ActorObservation;
pub use self::supervisor::{Supervisor, SupervisorState};

/// Heartbeat used to verify that actors are progressing.
///
/// If an actor does not advertise a progress within an interval of duration `HEARTBEAT`,
/// its supervisor will consider it as blocked and will proceed to kill it, as well
/// as all of the actors all the actors that share the killswitch.
pub const HEARTBEAT: Duration = if cfg!(any(test, feature = "testsuite")) {
    // Right now some unit test end when we detect that a
    // pipeline has terminated, which can require waiting
    // for a heartbeat.
    //
    // We use a shorter heartbeat to reduce the time running unit tests.
    Duration::from_millis(500)
} else {
    Duration::from_secs(3)
};

/// Time we accept to wait for a new observation.
///
/// Once this time is elapsed, we just return the last observation.
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(3);

/// Error that occurred while calling `ActorContext::ask(..)` or `Universe::ask`
#[derive(Error, Debug)]
pub enum AskError<E: fmt::Debug> {
    #[error("Message could not be delivered")]
    MessageNotDelivered,
    #[error("Error while the message was being processed.")]
    ProcessMessageError,
    #[error("The handler returned an error: `{0:?}`.")]
    ErrorReply(#[from] E),
}

impl<E: fmt::Debug + ServiceError> ServiceError for AskError<E> {
    fn status_code(&self) -> ServiceErrorCode {
        match self {
            AskError::MessageNotDelivered => ServiceErrorCode::Internal,
            AskError::ProcessMessageError => ServiceErrorCode::Internal,
            AskError::ErrorReply(err) => err.status_code(),
        }
    }
}
