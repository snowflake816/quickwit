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

mod args;
mod format;
mod metrics;

mod grpc;
mod rest;

mod cluster_api;
mod health_check_api;
mod index_api;
mod indexing_api;
mod ingest_api;
mod node_info_handler;
mod search_api;
#[cfg(test)]
mod test_utils;
#[cfg(test)]
mod tests;
mod ui_handler;

use std::collections::HashSet;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use anyhow::anyhow;
use format::Format;
use quickwit_actors::{Mailbox, Universe};
use quickwit_cluster::{Cluster, ClusterMember, QuickwitService};
use quickwit_common::uri::Uri;
use quickwit_config::QuickwitConfig;
use quickwit_core::IndexService;
use quickwit_indexing::actors::IndexingService;
use quickwit_indexing::start_indexing_service;
use quickwit_ingest_api::{start_ingest_api_service, IngestApiService};
use quickwit_janitor::actors::JanitorService;
use quickwit_janitor::start_janitor_service;
use quickwit_metastore::{quickwit_metastore_uri_resolver, Metastore, MetastoreGrpcClient};
use quickwit_search::{start_searcher_service, SearchService};
use quickwit_storage::quickwit_storage_uri_resolver;
use serde::{Deserialize, Serialize};
use tracing::error;
use warp::{Filter, Rejection};

pub use crate::args::ServeArgs;
pub use crate::metrics::SERVE_METRICS;
#[cfg(test)]
use crate::rest::recover_fn;

const READYNESS_REPORTING_INTERVAL: Duration = if cfg!(any(test, feature = "testsuite")) {
    Duration::from_millis(25)
} else {
    Duration::from_secs(10)
};

struct QuickwitServices {
    pub config: Arc<QuickwitConfig>,
    pub build_info: Arc<QuickwitBuildInfo>,
    pub cluster: Arc<Cluster>,
    pub metastore: Arc<dyn Metastore>,
    /// We do have a search service even on nodes that are not running `search`.
    /// It is only used to serve the rest API calls and will only execute
    /// the root requests.
    pub search_service: Arc<dyn SearchService>,
    pub indexer_service: Option<Mailbox<IndexingService>>,
    #[allow(dead_code)] // TODO remove
    pub janitor_service: Option<Mailbox<JanitorService>>,
    pub ingest_api_service: Option<Mailbox<IngestApiService>>,
    pub index_service: Arc<IndexService>,
    pub services: HashSet<QuickwitService>,
}

fn has_node_with_metastore_service(members: &[ClusterMember]) -> bool {
    members.iter().any(|member| {
        member
            .available_services
            .contains(&QuickwitService::Metastore)
    })
}

pub async fn serve_quickwit(
    config: QuickwitConfig,
    services: &HashSet<QuickwitService>,
) -> anyhow::Result<()> {
    let storage_resolver = quickwit_storage_uri_resolver().clone();
    let cluster = quickwit_cluster::start_cluster_service(&config, services).await?;

    // Instanciate either a file-backed or postgresql [`Metastore`] if the node runs a `Metastore`
    // service, else instanciate a [`MetastoreGrpcClient`].
    let metastore: Arc<dyn Metastore> = if services.contains(&QuickwitService::Metastore) {
        quickwit_metastore_uri_resolver()
            .resolve(&config.metastore_uri)
            .await?
    } else {
        // Wait 10 seconds for nodes running a `Metastore` service.
        cluster
            .wait_for_members(has_node_with_metastore_service, Duration::from_secs(10))
            .await
            .map_err(|_| {
                error!("No metastore service found among cluster members, stopping server.");
                anyhow!(
                    "Failed to start server: no metastore service was found among cluster \
                     members. Try running Quickwit with additional metastore service `quickwit \
                     run --service metastore`."
                )
            })?;
        let metastore_client = MetastoreGrpcClient::create_and_update_from_members(
            &cluster.members(),
            cluster.member_change_watcher(),
        )
        .await?;
        Arc::new(metastore_client)
    };

    let indexes = metastore
        .list_indexes_metadatas()
        .await?
        .into_iter()
        .map(|index| (index.index_id, index.index_uri));

    check_is_configured_for_cluster(&config.peer_seeds, &config.metastore_uri, indexes)?;

    tokio::spawn(node_readyness_reporting_task(
        cluster.clone(),
        metastore.clone(),
    ));

    let universe = Universe::new();

    let (ingest_api_service, indexer_service) = if services.contains(&QuickwitService::Indexer) {
        let ingest_api_service = start_ingest_api_service(&universe, &config.data_dir_path).await?;
        // TODO: Move to indexer config?
        let enable_ingest_api = true;
        let indexing_service = start_indexing_service(
            &universe,
            &config,
            metastore.clone(),
            storage_resolver.clone(),
            enable_ingest_api,
        )
        .await?;
        (Some(ingest_api_service), Some(indexing_service))
    } else {
        (None, None)
    };

    let janitor_service = if services.contains(&QuickwitService::Janitor) {
        let janitor_service = start_janitor_service(
            &universe,
            &config,
            metastore.clone(),
            storage_resolver.clone(),
        )
        .await?;
        Some(janitor_service)
    } else {
        None
    };

    let search_service: Arc<dyn SearchService> = start_searcher_service(
        &config,
        metastore.clone(),
        storage_resolver.clone(),
        cluster.clone(),
    )
    .await?;

    // Always instantiate index management service.
    let index_service = Arc::new(IndexService::new(
        metastore.clone(),
        storage_resolver,
        config.default_index_root_uri.clone(),
    ));

    let grpc_listen_addr = config.grpc_listen_addr;
    let rest_listen_addr = config.rest_listen_addr;

    let quickwit_services = QuickwitServices {
        config: Arc::new(config),
        build_info: Arc::new(build_quickwit_build_info()),
        cluster,
        metastore,
        search_service,
        indexer_service,
        janitor_service,
        ingest_api_service,
        index_service,
        services: services.clone(),
    };
    let grpc_server = grpc::start_grpc_server(grpc_listen_addr, &quickwit_services);
    let rest_server = rest::start_rest_server(rest_listen_addr, &quickwit_services);
    tokio::try_join!(grpc_server, rest_server)?;
    Ok(())
}

