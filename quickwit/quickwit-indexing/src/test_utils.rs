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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use chitchat::transport::ChannelTransport;
use quickwit_actors::{Mailbox, Universe};
use quickwit_cluster::create_cluster_for_test;
use quickwit_common::rand::append_random_suffix;
use quickwit_common::uri::{Protocol, Uri};
use quickwit_config::{
    build_doc_mapper, ConfigFormat, IndexConfig, IndexerConfig, SourceConfig, SourceParams,
    VecSourceParams,
};
use quickwit_doc_mapper::DocMapper;
use quickwit_metastore::file_backed_metastore::FileBackedMetastoreFactory;
use quickwit_metastore::{Metastore, MetastoreUriResolver, Split, SplitMetadata, SplitState};
use quickwit_storage::{Storage, StorageUriResolver};
use serde_json::Value as JsonValue;

use crate::actors::IndexingService;
use crate::models::{DetachIndexingPipeline, IndexingStatistics, SpawnPipeline};

/// Creates a Test environment.
///
/// It makes it easy to create a test index, perfect for unit testing.
/// The test index content is entirely in RAM and isolated,
/// but the construction of the index involves temporary file directory.
pub struct TestSandbox {
    index_id: String,
    indexing_service: Mailbox<IndexingService>,
    doc_mapper: Arc<dyn DocMapper>,
    metastore: Arc<dyn Metastore>,
    storage_resolver: StorageUriResolver,
    storage: Arc<dyn Storage>,
    add_docs_id: AtomicUsize,
    _universe: Universe,
    _temp_dir: tempfile::TempDir,
}

const METASTORE_URI: &str = "ram://quickwit-test-indexes";

fn index_uri(index_id: &str) -> Uri {
    Uri::from_well_formed(format!("{METASTORE_URI}/{index_id}"))
}

impl TestSandbox {
    /// Creates a new test environment.
    pub async fn create(
        index_id: &str,
        doc_mapping_yaml: &str,
        indexing_settings_yaml: &str,
        search_fields: &[&str],
    ) -> anyhow::Result<Self> {
        let node_id = append_random_suffix("test-node");
        let transport = ChannelTransport::default();
        let cluster = Arc::new(
            create_cluster_for_test(Vec::new(), &["indexer"], &transport, true)
                .await
                .unwrap(),
        );
        let index_uri = index_uri(index_id);
        let mut index_config = IndexConfig::for_test(index_id, index_uri.as_str());
        index_config.doc_mapping = ConfigFormat::Yaml.parse(doc_mapping_yaml.as_bytes())?;
        index_config.indexing_settings =
            ConfigFormat::Yaml.parse(indexing_settings_yaml.as_bytes())?;
        index_config.search_settings.default_search_fields = search_fields
            .iter()
            .map(|search_field| search_field.to_string())
            .collect();
        let doc_mapper =
            build_doc_mapper(&index_config.doc_mapping, &index_config.search_settings)?;
        let temp_dir = tempfile::tempdir()?;
        let indexer_config = IndexerConfig::for_test()?;
        let storage_resolver = StorageUriResolver::for_test();
        let metastore_uri_resolver = MetastoreUriResolver::builder()
            .register(
                Protocol::Ram,
                FileBackedMetastoreFactory::new(storage_resolver.clone()),
            )
            .build();
        let metastore = metastore_uri_resolver
            .resolve(&Uri::from_well_formed(METASTORE_URI))
            .await?;
        metastore.create_index(index_config.clone()).await?;
        let storage = storage_resolver.resolve(&index_uri)?;
        let universe = Universe::new();
        let indexing_service_actor = IndexingService::new(
            node_id.to_string(),
            temp_dir.path().to_path_buf(),
            indexer_config,
            cluster,
            metastore.clone(),
            storage_resolver.clone(),
        )
        .await?;
        let (indexing_service, _indexing_service_handle) =
            universe.spawn_builder().spawn(indexing_service_actor);
        Ok(TestSandbox {
            index_id: index_id.to_string(),
            indexing_service,
            doc_mapper,
            metastore,
            storage_resolver,
            storage,
            add_docs_id: AtomicUsize::default(),
            _universe: universe,
            _temp_dir: temp_dir,
        })
    }

