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

use quickwit_query::InvalidQuery;
use tantivy::schema::DocParsingError as TantivyDocParsingError;
use thiserror::Error;

/// Failed to parse query.
#[derive(Error, Debug)]
#[allow(missing_docs)]
pub enum QueryParserError {
    #[error("invalid json: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("invalid query: {0}")]
    InvalidQuery(#[from] InvalidQuery),
    #[error("invalid default search field: `{field_name}` {cause}")]
    InvalidDefaultField {
        cause: &'static str,
        field_name: String,
    },
    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

/// Error that may happen when parsing
/// a document from JSON.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum DocParsingError {
    /// The provided string is not a syntactically valid JSON object.
    #[error("the provided string is not a syntactically valid JSON object: {0}")]
    NotJsonObject(String),
    /// One of the value could not be parsed.
    #[error("the field `{0}` could not be parsed: {1}")]
    ValueError(String, String),
    /// The json-document contains a field that is not declared in the schema.
    #[error("the document contains a field that is not declared in the schema: {0:?}")]
    NoSuchFieldInSchema(String),
    /// The document contains a array of values but a single value is expected.
    #[error("the document contains an array of values but a single value is expected: {0:?}")]
    MultiValuesNotSupported(String),
    /// The document does not contain a field that is required.
    #[error("the document must contain field {0:?}")]
    RequiredField(String),
}

impl From<TantivyDocParsingError> for DocParsingError {
    fn from(value: TantivyDocParsingError) -> Self {
        match value {
            TantivyDocParsingError::InvalidJson(text) => DocParsingError::NoSuchFieldInSchema(text),
            TantivyDocParsingError::ValueError(text, error) => {
                DocParsingError::ValueError(text, format!("{error:?}"))
            }
        }
    }
}
