# quickwit-serve

This project hosts the REST, the gRPC API associated with quickwit and the react UI. 

## REST and gRPC API

The API is split into:
- the search API: the normal and the stream search api;
- the index management API: create, delete, list indexes and list splits of an index;
- the ingest API;
- the cluster API: expose information about the cluster, its members etc;
- the health check API: the health check of the current node. This API is rest only at the moment.

The APIs are usually accessible both via gRPC and REST.
This is done consistently using the following pattern.

A service async trait mimicks the tonic service api, but without the `tonic`
wrapping of the request and with a rich and specific error type instead of tonic::Status.
The argument and the response on the other hand are typically using protobuf object
directly whenever sensible.

This service only has one implementation but is a trait for mocking purpose.
This service is typically exposed by another crate, specific to the API considered.
For instance, the search api has a `SearchService` trait, using the `SearchError`
response in the `quickwit-search` crate.

An adapter then wraps this service to implement the grpc::Service
(It simply does the wrapping of the request / results and converts errors to the tonic status.).

The rest API then relies on calling this service.

```mermaid
graph TD
    grpc_ser[grpc::BlopService] --> |wraps| ser(Go shopping)
    rest[blop_handler] --> |calls| ser(Go shopping)
    ser[BlopService]
```

## UI

The server also exposes at `/ui` all static files located in `quickwit-ui/build` directory. These static files are
produced by the react app build in `quickwit-ui`.
During development, the server will serve the local files. When building the binary, these static files will be embedded in it.

## Elastic Compatible API

To help implement Elasticsearch compatible API in Quickwit, we generate endpoints and related types using the [elastic-api-generator](./src/elastic_api_generator.rs) binary.

To update the generated file, you will have to: 
- Download the spec files from the official Elasticsearch repository by running `cargo run --bin elastic-api-generator download` command.
- Include the endpoint spec file if you want to support new endpoints (optional).
- Generate the corresponding rust code by running `cargo run --bin elastic-api-generator generate` command.

The generated code is located at `quickwit-serve/src/elastic_search_api/api_specs.rs`. `quickwit-serve/src/elastic_search_api/rest_handler.rs` is then used to create the warp endpoint handlers by using the generated warp filters.
