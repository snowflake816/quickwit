/*
 * Copyright (C) 2021 Quickwit Inc.
 *
 * Quickwit is offered under the AGPL v3.0 and as commercial software.
 * For commercial licensing, contact us at hello@quickwit.io.
 *
 * AGPL:
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU Affero General Public License as
 * published by the Free Software Foundation, either version 3 of the
 * License, or (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU Affero General Public License for more details.
 *
 * You should have received a copy of the GNU Affero General Public License
 * along with this program.  If not, see <http://www.gnu.org/licenses/>.
 */

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use tokio::task::JoinHandle;
use tracing::*;

use quickwit_metastore::{Metastore, SplitMetadata};
use quickwit_proto::{
    FetchDocsRequest, FetchDocsResult, Hit, LeafSearchRequest, LeafSearchResult, PartialHit,
    SearchRequest, SearchResult,
};
use tantivy::collector::Collector;
use tokio::task::spawn_blocking;

use crate::client_pool::Job;
use crate::list_relevant_splits;
use crate::make_collector;
use crate::ClientPool;
use crate::SearchClientPool;

// Measure the cost associated to searching in a given split metadata.
fn compute_split_cost(_split_metadata: &SplitMetadata) -> u32 {
    //TODO: Have a smarter cost, by smoothing the number of docs.
    1
}

/// Perform a distributed search.
/// It sends a search request over gRPC to multiple leaf nodes and merges the search results.
pub async fn root_search(
    search_request: &SearchRequest,
    metastore: &dyn Metastore,
    client_pool: &Arc<SearchClientPool>,
) -> anyhow::Result<SearchResult> {
    let start_instant = tokio::time::Instant::now();

    // Create a job for leaf node search and assign the splits that the node is responsible for based on the job.
    let split_metadata_list = list_relevant_splits(search_request, metastore).await?;

    // Create a hash map of SplitMetadata with split id as a key.
    let split_metadata_map: HashMap<String, SplitMetadata> = split_metadata_list
        .into_iter()
        .map(|split_metadata| (split_metadata.split_id.clone(), split_metadata))
        .collect();

    // Create a job for fetching docs and assign the splits that the node is responsible for based on the job.
    let leaf_search_jobs: Vec<Job> = split_metadata_map
        .keys()
        .map(|split_id| {
            // TODO: Change to a better way that does not use unwrap().
            let split_metadata = split_metadata_map.get(split_id).unwrap();
            Job {
                split: split_id.clone(),
                cost: compute_split_cost(split_metadata),
            }
        })
        .collect();

    let assigned_leaf_search_jobs = client_pool.assign_jobs(leaf_search_jobs).await?;

    let clients = client_pool.clients.read().await;

    // Create search request with start offset is 0 that used by leaf node search.
    let mut search_request_with_offset_0 = search_request.clone();
    search_request_with_offset_0.start_offset = 0;
    search_request_with_offset_0.max_hits += search_request.start_offset;

    // Perform the query phese.
    let mut leaf_search_handles: Vec<JoinHandle<anyhow::Result<LeafSearchResult>>> = Vec::new();
    for (addr, jobs) in assigned_leaf_search_jobs.iter() {
        let mut search_client = if let Some(client) = clients.get(&addr) {
            client.clone()
        } else {
            anyhow::bail!(
                "There is no search client for {} in the search client pool.",
                addr
            );
        };

        let leaf_search_request = LeafSearchRequest {
            search_request: Some(search_request_with_offset_0.clone()),
            split_uris: jobs.iter().map(|job| job.split.clone()).collect(),
        };

        let handle = tokio::spawn(async move {
            match search_client.leaf_search(leaf_search_request).await {
                Ok(resp) => Ok(resp.into_inner()),
                Err(err) => Err(anyhow::anyhow!(
                    "Failed to search a leaf node due to {:?}",
                    err
                )),
            }
        });
        leaf_search_handles.push(handle);
    }
    let leaf_search_responses = futures::future::try_join_all(leaf_search_handles).await?;

    let index_metadata = metastore.index_metadata(&search_request.index_id).await?;
    let doc_mapper = index_metadata.index_config;
    let collector = make_collector(doc_mapper.as_ref(), search_request);

    // Find the sum of the number of hits and merge multiple partial hits into a single partial hits.
    let mut leaf_search_results = Vec::new();
    for leaf_search_response in leaf_search_responses {
        match leaf_search_response {
            Ok(leaf_search_result) => leaf_search_results.push(leaf_search_result),
            Err(err) => error!(err=?err),
        }
    }
    let leaf_search_result = spawn_blocking(move || collector.merge_fruits(leaf_search_results))
        .await
        .with_context(|| "Failed to merge leaf search results.")??;

    // Create a hash map of PartialHit with split as a key.
    let mut partial_hits_map: HashMap<String, Vec<PartialHit>> = HashMap::new();
    for partial_hit in leaf_search_result.partial_hits.iter() {
        partial_hits_map
            .entry(partial_hit.split.clone())
            .or_insert_with(Vec::new)
            .push(partial_hit.clone());
    }

    // Perform the fetch docs phese.
    let mut fetch_docs_handles: Vec<JoinHandle<anyhow::Result<FetchDocsResult>>> = Vec::new();
    for (addr, jobs) in assigned_leaf_search_jobs.iter() {
        for job in jobs {
            let mut search_client = if let Some(client) = clients.get(&addr) {
                client.clone()
            } else {
                anyhow::bail!(
                    "There is no search client for {} in the search client pool.",
                    addr
                );
            };

            if let Some(partial_hits) = partial_hits_map.get(&job.split) {
                let fetch_docs_request = FetchDocsRequest {
                    partial_hits: partial_hits.clone(),
                    index_id: search_request.index_id.clone(),
                };
                let handle = tokio::spawn(async move {
                    match search_client.fetch_docs(fetch_docs_request).await {
                        Ok(resp) => Ok(resp.into_inner()),
                        Err(err) => Err(anyhow::anyhow!("Failed to fetch docs due to {:?}", err)),
                    }
                });
                fetch_docs_handles.push(handle);
            }
        }
    }
    let fetch_docs_responses = futures::future::try_join_all(fetch_docs_handles).await?;

    // Merge the fetched docs.
    let mut hits: Vec<Hit> = Vec::new();
    for response in fetch_docs_responses {
        match response {
            Ok(fetch_docs_result) => {
                hits.extend(fetch_docs_result.hits);
            }
            Err(err) => error!(err=?err),
        }
    }
    hits.sort_by(|hit1, hit2| {
        let value1 = if let Some(partial_hit) = &hit1.partial_hit {
            partial_hit.sorting_field_value
        } else {
            0
        };
        let value2 = if let Some(partial_hit) = &hit2.partial_hit {
            partial_hit.sorting_field_value
        } else {
            0
        };
        // Sort by descending order.
        value2.cmp(&value1)
    });

    let elapsed = start_instant.elapsed();

    Ok(SearchResult {
        num_hits: leaf_search_result.num_hits,
        hits,
        elapsed_time_micros: elapsed.as_micros() as u64,
    })
}
