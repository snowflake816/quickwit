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

#![allow(clippy::derive_partial_eq_without_eq)]
#![deny(clippy::disallowed_methods)]
#![allow(rustdoc::invalid_html_tags)]

use std::cmp::Ordering;
use std::convert::Infallible;
use std::fmt;

use ::opentelemetry::global;
use ::opentelemetry::propagation::{Extractor, Injector};
use tonic::codegen::http;
use tonic::service::Interceptor;
use tonic::Status;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use ulid::Ulid;

pub mod control_plane;
pub mod indexing;
#[path = "codegen/quickwit/quickwit.metastore.rs"]
pub mod metastore;
#[path = "codegen/quickwit/quickwit.search.rs"]
pub mod search;

pub use metastore::*;
pub use search::*;
pub use sort_by_value::SortValue;
pub use tonic;

pub mod jaeger {
    pub mod api_v2 {
        include!("codegen/jaeger/jaeger.api_v2.rs");
    }
    pub mod storage {
        pub mod v1 {
            include!("codegen/jaeger/jaeger.storage.v1.rs");
        }
    }
}

pub mod opentelemetry {
    #[cfg(not(doctest))]
    pub mod proto {

        pub mod collector {
            pub mod logs {
                pub mod v1 {
                    include!("codegen/opentelemetry/opentelemetry.proto.collector.logs.v1.rs");
                }
            }
            // pub mod metrics {
            //     pub mod v1 {
            //         include!("codegen/opentelemetry/opentelemetry.proto.collector.metrics.v1.rs"
            // );     }
            // }
            pub mod trace {
                pub mod v1 {
                    include!("codegen/opentelemetry/opentelemetry.proto.collector.trace.v1.rs");
                }
            }
        }
        pub mod common {
            pub mod v1 {
                include!("codegen/opentelemetry/opentelemetry.proto.common.v1.rs");
            }
        }
        pub mod logs {
            pub mod v1 {
                include!("codegen/opentelemetry/opentelemetry.proto.logs.v1.rs");
            }
        }
        // pub mod metrics {
        //     pub mod experimental {
        //         include!("codegen/opentelemetry/opentelemetry.proto.metrics.experimental.rs");
        //     }
        //     pub mod v1 {
        //         tonic::include_proto!("codegen/opentelemetry/opentelemetry.proto.metrics.v1");
        //     }
        // }
        pub mod resource {
            pub mod v1 {
                include!("codegen/opentelemetry/opentelemetry.proto.resource.v1.rs");
            }
        }
        pub mod trace {
            pub mod v1 {
                include!("codegen/opentelemetry/opentelemetry.proto.trace.v1.rs");
            }
        }
    }
}

#[macro_use]
extern crate serde;

/// This enum serves as a Rosetta stone of
/// gRPC and Http status code.
///
/// It is voluntarily a restricted subset.
#[derive(Clone, Copy)]
pub enum ServiceErrorCode {
    BadRequest,
    Internal,
    MethodNotAllowed,
    NotFound,
    RateLimited,
    Unavailable,
    UnsupportedMediaType,
    NotSupportedYet, /* Used for API that is available in elasticsearch but is not yet
                      * available in Quickwit. */
}

