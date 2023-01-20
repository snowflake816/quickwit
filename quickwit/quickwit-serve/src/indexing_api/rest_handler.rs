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

use quickwit_actors::Mailbox;
use quickwit_indexing::actors::IndexingService;
use quickwit_indexing::models::Observe;
use warp::{Filter, Rejection};

use crate::format::Format;
use crate::require;

#[derive(utoipa::OpenApi)]
#[openapi(paths(indexing_endpoint))]
pub struct IndexingApi;

#[utoipa::path(
    get,
    tag = "Indexing",
    path = "/indexing",
    responses(
        (status = 200, description = "Successfully observed indexing pipelines.", body = IndexingStatistics)
    ),
)]
/// Observe Indexing Pipeline
async fn indexing_endpoint(indexing_service_mailbox: Mailbox<IndexingService>) -> impl warp::Reply {
    let obs = indexing_service_mailbox.ask(Observe).await;
    Format::PrettyJson.make_rest_reply_non_serializable_error(obs)
}

fn indexing_get_filter() -> impl Filter<Extract = (), Error = Rejection> + Clone {
    warp::path!("indexing").and(warp::get())
}

pub fn indexing_get_handler(
    indexing_service_mailbox_opt: Option<Mailbox<IndexingService>>,
) -> impl Filter<Extract = (impl warp::Reply,), Error = Rejection> + Clone {
    indexing_get_filter()
        .and(require(indexing_service_mailbox_opt))
        .then(indexing_endpoint)
}
