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

use anyhow::anyhow;
use anyhow::Context;
use ulid::Ulid;
mod quickwit;
mod quickwit_indexing_api;
mod quickwit_metastore_api;

pub mod indexing_api {
    pub use crate::quickwit_indexing_api::*;
}

pub mod metastore_api {
    pub use crate::quickwit_metastore_api::*;
}

pub mod jaeger {
    pub mod api_v2 {
        include!("jaeger.api_v2.rs");
    }
    pub mod storage {
        pub mod v1 {
            include!("jaeger.storage.v1.rs");
        }
    }
}

pub mod opentelemetry {
    #[cfg(not(doctest))]
    pub mod proto {

        pub mod collector {
            pub mod logs {
                pub mod v1 {
                    include!("opentelemetry.proto.collector.logs.v1.rs");
                }
            }
            // pub mod metrics {
            //     pub mod v1 {
            //         include!("opentelemetry.proto.collector.metrics.v1.rs");
            //     }
            // }
            pub mod trace {
                pub mod v1 {
                    include!("opentelemetry.proto.collector.trace.v1.rs");
                }
            }
        }
        pub mod common {
            pub mod v1 {
                include!("opentelemetry.proto.common.v1.rs");
            }
        }
        pub mod logs {
            pub mod v1 {
                include!("opentelemetry.proto.logs.v1.rs");
            }
        }
        // pub mod metrics {
        //     pub mod experimental {
        //         include!("opentelemetry.proto.metrics.experimental.rs");
        //     }
        //     pub mod v1 {
        //         tonic::include_proto!("opentelemetry.proto.metrics.v1");
        //     }
        // }
        pub mod resource {
            pub mod v1 {
                include!("opentelemetry.proto.resource.v1.rs");
            }
        }
        pub mod trace {
            pub mod v1 {
                include!("opentelemetry.proto.trace.v1.rs");
            }
        }
    }
}

#[macro_use]
extern crate serde;

use std::convert::Infallible;
use std::fmt;

use ::opentelemetry::global;
use ::opentelemetry::propagation::Extractor;
use ::opentelemetry::propagation::Injector;
pub use quickwit::*;
use quickwit_indexing_api::IndexingTask;
use quickwit_metastore_api::DeleteQuery;
pub use tonic;
use tonic::codegen::http;
use tonic::service::Interceptor;
use tonic::Status;
use tracing::Span;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use quickwit_query::query_ast::QueryAst;

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
    NotSupportedYet, //< Used for API that is available in elasticsearch but is not yet available in Quickwit.
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
            .. Default::default()
        })
    }
}

impl TryFrom<DeleteQuery> for SearchRequest {
    type Error = anyhow::Error;

    fn try_from(delete_query: DeleteQuery) -> anyhow::Result<Self> {
        Ok(Self {
            index_id: delete_query.index_id,
            query_ast: delete_query.query_ast,
            start_timestamp: delete_query.start_timestamp,
            end_timestamp: delete_query.end_timestamp,
            ..Default::default()
        })
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

/// [`tonic::service::interceptor::Interceptor`] which injects the span context into [`tonic::metadata::MetadataMap`].
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

impl ToString for IndexingTask {
    fn to_string(&self) -> String {
        format!(
            "{}:{}:{}",
            self.index_id, self.source_id, self.incarnation_id
        )
    }
}

impl TryFrom<&str> for IndexingTask {
    type Error = anyhow::Error;

    fn try_from(index_task_str: &str) -> anyhow::Result<IndexingTask> {
        let mut iter = index_task_str.split(':');
        let index_id = iter.next().ok_or_else(|| {
            anyhow!(
                "Invalid index task format, cannot find index_id in `{}`",
                index_task_str
            )
        })?;
        let source_id = iter.next().ok_or_else(|| {
            anyhow!(
                "Invalid index task format, cannot find source_id in `{}`",
                index_task_str
            )
        })?;
        let incarnation_id = if let Some(incarnation_id_str) = iter.next() {
            Ulid::from_string(incarnation_id_str).with_context(|| {
                format!(
                    "Invalid index task format, cannot parse incarnation_id in `{}`",
                    index_task_str
                )
            })?
        } else {
            Ulid::default()
        };
        Ok(IndexingTask {
            index_id: index_id.to_string(),
            source_id: source_id.to_string(),
            incarnation_id: incarnation_id.to_string(),
        })
    }
}


/// Creates a query ast json by parsing a user query.
///
/// The resulting query does not include `UserInputQuery` nodes.
/// The resolution assumes that there are no default search fields
/// in the doc mapper.
///
/// # Panics
///
/// Panics if the user text is invalid.
pub fn qast_helper(
    user_text: &str,
    default_fields: &[&'static str]
) -> String {
    let default_fields: Vec<String> = default_fields.iter().map(|default_field| default_field.to_string()).collect();
    let ast: QueryAst = query_ast_from_user_text(user_text, Some(default_fields))
        .parse_user_query(&[])
        .expect("Invalid user query");
    serde_json::to_string(&ast).expect("Failed to serialize ast")
}

/// Creates a QueryAST with a single UserInputQuery node.
///
/// Disclaimer:
/// At this point the query has not been parsed.
///
/// The actual parsing is meant to happen on a root node,
/// `default_fields` can be passed to decide which field should be search
/// if not specified specifically in the user query (e.g. hello as opposed to "body:hello").
///
/// If it is not supplied, the docmapper search fields are meant to be used.
///
/// If no boolean operator is specified, the default is `AND` (contrary to the Elasticsearch default).
pub fn query_ast_from_user_text(
    user_text: &str,
    default_fields: Option<Vec<String>>,
) -> QueryAst {
    quickwit_query::query_ast::UserInputQuery {
        user_text: user_text.to_string(),
        default_fields,
        default_operator: quickwit_query::DefaultOperator::And,
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ulid::Ulid;

    #[test]
    fn test_indexing_task_serialization() {
        let incarnation_id = Ulid::new();
        let original = IndexingTask {
            index_id: "test-index".to_string(),
            source_id: "test-source".to_string(),
            incarnation_id: incarnation_id.to_string(),
        };

        let serialized = original.to_string();
        let deserialized: IndexingTask = serialized.as_str().try_into().unwrap();
        assert_eq!(original, deserialized);
    }

    #[test]
    fn test_indexing_task_serialization_bwc() {
        assert_eq!(
            IndexingTask::try_from("foo:bar").unwrap(),
            IndexingTask {
                index_id: "foo".to_string(),
                source_id: "bar".to_string(),
                incarnation_id: Ulid::default().to_string(),
            }
        );
    }

    #[test]
    fn test_indexing_task_serialization_errors() {
        assert_eq!(
            "Invalid index task format, cannot find source_id in ``",
            IndexingTask::try_from("").unwrap_err().to_string()
        );
        assert_eq!(
            "Invalid index task format, cannot find source_id in `foo`",
            IndexingTask::try_from("foo").unwrap_err().to_string()
        );
        assert_eq!(
            "Invalid index task format, cannot parse incarnation_id in `foo:bar:baz`",
            IndexingTask::try_from("foo:bar:baz")
                .unwrap_err()
                .to_string()
        );
    }

    #[test]
    fn test_query_ast_from_user_text_default_as_and() {
        let ast = query_ast_from_user_text("hello you", None);
        let QueryAst::UserInput(input_query) = ast else { panic!() };
        assert_eq!(input_query.default_operator, quickwit_query::DefaultOperator::And);
    }
}
