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

use std::collections::{HashMap, HashSet};
use std::ops::Bound;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context;
use futures::future::try_join_all;
use itertools::{Either, Itertools};
use quickwit_directories::{CachingDirectory, HotDirectory, StorageDirectory};
use quickwit_doc_mapper::{DocMapper, TermRange, WarmupInfo};
use quickwit_proto::search::{
    LeafListTermsResponse, LeafSearchResponse, ListTermsRequest, SearchRequest,
    SplitIdAndFooterOffsets, SplitSearchError,
};
use quickwit_query::query_ast::QueryAst;
use quickwit_storage::{
    wrap_storage_with_long_term_cache, BundleStorage, MemorySizedCache, OwnedBytes, Storage,
};
use tantivy::collector::Collector;
use tantivy::directory::FileSlice;
use tantivy::fastfield::FastFieldReaders;
use tantivy::schema::{Field, FieldType};
use tantivy::tokenizer::TokenizerManager;
use tantivy::{Index, ReloadPolicy, Searcher, Term};
use tracing::*;

use crate::collector::{make_collector_for_split, make_merge_collector};
use crate::service::SearcherContext;
use crate::SearchError;

#[instrument(skip(index_storage, footer_cache))]
async fn get_split_footer_from_cache_or_fetch(
    index_storage: Arc<dyn Storage>,
    split_and_footer_offsets: &SplitIdAndFooterOffsets,
    footer_cache: &MemorySizedCache<String>,
) -> anyhow::Result<OwnedBytes> {
    {
        let possible_val = footer_cache.get(&split_and_footer_offsets.split_id);
        if let Some(footer_data) = possible_val {
            return Ok(footer_data);
        }
    }
    let split_file = PathBuf::from(format!("{}.split", split_and_footer_offsets.split_id));
    let footer_data_opt = index_storage
        .get_slice(
            &split_file,
            split_and_footer_offsets.split_footer_start as usize
                ..split_and_footer_offsets.split_footer_end as usize,
        )
        .await
        .with_context(|| {
            format!(
                "Failed to fetch hotcache and footer from {} for split `{}`",
                index_storage.uri(),
                split_and_footer_offsets.split_id
            )
        })?;

    footer_cache.put(
        split_and_footer_offsets.split_id.to_owned(),
        footer_data_opt.clone(),
    );

    Ok(footer_data_opt)
}

/// Opens a `tantivy::Index` for the given split with several cache layers:
/// - A split footer cache given by `SearcherContext.split_footer_cache`.
/// - A fast fields cache given by `SearcherContext.storage_long_term_cache`.
/// - An ephemeral unbounded cache directory whose lifetime is tied to the returned `Index`.
#[instrument(skip(searcher_context, index_storage, tokenizer_manager))]
pub(crate) async fn open_index_with_caches(
    searcher_context: &SearcherContext,
    index_storage: Arc<dyn Storage>,
    split_and_footer_offsets: &SplitIdAndFooterOffsets,
    tokenizer_manager: Option<&TokenizerManager>,
    ephemeral_unbounded_cache: bool,
) -> anyhow::Result<Index> {
    let split_file = PathBuf::from(format!("{}.split", split_and_footer_offsets.split_id));
    let footer_data = get_split_footer_from_cache_or_fetch(
        index_storage.clone(),
        split_and_footer_offsets,
        &searcher_context.split_footer_cache,
    )
    .await?;

    let (hotcache_bytes, bundle_storage) = BundleStorage::open_from_split_data(
        index_storage,
        split_file,
        FileSlice::new(Arc::new(footer_data)),
    )?;
    let bundle_storage_with_cache = wrap_storage_with_long_term_cache(
        searcher_context.fast_fields_cache.clone(),
        Arc::new(bundle_storage),
    );
    let directory = StorageDirectory::new(bundle_storage_with_cache);
    let hot_directory = if ephemeral_unbounded_cache {
        let caching_directory = CachingDirectory::new_unbounded(Arc::new(directory));
        HotDirectory::open(caching_directory, hotcache_bytes.read_bytes()?)?
    } else {
        HotDirectory::open(directory, hotcache_bytes.read_bytes()?)?
    };
    let mut index = Index::open(hot_directory)?;
    if let Some(tokenizer_manager) = tokenizer_manager {
        index.set_tokenizers(tokenizer_manager.clone());
    }
    index.set_fast_field_tokenizers(
        quickwit_query::get_quickwit_fastfield_normalizer_manager().clone(),
    );
    Ok(index)
}