fn require<T: Clone + Send>(
    val_opt: Option<T>,
) -> impl Filter<Extract = (T,), Error = Rejection> + Clone {
    warp::any().and_then(move || {
        let val_opt_clone = val_opt.clone();
        async move {
            if let Some(val) = val_opt_clone {
                Ok(val)
            } else {
                Err(warp::reject())
            }
        }
    })
}

fn with_arg<T: Clone + Send>(arg: T) -> impl Filter<Extract = (T,), Error = Infallible> + Clone {
    warp::any().map(move || arg.clone())
}

/// Reports node readyness to chitchat cluster every 10 seconds (25 ms for tests).
async fn node_readyness_reporting_task(cluster: Arc<Cluster>, metastore: Arc<dyn Metastore>) {
    let mut interval = tokio::time::interval(READYNESS_REPORTING_INTERVAL);
    loop {
        interval.tick().await;
        let node_ready = metastore.check_connectivity().await.is_ok();
        cluster.set_self_node_ready(node_ready).await;
    }
}

/// Checks if the conditions required to smoothly run a Quickwit cluster are met.
/// Currently we don't allow cluster feature upon using:
/// - A FileBacked metastore
/// - A FileStorage
fn check_is_configured_for_cluster(
    peer_seeds: &[String],
    metastore_uri: &Uri,
    mut indexes: impl Iterator<Item = (String, Uri)>,
) -> anyhow::Result<()> {
    if peer_seeds.is_empty() {
        return Ok(());
    }
    if metastore_uri.protocol().is_file() || metastore_uri.protocol().is_s3() {
        anyhow::bail!(
            "Quickwit cannot run in cluster mode with a file-backed metastore. Please, use a \
             PostgreSQL metastore instead."
        );
    }
    if let Some((index_id, index_uri)) =
        indexes.find(|(_, index_uri)| index_uri.protocol().is_file())
    {
        anyhow::bail!(
            "Quickwit cannot run in cluster mode with an index whose data is stored on a local \
             file system. Index URI for index `{}` is `{}`.",
            index_id,
            index_uri,
        );
    }
    Ok(())
}

#[derive(Serialize, Deserialize, Clone, Eq, PartialEq, Debug)]
pub struct QuickwitBuildInfo {
    pub commit_version_tag: &'static str,
    pub cargo_pkg_version: &'static str,
    pub cargo_build_target: &'static str,
    pub commit_short_hash: &'static str,
    pub commit_date: &'static str,
    pub version: &'static str,
}

/// Builds QuickwitBuildInfo from env variables.
pub fn build_quickwit_build_info() -> QuickwitBuildInfo {
    let commit_version_tag = env!("QW_COMMIT_VERSION_TAG");
    let cargo_pkg_version = env!("CARGO_PKG_VERSION");
    let version = if commit_version_tag == "none" {
        // concat macro only accepts literals.
        concat!(env!("CARGO_PKG_VERSION"), "-nightly")
    } else {
        cargo_pkg_version
    };
    QuickwitBuildInfo {
        commit_version_tag,
        cargo_pkg_version,
        cargo_build_target: env!("CARGO_BUILD_TARGET"),
        commit_short_hash: env!("QW_COMMIT_SHORT_HASH"),
        commit_date: env!("QW_COMMIT_DATE"),
        version,
    }
}
