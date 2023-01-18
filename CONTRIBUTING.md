# Contributing to Quickwit
There are many ways to contribute to Quickwit.
Code contribution are welcome of course, but also
bug reports, feature request, and evangelizing are as valuable.

# Submitting a PR
Check if your issue is already listed [github](https://github.com/quickwit-oss/quickwit/issues).
If it is not, create your own issue.

Please add the following phrase at the end of your commit.  `Closes #<Issue Number>`.
It will automatically link your PR in the issue page. Also, once your PR is merged, it will
closes the issue. If your PR only partially addresses the issue and you would like to
keep it open, just write `See #<Issue Number>`.

Feel free to send your contribution in an unfinished state to get early feedback.
In that case, simply mark the PR with the tag [WIP] (standing for work in progress).

# Signing the CLA
Quickwit is an opensource project licensed a AGPLv3.
It is also distributed under a commercial license by Quickwit, Inc.

Contributors are required to sign a Contributor License Agreement.
The process is simple and fast. Upon your first pull request, you will be prompted to
[sign our CLA by visiting this link](https://cla-assistant.io/quickwit-oss/quickwit).

# Development
## Setup & run tests
1. Install Rust, CMake, Docker (https://docs.docker.com/engine/install/) and Docker Compose (https://docs.docker.com/compose/install/)
2. Install node@16 and `npm install -g yarn`
3. Install awslocal https://github.com/localstack/awscli-local
4. Install protoc https://grpc.io/docs/protoc-installation/ (you may need to install the latest binaries rather than your distro's flavor)
5. In the project's root directory start the external services with `make docker-compose-up`
6. Switch to the `quickwit` subdirectory
7. Run `QW_S3_ENDPOINT=http://localhost:4566 AWS_ACCESS_KEY_ID=ignored AWS_SECRET_ACCESS_KEY=ignored cargo test --all-features`
8. Run UI tests `yarn --cwd quickwit-ui install` and `yarn --cwd quickwit-ui test`

## Start the UI
1. Switch to the `quickwit` subdirectory of the project and create a data directory `qwdata` there if it doesn't exist
2. Start a server `cargo r run --config ../config/quickwit.yaml`
3. `yarn --cwd quickwit-ui install` and `yarn --cwd quickwit-ui start`
4. Open your browser at `http://localhost:3000/ui` if it doesn't open automatically

## Running UI e2e tests
1. Ensure to run a searcher `cargo r run --service searcher --config ../config/quickwit.yaml`
2. Run `yarn --cwd quickwit-ui e2e-test`

## Running services such as Amazon Kinesis or S3, Kafka, or PostgreSQL locally.
1. Ensure Docker and Docker Compose are correctly installed on your machine (see above)
2. Run `make docker-compose-up` to launch all the services or `make docker-compose-up DOCKER_SERVICES=kafka,postgres` to launch a subset of services.

## Tracing with Jaeger
1. Ensure Docker and Docker Compose are correctly installed on your machine (see above)
2. Start the Jaeger services (UI, collector, agent, ...) running the command `make docker-compose-up DOCKER_SERVICES=jaeger`
3. Start Quickwit with the following environment variables:
```
QW_ENABLE_JAEGER_EXPORTER=true
OTEL_BSP_MAX_EXPORT_BATCH_SIZE=8
```

If you are on MacOS, the default UDP packet size is 9216 bytes which is too low compared to the jaeger exporter max size set by default at 65000 bytes. As a workaround, you can increase the limit at your own risk: `sudo sysctl -w net.inet.udp.maxdgram=65535`.

The `OTEL_BSP_MAX_EXPORT_BATCH_SIZE` is the key parameter, it sets the maximum number of spans sent to Jaeger in one batch. Quickwit tends to produce spans of relatively big size and if the batch size is greater than the maximum UDP packet size, the sending of the batch to Jaeger will fail and the following error will appear in the logs: 

```
OpenTelemetry trace error occurred. Exporter jaeger encountered the following error(s): thrift agent failed with transport error
```

Ref: https://github.com/open-telemetry/opentelemetry-rust/issues/851


4. Open your browser and visit [localhost:16686](http://localhost:16686/)

## Using tokio console
1. Install tokio-console by running `cargo install tokio-console`.
2. Install the quickwit binary in the quickwit-cli folder `RUSTFLAGS="--cfg tokio_unstable" cargo install --path . --features tokio-console`
3. Launch a long running command such as index and activate tokio with the: `QW_TOKIO_CONSOLE_ENABLED=1 quickwit index ...`
4. Run `tokio-console`.

## Building binaries

Currently, we use [cross](https://github.com/rust-embedded/cross) to build Quickwit binaries for different architectures.
For this to work, we've had to customize the docker images cross uses. These customizations can be found in docker files located in `./cross-images` folder. To make cross take into account any change on those
docker files, you will need to build and push the images on Docker Hub by running `make cross-images`.
We also have nightly builds that are pushed to Docker Hub. This helps continuously check our binaries are still built even with external dependency update. Successful builds let you accessed the artifacts for the next three days. Release builds always have their artifacts attached to the release.

## Docker images

Each merge on the `main` branch triggers the build of a new Docker image available on DockerHub at `quickwit/quickwit:edge`. Tagging a commit also creates a new image `quickwit/quickwit:<tag name>` if the tag name starts with `v*` or `qw*`. The Docker images are based on Debian.

### Notes on the embedded UI
As the react UI is embedded in the rust binary, we need to build the react app before building the binary. Hence `make cross-image` depends on the command `build-ui`.

## Testing release (alpha, beta, rc)

The following Quickwit installation command `curl -L https://install.quickwit.io | sh` always installs the latest stable version of quickwit. To make it easier in installing and testing new (alpha, beta, rc) releases, you can manually pull and execute the script as `./install.sh --allow-any-latest-version`. This will force the script to install any latest available release package.


# Documentation

Quickwit documentation is located in the docs directory.

## Generating the CLI docs.

The [CLI doc page](docs/reference/cli.md) is partly generated by a script.
To update it, first run the script:
```bash
cargo run --bin generate_markdown > docs/reference/cli_insert.md
```

Then manually edit the [doc page](docs/reference/cli.md) to update it.
I put two comments to indicate where you want to insert the new docs and where it ends:`
```markdown
[comment]: <> (Insert auto generated CLI docs from here.)

...docs to insert...

[comment]: <> (End of auto generated CLI docs.)
```