/// Tantivy search does not make it possible to fetch data asynchronously during
/// search.
///
/// It is required to download all required information in advance.
/// This is the role of the `warmup` function.
///
/// The downloaded data depends on the query (which term's posting list is required,
/// are position required too), and the collector.
///
/// * `query` - query is used to extract the terms and their fields which will be loaded from the
/// inverted_index.
///
/// * `term_dict_field_names` - A list of fields, where the whole dictionary needs to be loaded.
/// This is e.g. required for term aggregation, since we don't know in advance which terms are going
/// to be hit.
#[instrument(skip(searcher))]
pub(crate) async fn warmup(searcher: &Searcher, warmup_info: &WarmupInfo) -> anyhow::Result<()> {
    debug!(warmup_info=?warmup_info, "warmup");
    let warm_up_terms_future = warm_up_terms(searcher, &warmup_info.terms_grouped_by_field)
        .instrument(debug_span!("warm_up_terms"));
    let warm_up_term_ranges_future =
        warm_up_term_ranges(searcher, &warmup_info.term_ranges_grouped_by_field)
            .instrument(debug_span!("warm_up_term_ranges"));
    let warm_up_term_dict_future =
        warm_up_term_dict_fields(searcher, &warmup_info.term_dict_field_names)
            .instrument(debug_span!("warm_up_term_dicts"));
    let warm_up_fastfields_future = warm_up_fastfields(searcher, &warmup_info.fast_field_names)
        .instrument(debug_span!("warm_up_fastfields"));
    let warm_up_fieldnorms_future = warm_up_fieldnorms(searcher, warmup_info.field_norms)
        .instrument(debug_span!("warm_up_fieldnorms"));
    let warm_up_postings_future = warm_up_postings(searcher, &warmup_info.posting_field_names)
        .instrument(debug_span!("warm_up_postings"));

    tokio::try_join!(
        warm_up_terms_future,
        warm_up_term_ranges_future,
        warm_up_fastfields_future,
        warm_up_term_dict_future,
        warm_up_fieldnorms_future,
        warm_up_postings_future,
    )?;

    Ok(())
}

async fn warm_up_term_dict_fields(
    searcher: &Searcher,
    term_dict_field_names: &HashSet<String>,
) -> anyhow::Result<()> {
    let mut term_dict_fields = Vec::new();
    for term_dict_field_name in term_dict_field_names.iter() {
        let term_dict_field = searcher
            .schema()
            .get_field(term_dict_field_name)
            .with_context(|| {
                format!(
                    "Couldn't get field named `{term_dict_field_name}` from schema to warm up \
                     term dicts."
                )
            })?;

        term_dict_fields.push(term_dict_field);
    }

    let mut warm_up_futures = Vec::new();
    for field in term_dict_fields {
        for segment_reader in searcher.segment_readers() {
            let inverted_index = segment_reader.inverted_index(field)?.clone();
            warm_up_futures.push(async move {
                let dict = inverted_index.terms();
                dict.warm_up_dictionary().await
            });
        }
    }
    try_join_all(warm_up_futures).await?;
    Ok(())
}

async fn warm_up_postings(
    searcher: &Searcher,
    field_names: &HashSet<String>,
) -> anyhow::Result<()> {
    let mut fields = Vec::new();
    for field_name in field_names.iter() {
        let field = searcher.schema().get_field(field_name).with_context(|| {
            format!("Couldn't get field named `{field_name}` from schema to warm up postings.")
        })?;

        fields.push(field);
    }

    let mut warm_up_futures = Vec::new();
    for field in fields {
        for segment_reader in searcher.segment_readers() {
            let inverted_index = segment_reader.inverted_index(field)?.clone();
            warm_up_futures.push(async move { inverted_index.warm_postings_full(false).await });
        }
    }
    try_join_all(warm_up_futures).await?;
    Ok(())
}

