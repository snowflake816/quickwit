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

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::net::SocketAddr;

use anyhow::bail;
use quickwit_common::rendezvous_hasher::sort_by_rendez_vous_hash;
use quickwit_grpc_clients::service_client_pool::ServiceClientPool;
use tracing::error;

use crate::SearchServiceClient;

/// Job.
/// The unit in which distributed search is performed.
///
/// The `split_id` is used to define an affinity between a leaf nodes and a job.
/// The `cost` is used to spread the work evenly amongst nodes.
pub trait Job {
    /// SplitId of the split that is targetted.
    fn split_id(&self) -> &str;
    /// Estimation of the load associated with running a given job.
    ///
    /// A list of job will be assigned to leaf nodes in a way that spread
    /// the sum of cost evenly.
    fn cost(&self) -> u32;
}

/// Search job placer.
/// It assigns jobs to search clients.
#[derive(Clone, Default)]
pub struct SearchJobPlacer {
    /// Search clients pool.
    clients_pool: ServiceClientPool<SearchServiceClient>,
}

impl SearchJobPlacer {
    /// Returns an [`SearchJobPlacer`] from a search service client pool.
    pub fn new(clients_pool: ServiceClientPool<SearchServiceClient>) -> Self {
        Self { clients_pool }
    }

    /// Returns a copy of the entire clients map.
    pub fn clients(&self) -> HashMap<SocketAddr, SearchServiceClient> {
        self.clients_pool.all()
    }
}

fn job_order_key<J: Job>(job: &J) -> (Reverse<u32>, &str) {
    (Reverse(job.cost()), job.split_id())
}

/// Node is a utility struct used to represent a rendez-vous hashing node.
/// It's used to track the load and the computed hash for a given key
#[derive(Clone, Debug)]
struct Node {
    pub peer_grpc_addr: SocketAddr,
    pub load: u64,
}

impl Hash for Node {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.peer_grpc_addr.hash(state);
    }
}

impl SearchJobPlacer {
    /// Assign the given job to the clients.
    /// Returns a list of pair (SocketAddr, `Vec<Job>`)
    ///
    /// When exclude_addresses filters all clients it is ignored.
    pub fn assign_jobs<J: Job>(
        &self,
        mut jobs: Vec<J>,
        exclude_addresses: &HashSet<SocketAddr>,
    ) -> anyhow::Result<Vec<(SearchServiceClient, Vec<J>)>> {
        let mut splits_groups: HashMap<SocketAddr, Vec<J>> = HashMap::new();

        // Distribute using rendez-vous hashing
        let mut nodes: Vec<Node> = Vec::new();
        let mut socket_to_client: HashMap<SocketAddr, SearchServiceClient> = Default::default();

        {
            // TODO optimize the case where there are few jobs and many clients.
            let clients = self.clients();

            // when exclude_addresses excludes all adresses we discard it
            let empty_set = HashSet::default();
            let exclude_addresses_if_not_saturated = if exclude_addresses.len() == clients.len() {
                &empty_set
            } else {
                exclude_addresses
            };

            for (grpc_addr, client) in clients
                .into_iter()
                .filter(|(grpc_addr, _)| !exclude_addresses_if_not_saturated.contains(grpc_addr))
            {
                nodes.push(Node {
                    peer_grpc_addr: grpc_addr,
                    load: 0,
                });
                socket_to_client.insert(grpc_addr, client);
            }
        }

        if nodes.is_empty() {
            bail!("No search node available.");
        }

        // Sort job
        jobs.sort_by(|left, right| {
            // sort_by_key does not work here unfortunately
            job_order_key(left).cmp(&job_order_key(right))
        });

        for job in jobs {
            sort_by_rendez_vous_hash(&mut nodes, job.split_id());
            // choose one of the the first two nodes based on least loaded
            let chosen_node_index: usize = if nodes.len() >= 2 {
                usize::from(nodes[0].load > nodes[1].load)
            } else {
                0
            };

            // update node load for next round
            nodes[chosen_node_index].load += job.cost() as u64;

            let chosen_leaf_grpc_addr: SocketAddr = nodes[chosen_node_index].peer_grpc_addr;
            splits_groups
                .entry(chosen_leaf_grpc_addr)
                .or_insert_with(Vec::new)
                .push(job);
        }

        let mut client_to_jobs = Vec::new();
        for (socket_addr, jobs) in splits_groups {
            // Removing the client in order to ensure a 1:1 cardinality on grpc_addr and clients
            if let Some(client) = socket_to_client.remove(&socket_addr) {
                client_to_jobs.push((client, jobs));
            } else {
                error!("Client is missing. This should never happen! Please, report on https://github.com/quickwit-oss/quickwit/issues.");
            }
        }

        Ok(client_to_jobs)
    }