impl ServiceErrorCode {
    pub fn to_grpc_status_code(self) -> tonic::Code {
        match self {
            ServiceErrorCode::BadRequest => tonic::Code::InvalidArgument,
            ServiceErrorCode::Internal => tonic::Code::Internal,
            ServiceErrorCode::MethodNotAllowed => tonic::Code::InvalidArgument,
            ServiceErrorCode::NotFound => tonic::Code::NotFound,
            ServiceErrorCode::RateLimited => tonic::Code::ResourceExhausted,
            ServiceErrorCode::Unavailable => tonic::Code::Unavailable,
            ServiceErrorCode::UnsupportedMediaType => tonic::Code::InvalidArgument,
            ServiceErrorCode::NotSupportedYet => tonic::Code::Unimplemented,
        }
    }
    pub fn to_http_status_code(self) -> http::StatusCode {
        match self {
            ServiceErrorCode::BadRequest => http::StatusCode::BAD_REQUEST,
            ServiceErrorCode::Internal => http::StatusCode::INTERNAL_SERVER_ERROR,
            ServiceErrorCode::MethodNotAllowed => http::StatusCode::METHOD_NOT_ALLOWED,
            ServiceErrorCode::NotFound => http::StatusCode::NOT_FOUND,
            ServiceErrorCode::RateLimited => http::StatusCode::TOO_MANY_REQUESTS,
            ServiceErrorCode::Unavailable => http::StatusCode::SERVICE_UNAVAILABLE,
            ServiceErrorCode::UnsupportedMediaType => http::StatusCode::UNSUPPORTED_MEDIA_TYPE,
            ServiceErrorCode::NotSupportedYet => http::StatusCode::NOT_IMPLEMENTED,
        }
    }
}

pub trait ServiceError: ToString {
    fn grpc_error(&self) -> tonic::Status {
        let grpc_code = self.status_code().to_grpc_status_code();
        let error_msg = self.to_string();
        tonic::Status::new(grpc_code, error_msg)
    }

    fn status_code(&self) -> ServiceErrorCode;
}

impl ServiceError for Infallible {
    fn status_code(&self) -> ServiceErrorCode {
        unreachable!()
    }
}

pub fn convert_to_grpc_result<T, E: ServiceError>(
    res: Result<T, E>,
) -> Result<tonic::Response<T>, tonic::Status> {
    res.map(tonic::Response::new)
        .map_err(|error| error.grpc_error())
}

impl TryFrom<SearchStreamRequest> for SearchRequest {
    type Error = anyhow::Error;

    fn try_from(search_stream_req: SearchStreamRequest) -> Result<Self, Self::Error> {
        Ok(Self {
            index_id: search_stream_req.index_id,
            query_ast: search_stream_req.query_ast,
            snippet_fields: search_stream_req.snippet_fields,
            start_timestamp: search_stream_req.start_timestamp,
            end_timestamp: search_stream_req.end_timestamp,
            ..Default::default()
        })
    }
}

impl TryFrom<DeleteQuery> for SearchRequest {
    type Error = anyhow::Error;

    fn try_from(delete_query: DeleteQuery) -> anyhow::Result<Self> {
        let index_uid: IndexUid = delete_query.index_uid.into();
        Ok(Self {
            index_id: index_uid.index_id().to_string(),
            query_ast: delete_query.query_ast,
            start_timestamp: delete_query.start_timestamp,
            end_timestamp: delete_query.end_timestamp,
            ..Default::default()
        })
    }
}

impl SearchRequest {
    pub fn time_range(&self) -> impl std::ops::RangeBounds<i64> {
        use std::ops::Bound;
        (
            self.start_timestamp
                .map_or(Bound::Unbounded, Bound::Included),
            self.end_timestamp.map_or(Bound::Unbounded, Bound::Excluded),
        )
    }
}

impl SplitIdAndFooterOffsets {
    pub fn time_range(&self) -> impl std::ops::RangeBounds<i64> {
        use std::ops::Bound;
        (
            self.timestamp_start
                .map_or(Bound::Unbounded, Bound::Included),
            self.timestamp_end.map_or(Bound::Unbounded, Bound::Included),
        )
    }
}

impl fmt::Display for SplitSearchError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "({}, split_id: {})", self.error, self.split_id)
    }
}

/// `MutMetadataMap` used to extract [`tonic::metadata::MetadataMap`] from a request.
pub struct MutMetadataMap<'a>(&'a mut tonic::metadata::MetadataMap);