    /// Adds documents.
    ///
    /// The documents are expected to be `JsonValue`.
    /// They can be created using the `serde_json::json!` macro.
    pub async fn add_documents<I>(&self, split_docs: I) -> anyhow::Result<IndexingStatistics>
    where
        I: IntoIterator<Item = JsonValue> + 'static,
        I::IntoIter: Send,
    {
        let docs: Vec<String> = split_docs
            .into_iter()
            .map(|doc_json| doc_json.to_string())
            .collect();
        let add_docs_id = self.add_docs_id.fetch_add(1, Ordering::SeqCst);
        let source_config = SourceConfig {
            source_id: self.index_id.clone(),
            max_num_pipelines_per_indexer: 0,
            desired_num_pipelines: 1,
            enabled: true,
            source_params: SourceParams::Vec(VecSourceParams {
                docs,
                batch_num_docs: 10,
                partition: format!("add-docs-{}", add_docs_id),
            }),
            transform_config: None,
        };
        let pipeline_id = self
            .indexing_service
            .ask_for_res(SpawnPipeline {
                index_id: self.index_id.clone(),
                source_config,
                pipeline_ord: 0,
            })
            .await?;
        let pipeline_handle = self
            .indexing_service
            .ask_for_res(DetachIndexingPipeline {
                pipeline_id: pipeline_id.clone(),
            })
            .await?;
        let (_pipeline_exit_status, pipeline_statistics) = pipeline_handle.join().await;
        Ok(pipeline_statistics)
    }

    /// Returns the metastore of the TestSandbox.
    ///
    /// The metastore is a file-backed metastore.
    /// Its data can be found via the `storage` in
    /// the `ram://quickwit-test-indexes` directory.
    pub fn metastore(&self) -> Arc<dyn Metastore> {
        self.metastore.clone()
    }

    /// Returns the storage of the TestSandbox.
    pub fn storage(&self) -> Arc<dyn Storage> {
        self.storage.clone()
    }

    /// Returns the storage URI resolver of the TestSandbox.
    pub fn storage_uri_resolver(&self) -> StorageUriResolver {
        self.storage_resolver.clone()
    }

    /// Returns the doc mapper of the TestSandbox.
    pub fn doc_mapper(&self) -> Arc<dyn DocMapper> {
        self.doc_mapper.clone()
    }

    /// Returns the index ID.
    pub fn index_id(&self) -> &str {
        &self.index_id
    }
}

/// Mock split helper.
pub fn mock_split(split_id: &str) -> Split {
    Split {
        split_state: SplitState::Published,
        split_metadata: mock_split_meta(split_id),
        update_timestamp: 0,
        publish_timestamp: None,
    }
}

/// Mock split meta helper.
pub fn mock_split_meta(split_id: &str) -> SplitMetadata {
    SplitMetadata {
        split_id: split_id.to_string(),
        partition_id: 13u64,
        num_docs: 10,
        uncompressed_docs_size_in_bytes: 256,
        time_range: None,
        create_timestamp: 0,
        footer_offsets: 700..800,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::TestSandbox;

    #[tokio::test]
    async fn test_test_sandbox() -> anyhow::Result<()> {
        quickwit_common::setup_logging_for_tests();
        let doc_mapping_yaml = r#"
            field_mappings:
              - name: title
                type: text
              - name: body
                type: text
              - name: url
                type: text
        "#;
        let test_sandbox =
            TestSandbox::create("test_index", doc_mapping_yaml, "{}", &["body"]).await?;
        let statistics = test_sandbox.add_documents(vec![
            serde_json::json!({"title": "Hurricane Fay", "body": "...", "url": "http://hurricane-fay"}),
            serde_json::json!({"title": "Ganimede", "body": "...", "url": "http://ganimede"}),
        ]).await?;
        assert_eq!(statistics.num_uploaded_splits, 1);
        let metastore = test_sandbox.metastore();
        {
            let splits = metastore.list_all_splits("test_index").await?;
            assert_eq!(splits.len(), 1);
            test_sandbox.add_documents(vec![
            serde_json::json!({"title": "Byzantine-Ottoman wars", "body": "...", "url": "http://biz-ottoman"}),
        ]).await?;
        }
        {
            let splits = metastore.list_all_splits("test_index").await?;
            assert_eq!(splits.len(), 2);
        }
        Ok(())
    }
}
