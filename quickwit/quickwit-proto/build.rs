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

use std::path::PathBuf;

use glob::glob;
use quickwit_codegen::Codegen;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // "Classic" prost + tonic codegen for metastore and search services.
    let protos: Vec<PathBuf> = find_protos("protos/quickwit")
        .into_iter()
        .filter(|path| !path.ends_with("control_pane.proto") || !path.ends_with("indexing.proto"))
        .collect();

    let mut prost_config = prost_build::Config::default();
    prost_config.protoc_arg("--experimental_allow_proto3_optional");

    tonic_build::configure()
        .type_attribute(".", "#[derive(Serialize, Deserialize, utoipa::ToSchema)]")
        .type_attribute("SearchRequest", "#[derive(Eq, Hash)]")
        .type_attribute("SortField", "#[derive(Eq, Hash)]")
        .type_attribute("SortByValue", "#[derive(Ord, PartialOrd)]")
        .type_attribute("DeleteQuery", "#[serde(default)]")
        .field_attribute(
            "DeleteQuery.start_timestamp",
            "#[serde(skip_serializing_if = \"Option::is_none\")]",
        )
        .field_attribute(
            "DeleteQuery.end_timestamp",
            "#[serde(skip_serializing_if = \"Option::is_none\")]",
        )
        .type_attribute("PartialHit.sort_value", "#[derive(Copy)]")
        .enum_attribute(".", "#[serde(rename_all=\"snake_case\")]")
        .out_dir("src/codegen/quickwit")
        .compile_with_config(prost_config, &protos, &["protos/quickwit"])?;

    // Prost + tonic + Quickwit codegen for control plane and indexing services.
    //
    // Control plane
    Codegen::run(
        &["protos/quickwit/control_plane.proto"],
        "src/codegen/quickwit",
        "crate::control_plane::Result",
        "crate::control_plane::ControlPlaneError",
        &[],
    )
    .unwrap();

    // Indexing Service
    let mut index_api_config = prost_build::Config::default();
    index_api_config.type_attribute("IndexingTask", "#[derive(Eq, Hash)]");

    Codegen::run_with_config(
        &["protos/quickwit/indexing.proto"],
        "src/codegen/quickwit",
        "crate::indexing::Result",
        "crate::indexing::IndexingError",
        &[],
        index_api_config,
    )
    .unwrap();

    // Jaeger proto
    let protos = find_protos("protos/third-party/jaeger");

    let mut prost_config = prost_build::Config::default();
    prost_config.type_attribute("Operation", "#[derive(Eq, Ord, PartialOrd)]");

    tonic_build::configure()
        .out_dir("src/codegen/jaeger")
        .compile_with_config(
            prost_config,
            &protos,
            &["protos/third-party/jaeger", "protos/third-party"],
        )?;

    // OTEL proto
    let mut prost_config = prost_build::Config::default();
    prost_config.protoc_arg("--experimental_allow_proto3_optional");

    let protos = find_protos("protos/third-party/opentelemetry");
    tonic_build::configure()
        .type_attribute(".", "#[derive(Serialize, Deserialize)]")
        .type_attribute("StatusCode", r#"#[serde(rename_all = "snake_case")]"#)
        .out_dir("src/codegen/opentelemetry")
        .compile_with_config(prost_config, &protos, &["protos/third-party"])?;
    Ok(())
}

fn find_protos(dir_path: &str) -> Vec<PathBuf> {
    glob(&format!("{dir_path}/**/*.proto"))
        .unwrap()
        .flatten()
        .collect()
}
