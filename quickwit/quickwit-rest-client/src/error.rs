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

use reqwest::StatusCode;
use serde::Deserialize;
use thiserror::Error;

pub static DEFAULT_ADDRESS: &str = "http://127.0.0.1:7280";
pub static DEFAULT_CONTENT_TYPE: &str = "application/json";

#[derive(Error, Debug)]
pub enum Error {
    // Error returned by Quickwit server.
    #[error("Api error: {0}")]
    Api(#[from] ApiError),
    // Error returned by reqwest lib.
    #[error("Reqwest client lib error: {0}")]
    Client(#[from] reqwest::Error),
    // IO Error returned by tokio lib.
    #[error("IO error: {0}")]
    Io(#[from] tokio::io::Error),
    // Internal error returned by quickwit client lib.
    #[error("Internal Quickwit client error: {0}")]
    Internal(String),
    // Json serialization/deserialization error.
    #[error("Serde JSON error: {0}")]
    Json(#[from] serde_json::error::Error),
    // Error returned by url lib when parsing a string.
    #[error("Url parsing error: {0}")]
    UrlParse(String),
}

impl Error {
    pub fn status_code(&self) -> Option<StatusCode> {
        match &self {
            Error::Client(err) => err.status(),
            Error::Api(err) => Some(err.status),
            _ => None,
        }
    }
}

#[derive(Error, Debug)]
pub struct ApiError {
    pub error: Option<String>,
    pub status: StatusCode,
}

// Implement `Display` for `ApiError`.
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        if let Some(error) = &self.error {
            write!(f, "(status={}, error={})", self.status, error)
        } else {
            write!(f, "(status={})", self.status)
        }
    }
}

#[derive(Deserialize)]
pub(crate) struct ErrorResponsePayload {
    pub error: String,
}