async fn warm_up_fastfield(
    fast_field_reader: &FastFieldReaders,
    fast_field_name: &str,
) -> anyhow::Result<()> {
    let columns = fast_field_reader
        .list_dynamic_column_handles(fast_field_name)
        .await?;
    futures::future::try_join_all(
        columns
            .into_iter()
            .map(|col| async move { col.file_slice().read_bytes_async().await }),
    )
    .await?;
    Ok(())
}

/// Populates the short-lived cache with the data for
/// all of the fast fields passed as argument.
async fn warm_up_fastfields(
    searcher: &Searcher,
    fast_field_names: &HashSet<String>,
) -> anyhow::Result<()> {
    let mut warm_up_futures = Vec::new();
    for segment_reader in searcher.segment_readers() {
        let fast_field_reader = segment_reader.fast_fields();
        for fast_field_name in fast_field_names {
            let warm_up_fut = warm_up_fastfield(fast_field_reader, fast_field_name);
            warm_up_futures.push(Box::pin(warm_up_fut));
        }
    }
    futures::future::try_join_all(warm_up_futures).await?;
    Ok(())
}

async fn warm_up_terms(
    searcher: &Searcher,
    terms_grouped_by_field: &HashMap<Field, HashMap<Term, bool>>,
) -> anyhow::Result<()> {
    let mut warm_up_futures = Vec::new();
    for (field, terms) in terms_grouped_by_field {
        for segment_reader in searcher.segment_readers() {
            let inv_idx = segment_reader.inverted_index(*field)?;
            for (term, position_needed) in terms.iter() {
                let inv_idx_clone = inv_idx.clone();
                warm_up_futures
                    .push(async move { inv_idx_clone.warm_postings(term, *position_needed).await });
            }
        }
    }
    try_join_all(warm_up_futures).await?;
    Ok(())
}

async fn warm_up_term_ranges(
    searcher: &Searcher,
    terms_grouped_by_field: &HashMap<Field, HashMap<TermRange, bool>>,
) -> anyhow::Result<()> {
    let mut warm_up_futures = Vec::new();
    for (field, terms) in terms_grouped_by_field {
        for segment_reader in searcher.segment_readers() {
            let inv_idx = segment_reader.inverted_index(*field)?;
            for (term_range, position_needed) in terms.iter() {
                let inv_idx_clone = inv_idx.clone();
                let range = (term_range.start.as_ref(), term_range.end.as_ref());
                warm_up_futures.push(async move {
                    inv_idx_clone
                        .warm_postings_range(range, term_range.limit, *position_needed)
                        .await
                });
            }
        }
    }
    try_join_all(warm_up_futures).await?;
    Ok(())
}

async fn warm_up_fieldnorms(searcher: &Searcher, requires_scoring: bool) -> anyhow::Result<()> {
    if !requires_scoring {
        return Ok(());
    }
    let mut warm_up_futures = Vec::new();
    for field in searcher.schema().fields() {
        for segment_reader in searcher.segment_readers() {
            let fieldnorm_readers = segment_reader.fieldnorms_readers();
            let file_handle_opt = fieldnorm_readers.get_inner_file().open_read(field.0);
            if let Some(file_handle) = file_handle_opt {
                warm_up_futures.push(async move { file_handle.read_bytes_async().await })
            }
        }
    }
    try_join_all(warm_up_futures).await?;
    Ok(())
}

