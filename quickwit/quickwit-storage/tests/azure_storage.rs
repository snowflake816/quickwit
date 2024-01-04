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

// This file is an integration test that assumes that a connection
// to Azurite (the emulated azure blob storage environment)
// with default `loose` config is possible.

#[cfg(feature = "integration-testsuite")]
#[tokio::test]
#[cfg_attr(not(feature = "ci-test"), ignore)]
async fn azure_storage_test_suite() -> anyhow::Result<()> {
    use std::path::PathBuf;

    use anyhow::Context;
    use azure_storage_blobs::prelude::ClientBuilder;
    use quickwit_common::rand::append_random_suffix;
    use quickwit_storage::{AzureBlobStorage, MultiPartPolicy};
    let _ = tracing_subscriber::fmt::try_init();

    // Setup container.
    let container_name = append_random_suffix("quickwit").to_lowercase();
    let container_client = ClientBuilder::emulator().container_client(&container_name);
    container_client.create().into_future().await?;

    let mut object_storage = AzureBlobStorage::new_emulated(&container_name);
    quickwit_storage::storage_test_suite(&mut object_storage).await?;

    let mut object_storage = AzureBlobStorage::new_emulated(&container_name).with_prefix(
        PathBuf::from("/integration-tests/test-azure-compatible-storage"),
    );
    quickwit_storage::storage_test_single_part_upload(&mut object_storage)
        .await
        .context("test single-part upload failed")?;

    object_storage.set_policy(MultiPartPolicy {
        // On azure, block size is limited between 64KB and 100MB.
        target_part_num_bytes: 5 * 1_024 * 1_024, // 5MB
        max_num_parts: 10_000,
        multipart_threshold_num_bytes: 10_000_000,
        max_object_num_bytes: 5_000_000_000_000,
        max_concurrent_uploads: 100,
    });
    quickwit_storage::storage_test_multi_part_upload(&mut object_storage)
        .await
        .context("test multipart upload failed")?;

    // Teardown container.
    container_client.delete().into_future().await?;
    Ok(())
}