    /// Assigns one job to a client.
    pub fn assign_job<J: Job>(
        &self,
        job: J,
        excluded_addresses: &HashSet<SocketAddr>,
    ) -> anyhow::Result<SearchServiceClient> {
        self.assign_jobs(vec![job], excluded_addresses)?
            .into_iter()
            .next()
            .map(|(client, _jobs)| client)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "`assign_jobs` with {} excluded addresses failed to return at least one \
                     client.",
                    excluded_addresses.len()
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;

    use chitchat::transport::{ChannelTransport, Transport};
    use itertools::Itertools;
    use quickwit_cluster::{create_cluster_for_test, grpc_addr_from_listen_addr_for_test, Cluster};
    use quickwit_grpc_clients::service_client_pool::ServiceClientPool;

    use crate::client::create_search_service_client;
    use crate::root::SearchJob;
    use crate::SearchJobPlacer;

    async fn create_cluster_simple_for_test(
        transport: &dyn Transport,
    ) -> anyhow::Result<Arc<Cluster>> {
        let cluster = create_cluster_for_test(Vec::new(), &["searcher"], transport, true).await?;
        Ok(Arc::new(cluster))
    }

    #[tokio::test]
    async fn test_search_client_pool_single_node() -> anyhow::Result<()> {
        let transport = ChannelTransport::default();
        let cluster = create_cluster_simple_for_test(&transport).await?;
        let job_placer = SearchJobPlacer::new(
            ServiceClientPool::create_and_update_members(cluster.ready_member_change_watcher())
                .await
                .unwrap(),
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
        let clients = job_placer.clients();
        let addrs: Vec<SocketAddr> = clients.into_keys().collect();
        let expected_addrs = vec![grpc_addr_from_listen_addr_for_test(
            cluster.gossip_listen_addr,
        )];
        assert_eq!(addrs, expected_addrs);
        Ok(())
    }

    #[tokio::test]
    async fn test_search_job_placer_multiple_nodes() -> anyhow::Result<()> {
        let transport = ChannelTransport::default();
        let cluster1 = create_cluster_simple_for_test(&transport).await?;
        let node_1 = cluster1.gossip_listen_addr.to_string();
        let cluster2 =
            create_cluster_for_test(vec![node_1], &["searcher"], &transport, true).await?;

        cluster1
            .wait_for_members(|members| members.len() == 2, Duration::from_secs(5))
            .await?;

        let job_placer = SearchJobPlacer::new(
            ServiceClientPool::create_and_update_members(cluster1.ready_member_change_watcher())
                .await
                .unwrap(),
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
        let clients = job_placer.clients();

        let addrs: Vec<SocketAddr> = clients.into_keys().sorted().collect();
        let mut expected_addrs = vec![
            grpc_addr_from_listen_addr_for_test(cluster1.gossip_listen_addr),
            grpc_addr_from_listen_addr_for_test(cluster2.gossip_listen_addr),
        ];
        expected_addrs.sort();
        assert_eq!(addrs, expected_addrs);
        Ok(())
    }

    #[tokio::test]
    async fn test_search_job_placer_single_node_assign_jobs() -> anyhow::Result<()> {
        let transport = ChannelTransport::default();
        let cluster = create_cluster_simple_for_test(&transport).await?;
        let job_placer = SearchJobPlacer::new(
            ServiceClientPool::create_and_update_members(cluster.ready_member_change_watcher())
                .await
                .unwrap(),
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
        let jobs = vec![
            SearchJob::for_test("split1", 1),
            SearchJob::for_test("split2", 2),
            SearchJob::for_test("split3", 3),
            SearchJob::for_test("split4", 4),
        ];

        let assigned_jobs = job_placer.assign_jobs(jobs, &HashSet::default())?;
        let expected_assigned_jobs = vec![(
            create_search_service_client(grpc_addr_from_listen_addr_for_test(
                cluster.gossip_listen_addr,
            ))
            .await?,
            vec![
                SearchJob::for_test("split4", 4),
                SearchJob::for_test("split3", 3),
                SearchJob::for_test("split2", 2),
                SearchJob::for_test("split1", 1),
            ],
        )];
        assert_eq!(
            assigned_jobs.get(0).unwrap().1,
            expected_assigned_jobs.get(0).unwrap().1
        );
        Ok(())
    }
}