/// Apply a leaf search on a single split.
#[instrument(skip(searcher_context, search_request, storage, split, doc_mapper,))]
async fn leaf_search_single_split(
    searcher_context: &SearcherContext,
    mut search_request: SearchRequest,
    storage: Arc<dyn Storage>,
    split: SplitIdAndFooterOffsets,
    doc_mapper: Arc<dyn DocMapper>,
) -> crate::Result<LeafSearchResponse> {
    rewrite_request(&mut search_request, &split);
    if let Some(cached_answer) = searcher_context
        .leaf_search_cache
        .get(split.clone(), search_request.clone())
    {
        return Ok(cached_answer);
    }

    let split_id = split.split_id.to_string();
    let index = open_index_with_caches(
        searcher_context,
        storage,
        &split,
        Some(doc_mapper.tokenizer_manager()),
        true,
    )
    .await?;
    let split_schema = index.schema();

    let quickwit_collector = make_collector_for_split(
        split_id.clone(),
        doc_mapper.as_ref(),
        &search_request,
        searcher_context.get_aggregation_limits(),
    )?;
    let query_ast: QueryAst = serde_json::from_str(search_request.query_ast.as_str())
        .map_err(|err| SearchError::InvalidQuery(err.to_string()))?;
    let (query, mut warmup_info) = doc_mapper.query(split_schema, &query_ast, false)?;
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let searcher = reader.searcher();

    let collector_warmup_info = quickwit_collector.warmup_info();
    warmup_info.merge(collector_warmup_info);

    warmup(&searcher, &warmup_info).await?;
    let span = info_span!("tantivy_search", split_id = %split.split_id);
    let leaf_search_response = crate::run_cpu_intensive(move || {
        let _span_guard = span.enter();
        searcher.search(&query, &quickwit_collector)
    })
    .await
    .map_err(|_| {
        crate::SearchError::InternalError(format!("Leaf search panicked. split={split_id}"))
    })??;

    searcher_context
        .leaf_search_cache
        .put(split, search_request, leaf_search_response.clone());
    Ok(leaf_search_response)
}

/// Rewrite a request removing parts which incure additional download or computation with no
/// effect.
///
/// This include things such as sorting result by a field or _score when no document is requested,
/// or applying date range when the range covers the entire split.
fn rewrite_request(search_request: &mut SearchRequest, split: &SplitIdAndFooterOffsets) {
    if search_request.max_hits == 0 {
        search_request.sort_fields = vec![];
    }
    rewrite_start_end_time_bounds(
        &mut search_request.start_timestamp,
        &mut search_request.end_timestamp,
        split,
    )
}

pub(crate) fn rewrite_start_end_time_bounds(
    start_timestamp_opt: &mut Option<i64>,
    end_timestamp_opt: &mut Option<i64>,
    split: &SplitIdAndFooterOffsets,
) {
    if let (Some(split_start), Some(split_end)) = (split.timestamp_start, split.timestamp_end) {
        if let Some(start_timestamp) = start_timestamp_opt {
            // both starts are inclusive
            if *start_timestamp <= split_start {
                *start_timestamp_opt = None;
            }
        }
        if let Some(end_timestamp) = end_timestamp_opt {
            // search end is exclusive, split end is inclusive
            if *end_timestamp > split_end {
                *end_timestamp_opt = None;
            }
        }
    }
}

