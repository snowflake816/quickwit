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

use std::collections::HashSet;
use std::env;
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Represents the payload of the request sent with telemetry requests.
#[derive(Debug, Serialize, Deserialize)]
pub struct TelemetryPayload {
    /// Client information. See details in `[ClientInformation]`.
    pub client_info: ClientInfo,
    /// Quickwit information. See details in `[QuickwitInfo]`.
    pub quickwit_info: QuickwitTelemetryInfo,
    pub events: Vec<EventWithTimestamp>,
    /// Represents the number of events that where drops due to the
    /// combination of the `TELEMETRY_PUSH_COOLDOWN` and `MAX_EVENT_IN_QUEUE`.
    pub num_dropped_events: usize,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EventWithTimestamp {
    /// Unix time in seconds.
    pub unixtime: u64,
    /// Telemetry event.
    #[serde(flatten)]
    pub event: TelemetryEvent,
}

/// Returns the number of seconds elapsed since UNIX_EPOCH.
///
/// If the system clock is set before 1970, returns 0.
fn unixtime() -> u64 {
    match UNIX_EPOCH.elapsed() {
        Ok(duration) => duration.as_secs(),
        Err(_) => 0u64,
    }
}

impl From<TelemetryEvent> for EventWithTimestamp {
    fn from(event: TelemetryEvent) -> Self {
        EventWithTimestamp {
            unixtime: unixtime(),
            event,
        }
    }
}

/// Represents a Telemetry Event send to Quickwit's telemetry server for usage information.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
#[serde(rename_all = "snake_case")]
pub enum TelemetryEvent {
    RunCommand,
    /// EndCommand (with the return code).
    EndCommand {
        return_code: i32,
    },
    /// Event sent every 12h to signal the server is running.
    Running,
    /// UI index.html was requested.
    UiIndexPageLoad,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClientInfo {
    session_uuid: uuid::Uuid,
    os: String,
    arch: String,
    hashed_host_username: String,
    kubernetes: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuickwitTelemetryInfo {
    pub version: String,
    pub services: HashSet<String>,
    pub features: HashSet<QuickwitFeature>,
}

impl QuickwitTelemetryInfo {
    pub fn new(services: HashSet<String>, features: HashSet<QuickwitFeature>) -> Self {
        Self {
            features,
            version: env!("CARGO_PKG_VERSION").to_string(),
            services,
        }
    }
}

impl Default for QuickwitTelemetryInfo {
    fn default() -> Self {
        Self {
            features: HashSet::new(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            services: HashSet::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum QuickwitFeature {
    FileBackedMetastore,
    Jaeger,
    Otlp,
    PostgresqMetastore,
    AwsLambda,
}

fn hashed_host_username() -> String {
    let hostname = hostname::get()
        .map(|hostname| hostname.to_string_lossy().to_string())
        .unwrap_or_default();
    let username = username::get_user_name().unwrap_or_default();
    let hashed_value = format!("{hostname}:{username}");
    let digest = md5::compute(hashed_value.as_bytes());
    format!("{digest:x}")
}

impl Default for ClientInfo {
    fn default() -> ClientInfo {
        ClientInfo {
            session_uuid: Uuid::new_v4(),
            os: env::consts::OS.to_string(),
            arch: env::consts::ARCH.to_string(),
            hashed_host_username: hashed_host_username(),
            kubernetes: std::env::var_os("KUBERNETES_SERVICE_HOST").is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json;

    use super::{EventWithTimestamp, TelemetryEvent};

    #[test]
    fn test_serialize_payload_as_expected() {
        let event = EventWithTimestamp {
            unixtime: 0,
            event: TelemetryEvent::EndCommand { return_code: 0 },
        };
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(
            json,
            r#"{"unixtime":0,"type":"end_command","return_code":0}"#
        );
    }
}