impl<'a> Injector for MutMetadataMap<'a> {
    /// Sets a key-value pair in the [`MetadataMap`]. No-op if the key or value is invalid.
    fn set(&mut self, key: &str, value: String) {
        if let Ok(metadata_key) = tonic::metadata::MetadataKey::from_bytes(key.as_bytes()) {
            if let Ok(metadata_value) = tonic::metadata::MetadataValue::try_from(&value) {
                self.0.insert(metadata_key, metadata_value);
            }
        }
    }
}

impl<'a> Extractor for MutMetadataMap<'a> {
    /// Gets a value for a key from the MetadataMap.  If the value can't be converted to &str,
    /// returns None.
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|metadata| metadata.to_str().ok())
    }

    /// Collect all the keys from the MetadataMap.
    fn keys(&self) -> Vec<&str> {
        self.0
            .keys()
            .map(|key| match key {
                tonic::metadata::KeyRef::Ascii(v) => v.as_str(),
                tonic::metadata::KeyRef::Binary(v) => v.as_str(),
            })
            .collect::<Vec<_>>()
    }
}

/// [`tonic::service::interceptor::Interceptor`] which injects the span context into
/// [`tonic::metadata::MetadataMap`].
#[derive(Clone, Debug)]
pub struct SpanContextInterceptor;

impl Interceptor for SpanContextInterceptor {
    fn call(&mut self, mut request: tonic::Request<()>) -> Result<tonic::Request<()>, Status> {
        global::get_text_map_propagator(|propagator| {
            propagator.inject_context(
                &tracing::Span::current().context(),
                &mut MutMetadataMap(request.metadata_mut()),
            )
        });
        Ok(request)
    }
}

/// `MetadataMap` extracts OpenTelemetry
/// tracing keys from request's headers.
struct MetadataMap<'a>(&'a tonic::metadata::MetadataMap);

impl<'a> Extractor for MetadataMap<'a> {
    /// Gets a value for a key from the MetadataMap.  If the value can't be converted to &str,
    /// returns None.
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|metadata| metadata.to_str().ok())
    }

    /// Collect all the keys from the MetadataMap.
    fn keys(&self) -> Vec<&str> {
        self.0
            .keys()
            .map(|key| match key {
                tonic::metadata::KeyRef::Ascii(v) => v.as_str(),
                tonic::metadata::KeyRef::Binary(v) => v.as_str(),
            })
            .collect::<Vec<_>>()
    }
}

/// Sets parent span context derived from [`tonic::metadata::MetadataMap`].
pub fn set_parent_span_from_request_metadata(request_metadata: &tonic::metadata::MetadataMap) {
    let parent_cx =
        global::get_text_map_propagator(|prop| prop.extract(&MetadataMap(request_metadata)));
    Span::current().set_parent(parent_cx);
}

/// Index identifiers that uniquely identify not only the index, but also
/// its incarnation allowing to distinguish between deleted and recreated indexes.
/// It is represented as a stiring in index_id:incarnation_id format.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq, Ord, PartialOrd, Hash)]
pub struct IndexUid(String);

impl IndexUid {
    /// Creates a new index uid form index_id and incarnation_id
    pub fn new(index_id: impl Into<String>) -> Self {
        Self::from_parts(index_id, Ulid::new().to_string())
    }

    pub fn from_parts(index_id: impl Into<String>, incarnation_id: impl Into<String>) -> Self {
        let incarnation_id = incarnation_id.into();
        let index_id = index_id.into();
        if incarnation_id.is_empty() {
            Self(index_id)
        } else {
            Self(format!("{index_id}:{incarnation_id}"))
        }
    }

    pub fn index_id(&self) -> &str {
        self.0.split(':').next().unwrap()
    }