/// `leaf` step of search.
///
/// The leaf search collects all kind of information, and returns a set of
/// [PartialHit](quickwit_proto::PartialHit) candidates. The root will be in
/// charge to consolidate, identify the actual final top hits to display, and
/// fetch the actual documents to convert the partial hits into actual Hits.
pub async fn leaf_search(
    searcher_context: Arc<SearcherContext>,
    request: &SearchRequest,
    index_storage: Arc<dyn Storage>,
    splits: &[SplitIdAndFooterOffsets],
    doc_mapper: Arc<dyn DocMapper>,
) -> Result<LeafSearchResponse, SearchError> {
    let request = Arc::new(request.clone());
    let leaf_search_single_split_futures: Vec<_> = splits
        .iter()
        .map(|split| {
            let split = split.clone();
            let doc_mapper_clone = doc_mapper.clone();
            let index_storage_clone = index_storage.clone();
            let searcher_context_clone = searcher_context.clone();
            let request = request.clone();
            tokio::spawn(
                async move {
                let _leaf_split_search_permit = searcher_context_clone.leaf_search_split_semaphore
                    .acquire()
                    .await
                    .expect("Failed to acquire permit. This should never happen! Please, report on https://github.com/quickwit-oss/quickwit/issues.");
                crate::SEARCH_METRICS.leaf_searches_splits_total.inc();
                let timer = crate::SEARCH_METRICS
                    .leaf_search_split_duration_secs
                    .start_timer();
                let leaf_search_single_split_res = leaf_search_single_split(
                    &searcher_context_clone,
                    (*request).clone(),
                    index_storage_clone,
                    split.clone(),
                    doc_mapper_clone,
                )
                .await;
                timer.observe_duration();
                leaf_search_single_split_res.map_err(|err| (split.split_id.clone(), err))
            }.in_current_span())
        })
        .collect();
    let split_search_results = futures::future::join_all(leaf_search_single_split_futures).await;

    // the result wrapping is only for the collector api merge_fruits
    // (Vec<tantivy::Result<LeafSearchResponse>>)
    let (split_search_responses, errors): (
        Vec<tantivy::Result<LeafSearchResponse>>,
        Vec<(String, SearchError)>,
    ) = split_search_results
        .into_iter()
        .partition_map(|split_search_res| match split_search_res {
            Ok(Ok(split_search_resp)) => Either::Left(Ok(split_search_resp)),
            Ok(Err(err)) => Either::Right(err),
            Err(e) => {
                warn!("A leaf_search_single_split panicked");
                Either::Right(("unknown".to_string(), e.into()))
            }
        });

    // Creates a collector which merges responses into one
    let merge_collector =
        make_merge_collector(&request, &searcher_context.get_aggregation_limits())?;

    // Merging is a cpu-bound task.
    // It should be executed by Tokio's blocking threads.
    let span = info_span!("merge_search_responses");
    let mut merged_search_response = crate::run_cpu_intensive(move || {
        let _span_guard = span.enter();
        merge_collector.merge_fruits(split_search_responses)
    })
    .await
    .context("Failed to merge split search responses.")??;

    merged_search_response
        .failed_splits
        .extend(errors.into_iter().map(|(split_id, err)| SplitSearchError {
            split_id,
            error: format!("{err}"),
            retryable_error: true,
        }));
    Ok(merged_search_response)
}

/// Apply a leaf list terms on a single split.
#[instrument(skip(searcher_context, search_request, storage, split))]
async fn leaf_list_terms_single_split(
    searcher_context: &SearcherContext,
    search_request: &ListTermsRequest,
    storage: Arc<dyn Storage>,
    split: SplitIdAndFooterOffsets,
) -> crate::Result<LeafListTermsResponse> {
    let index = open_index_with_caches(searcher_context, storage, &split, None, true).await?;
    let split_schema = index.schema();
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let searcher = reader.searcher();

    let field = split_schema
        .get_field(&search_request.field)
        .with_context(|| {
            format!(
                "Couldn't get field named {:?} from schema to list terms.",
                search_request.field
            )
        })?;

    let field_type = split_schema.get_field_entry(field).field_type();
    let start_term: Option<Term> = search_request
        .start_key
        .as_ref()
        .map(|data| term_from_data(field, field_type, data));
    let end_term: Option<Term> = search_request
        .end_key
        .as_ref()
        .map(|data| term_from_data(field, field_type, data));

    let mut segment_results = Vec::new();
    for segment_reader in searcher.segment_readers() {
        let inverted_index = segment_reader.inverted_index(field)?.clone();
        let dict = inverted_index.terms();
        dict.file_slice_for_range(
            (
                start_term
                    .as_ref()
                    .map(Term::serialized_value_bytes)
                    .map(Bound::Included)
                    .unwrap_or(Bound::Unbounded),
                end_term
                    .as_ref()
                    .map(Term::serialized_value_bytes)
                    .map(Bound::Excluded)
                    .unwrap_or(Bound::Unbounded),
            ),
            search_request.max_hits,
        )
        .read_bytes_async()
        .await
        .with_context(|| "Failed to load sstable range")?;

        let mut range = dict.range();
        if let Some(limit) = search_request.max_hits {
            range = range.limit(limit);
        }
        if let Some(start_term) = &start_term {
            range = range.ge(start_term.serialized_value_bytes())
        }
        if let Some(end_term) = &end_term {
            range = range.lt(end_term.serialized_value_bytes())
        }
        let mut stream = range
            .into_stream()
            .with_context(|| "Failed to create stream over sstable")?;
        let mut segment_result: Vec<Vec<u8>> =
            Vec::with_capacity(search_request.max_hits.unwrap_or(0) as usize);
        while stream.advance() {
            segment_result.push(term_to_data(field, field_type, stream.key()));
        }
        segment_results.push(segment_result);
    }

    let merged_iter = segment_results.into_iter().kmerge().dedup();
    let merged_results: Vec<Vec<u8>> = if let Some(limit) = search_request.max_hits {
        merged_iter.take(limit as usize).collect()
    } else {
        merged_iter.collect()
    };

    Ok(LeafListTermsResponse {
        num_hits: merged_results.len() as u64,
        terms: merged_results,
        num_attempted_splits: 1,
        failed_splits: Vec::new(),
    })
}

fn term_from_data(field: Field, field_type: &FieldType, data: &[u8]) -> Term {
    let mut term = Term::from_field_bool(field, false);
    term.clear_with_type(field_type.value_type());
    term.append_bytes(data);
    term
}

fn term_to_data(field: Field, field_type: &FieldType, field_value: &[u8]) -> Vec<u8> {
    let mut term = Term::from_field_bool(field, false);
    term.clear_with_type(field_type.value_type());
    term.append_bytes(field_value);
    term.serialized_term().to_vec()
}

/// `leaf` step of list terms.
pub async fn leaf_list_terms(
    searcher_context: Arc<SearcherContext>,
    request: &ListTermsRequest,
    index_storage: Arc<dyn Storage>,
    splits: &[SplitIdAndFooterOffsets],
) -> Result<LeafListTermsResponse, SearchError> {
    let leaf_search_single_split_futures: Vec<_> = splits
        .iter()
        .map(|split| {
            let index_storage_clone = index_storage.clone();
            let searcher_context_clone = searcher_context.clone();
            async move {
                let _leaf_split_search_permit = searcher_context_clone.leaf_search_split_semaphore
                    .acquire()
                    .await
                    .expect("Failed to acquire permit. This should never happen! Please, report on https://github.com/quickwit-oss/quickwit/issues.");
                // TODO dedicated counter and timer?
                crate::SEARCH_METRICS.leaf_searches_splits_total.inc();
                let timer = crate::SEARCH_METRICS
                    .leaf_search_split_duration_secs
                    .start_timer();
                let leaf_search_single_split_res = leaf_list_terms_single_split(
                    &searcher_context_clone,
                    request,
                    index_storage_clone,
                    split.clone(),
                )
                .await;
                timer.observe_duration();
                leaf_search_single_split_res.map_err(|err| (split.split_id.clone(), err))
            }
        })
        .collect();

    let split_search_results = futures::future::join_all(leaf_search_single_split_futures).await;

    let (split_search_responses, errors): (Vec<LeafListTermsResponse>, Vec<(String, SearchError)>) =
        split_search_results
            .into_iter()
            .partition_map(|split_search_res| match split_search_res {
                Ok(split_search_resp) => Either::Left(split_search_resp),
                Err(err) => Either::Right(err),
            });

    let merged_iter = split_search_responses
        .into_iter()
        .map(|leaf_search_response| leaf_search_response.terms)
        .kmerge()
        .dedup();
    let terms: Vec<Vec<u8>> = if let Some(limit) = request.max_hits {
        merged_iter.take(limit as usize).collect()
    } else {
        merged_iter.collect()
    };

    let failed_splits = errors
        .into_iter()
        .map(|(split_id, err)| SplitSearchError {
            split_id,
            error: err.to_string(),
            retryable_error: true,
        })
        .collect();
    let merged_search_response = LeafListTermsResponse {
        num_hits: terms.len() as u64,
        terms,
        num_attempted_splits: splits.len() as u64,
        failed_splits,
    };

    Ok(merged_search_response)
}