    pub fn incarnation_id(&self) -> &str {
        if let Some(incarnation_id) = self.0.split(':').nth(1) {
            incarnation_id
        } else {
            ""
        }
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<IndexUid> for String {
    fn from(val: IndexUid) -> Self {
        val.0
    }
}

impl fmt::Display for IndexUid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for IndexUid {
    fn from(index_uid: String) -> Self {
        IndexUid(index_uid)
    }
}

// !!! Disclaimer !!!
//
// Prost imposes the PartialEq derived implementation.
// This is terrible because this means Eq, PartialEq are not really in line with Ord's
// implementation. if in presence of NaN.
impl Eq for SortByValue {}
impl Copy for SortByValue {}
impl From<SortValue> for SortByValue {
    fn from(sort_value: SortValue) -> Self {
        SortByValue {
            sort_value: Some(sort_value),
        }
    }
}

impl Copy for SortValue {}
impl Eq for SortValue {}

impl Ord for SortValue {
    fn cmp(&self, other: &Self) -> Ordering {
        // We make sure to end up with a total order.
        match (*self, *other) {
            // Same types.
            (SortValue::U64(left), SortValue::U64(right)) => left.cmp(&right),
            (SortValue::I64(left), SortValue::I64(right)) => left.cmp(&right),
            (SortValue::F64(left), SortValue::F64(right)) => {
                if left.is_nan() {
                    if right.is_nan() {
                        Ordering::Equal
                    } else {
                        Ordering::Less
                    }
                } else if right.is_nan() {
                    Ordering::Greater
                } else {
                    left.partial_cmp(&right).unwrap_or(Ordering::Less)
                }
            }
            (SortValue::Boolean(left), SortValue::Boolean(right)) => left.cmp(&right),
            // We half the logic by making sure we keep
            // the "stronger" type on the left.
            (SortValue::U64(left), SortValue::I64(right)) => {
                if left > i64::MAX as u64 {
                    return Ordering::Greater;
                }
                (left as i64).cmp(&right)
            }
            (SortValue::F64(left), _) if left.is_nan() => Ordering::Less,
            (SortValue::F64(left), SortValue::U64(right)) => {
                left.partial_cmp(&(right as f64)).unwrap_or(Ordering::Less)
            }
            (SortValue::F64(left), SortValue::I64(right)) => {
                left.partial_cmp(&(right as f64)).unwrap_or(Ordering::Less)
            }
            (SortValue::Boolean(left), right) => SortValue::U64(left as u64).cmp(&right),
            (left, right) => right.cmp(&left).reverse(),
        }
    }
}

impl PartialOrd for SortValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<E: fmt::Debug + ServiceError> ServiceError for quickwit_actors::AskError<E> {
    fn status_code(&self) -> ServiceErrorCode {
        match self {
            quickwit_actors::AskError::MessageNotDelivered => ServiceErrorCode::Internal,
            quickwit_actors::AskError::ProcessMessageError => ServiceErrorCode::Internal,
            quickwit_actors::AskError::ErrorReply(err) => err.status_code(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_index_uid_parsing() {
        assert_eq!("foo", IndexUid::from("foo".to_string()).index_id());
        assert_eq!("foo", IndexUid::from("foo:bar".to_string()).index_id());
        assert_eq!("", IndexUid::from("foo".to_string()).incarnation_id());
        assert_eq!(
            "bar",
            IndexUid::from("foo:bar".to_string()).incarnation_id()
        );
    }

    #[test]
    fn test_index_uid_roundtrip() {
        assert_eq!("foo", IndexUid::from("foo".to_string()).to_string());
        assert_eq!("foo:bar", IndexUid::from("foo:bar".to_string()).to_string());
    }

    #[test]
    fn test_index_uid_roundtrip_using_parts() {
        assert_eq!("foo", index_uid_roundtrip_using_parts("foo"));
        assert_eq!("foo:bar", index_uid_roundtrip_using_parts("foo:bar"));
    }

    fn index_uid_roundtrip_using_parts(index_uid: &str) -> String {
        let index_uid = IndexUid::from(index_uid.to_string());
        let index_id = index_uid.index_id();
        let incarnation_id = index_uid.incarnation_id();
        let index_uid_from_parts = IndexUid::from_parts(index_id, incarnation_id);
        index_uid_from_parts.to_string()
    }
}
