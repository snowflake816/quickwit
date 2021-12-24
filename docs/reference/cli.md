---
title: CLI Reference
sidebar_position: 1
---

Quickwit is a single binary that makes it easy to index and search structured or unstructured data from the command line. It consumes datasets consisting of newline-delimited JSON objects with arbitrary keys. It produces indexes that can be stored locally or remotely on an object storage such as Amazon S3 and queried with subsecond latency.

This page documents all the available commands, related options, and environment variables.

:::caution

Before using Quickwit with an object storage, check out our [advice](../administration/cloud-env.md) for deploying on AWS S3 to avoid some nasty surprises at the end of the month.

:::


## Commands

[Command-line synopsis syntax](https://developers.google.com/style/code-syntax)

### Help

`quickwit` or `quickwit help` displays the list of available commands.

`quickwit help <command name>` displays the documentation for the command and a usage example.

#### Note on telemetry
Quickwit collects some [anonymous usage data](telemetry.md), you can disable it. When it's enabled you will see this
output:
```
quickwit help
Quickwit 0.1.0
Quickwit, Inc. <hello@quickwit.io>
Indexing your large dataset on object storage & making it searchable from the command line.
Telemetry enabled
[...]
```

The line `Telemetry enabled` disappears when you disable it.


### Version

`quickwit --version` displays the version. It is useful for reporting bugs.



### New

*Description*

Creates an index at `index-uri` configured by a json file located at `index-config-path`. The command fails if an index already exists at `index-uri` unless `overwrite` is passed. When `overwrite` is enabled, the command deletes all the files stored at `index-uri` before creating a new index. The index config defines how a document and fields it contains, are stored and indexed, see the [index config documentation](index-config.md).

*Synopsis*

```bash
quickwit new
    --index-uri <uri>
    --index-config-path <path>
    [--overwrite]
```

*Options*

`--index-uri` (string) Defines the index location.<br />
`--index-config-path` (string) Defines the index config path.<br />
`--overwrite` (boolean) Overwrites existing index.

*Examples*

*Creating a new index on local file system*

```bash
quickwit new --index-uri file:///quickwit-indexes/catalog --index-config-path ~/quickwit-conf/index_config.json
```

Creating a new index on Amazon S3*

```bash
quickwit new --index-uri s3://quickwit-indexes/catalog --index_config-path ~/quickwit-conf/index_config.json
```

*Replacing an existing index*

```bash
quickwit new --index-uri s3://quickwit-indexes/catalog --index_config-path ~/quickwit-conf/index_config.json --overwrite
```

:::note

When creating an index on a local file system, absolute path is enforce. This implies that index-uri like `file:///quickwit-indexes/catalog` pertenains you have the required permissions on `/quickwit-indexes/catalog`.

:::

### Index

*Description*

Indexes a dataset consisting of newline-delimited JSON objects located at `input-path` or read from *stdin*. The data is appended to the target index specified by `index-uri` unless `overwrite` is passed. `input-path` can be a file or another command output piped into stdin. Currently, only local datasets are supported. By default, tantivy's indexer will work with a heap of 1 GiB of memory, but this can be set with the `heap-size` options. This does not directly reflect the overall memory usage of `quickwit index`, but doubling this value should give a fair approximation.


*Synopsis*

```bash
quickwit index
    --index-uri <uri>
    [--input-path <path>]
    [--overwrite]
    [--heap-size <num bytes>]
    [--temp-dir]
```

*Options*

`--index-uri` (string) Location of the target index.<br />
`--input-path` (string) Location of the source dataset.<br />
`--overwrite` (boolean) Overwrites existing data.<br />
`--heap-size` (integer) Amount of allocated memory for the process.<br />
`--temp-dir` (string) Path of temporary directory for building the index (defaults to `/tmp`)

*Examples*

*Indexing a local dataset*

```bash
quickwit index --index-uri s3://quickwit-indexes/nginx --input-path nginx.json
```

*Indexing a dataset from stdin*

```bash
cat nginx.json | quickwit index --index-uri s3://quickwit-indexes/nginx
quickwit index --index-uri s3://quickwit-indexes/nginx < nginx.json
```

*Reindexing a dataset*

```bash
quickwit index --index-uri s3://quickwit-indexes/nginx --input-path nginx.json --overwrite
```

*Customizing the resources allocated to the program*

```bash
quickwit index --index-uri s3://quickwit-indexes/nginx --input-path nginx.json --heap-size 4GiB
```

### Search

*Description*

Searches the index stored at `index-uri` and returns the documents matching the query specified with `query`. The offset of the first hit returned and the number of hits returned can be set with the `start-offset` and `max-hits` options. Given the query doesn't explicitly contains fields, it's possible to restrict the search on specified fields using the `search-fields` option. Search can also be limited to a time range using the `start-timestamp` and `end-timestamp` options. These timestamp options can particularly be useful in boosting query performance when using a time series dataset and only need to query a particular window.

*Synopsis*

```bash
quickwit search
    --index-uri <uri>
    --query <query>
    [--max-hits <n>]
    [--start-offset <offset>]
    [--search-fields <comma-separated list of fields>]
    [--start-timestamp <i64>]
    [--end-timestamp <i64>]
```

*Options*

`--index-uri` (string) Location of the target index.<br />
`--query` (string) Query expressed in Tantivy syntax.<br />
`--max-hits` (integer) Maximum number of hits returned (defaults to `20`).<br />
`--start-offset` (integer) Skips the first `start-offset` hits (defaults to `0`).<br />
`--search-fields` (string) Search only on this comma-separated list of field names.<br />
`--start-timestamp` (string) Inclusive lower bound.<br />
`--end-timestamp` (string) Exclusive upper bound.<br />

*Examples*

*Searching a local index*

```bash
quickwit search --index-uri file:///path-to-my-indexes/wikipedia --query "Barack Obama"
```

*Searching a remote index*

```bash
quickwit search --index-uri s3://quickwit-indexes/wikipedia --query "Barack Obama"
```

*Limiting the result set to 50 hits*

```bash
quickwit search --index-uri s3://quickwit-indexes/wikipedia --query "Barack Obama" --max-hits 50
```

*Skipping the first 20 hits*

```bash
quickwit search --index-uri s3://quickwit-indexes/wikipedia --query "Barack Obama" --start-offset 20
```

*Looking for matches in the title and url fields only*

```bash
quickwit search --index-uri s3://quickwit-indexes/wikipedia --query "Barack Obama" --search-fields title,url
```

### Serve

*Description*

Starts a web server listening on `host`:`port` that exposes the [Quickwit REST API](search-api.md). The `index-uri` option, which accepts a comma-separated list of index URIs, specifies the indexes targeted by the API. The node can optionally join a cluster using the `peer-seed` parameter. This list of comma-separated node addresses is used to discover the remaining peer nodes in the cluster through the use of a gossip protocol (SWIM).

:::note

Quickwit services run on three TCP ports ranging from `port` to `port` + 2, and one UDP port (`port` + 1):
- the web server listens on the first TCP port (default: 8080);
- the cluster management service uses the second TCP port (default: 8081) and the UDP port with the same number;
- gRPC services run on the third TCP port (default: 8082).

If any of those ports is already in use when the services start, the command will fail.
:::


*Synopsis*

```bash
quickwit serve
    --index-uri <list of URIs>
    --host <hostname>
    --port <port>
    --peer-seed <list of addresses>
```

*Options*

`--index-uri` (string) Comma-separated list of target index locations.<br />
`--host` (string) Hostname the web server should bind to.<br />
`--port` (string) Port the web server should bind to.<br />
`--peer-seed` (string) Comma-separated list of node addresses (e.g. 10.0.0.1:8080) used as seeds for cluster peer discovery.<br />


*Examples*

*Serving a local index*

```bash
quickwit serve --index-uri file:///my-indexes/wikipedia
```

*Serving a remote index*

```bash
quickwit serve --index-uri s3://my-bucket/nginx-logs
```

*Serving multiple indexes*

```bash
quickwit serve --index-uri file:///my-indexes/wikipedia,s3://my-bucket/nginx-logs
```

*Creating a multi-node cluster*

```bash
# On host 10.0.0.1
quickwit serve --index-uri s3:///my-bucket/nginx-logs --peer-seed 10.0.0.2:8080

# On host 10.0.0.2
quickwit serve --index-uri s3:///my-bucket/nginx-logs --peer-seed 10.0.0.1:8080

# On host 10.0.0.3
quickwit serve --index-uri s3:///my-bucket/nginx-logs --peer-seed 10.0.0.1:8080,10.0.0.2.8080

# On host 10.0.0.4
quickwit serve --index-uri s3:///my-bucket/nginx-logs --peer-seed 10.0.0.1:8080,10.0.0.2.8080
```

### Delete

*Description*

Deletes the index at `index-uri`.

*Synopsis*

```bash
quickwit delete
    --index-uri <uri>
    [--dry-run]
```

*Options*

`--index-uri` (string) Location of the target index.<br />
`--dry-run` (boolean) Executes the command in dry run mode and displays the list of files subject to be deleted.<br />

*Examples*

*Deleting an index*
```bash
quickwit delete --index-uri s3://quickwit-indexes/catalog
```

*Executing in dry run mode*
```bash
quickwit delete --index-uri s3://quickwit-indexes/catalog --dry-run
```

### Garbage collect (gc)

*Description*

Garbage collects all dangling files within the index at `index-uri`.

*Synopsis*

```bash
quickwit gc
    --index-uri <uri>
    [--grace-period <duration>]
    [--dry-run]
```

:::note

Intermediate files are created while executing Quickwit commands. These intermediate files are always cleaned at the end of each successfully executed command. However, failed or interrupted commands can leave behind intermediate files that need to be removed.
Also note that using very short grace-period (like seconds) can cause removal of intermediate files being operated on especially when using Quickwit concurently on the same index. In practice you can settle with the default value (1 hour) and only specify a value if you really know what you are doing.

:::

*Options*

`--index-uri` (string) Location of the target index.<br />
`--grace-period` (string) Threshold period after which intermediate files can be garbage collected. This is an integer followed by one of the letters `s`(second), `m`(minutes), `h`(hours) and `d`(days) as unit, (defaults to `1h`).<br />
`--dry-run` (boolean) Executes the command in dry run mode and displays the list of files subject to be removed.<br />

*Examples*

*Garbage collecting an index*
```bash
quickwit gc --index-uri s3://quickwit-indexes/catalog
```

*Executing in dry run mode*
```bash
quickwit gc --index-uri s3://quickwit-indexes/catalog --dry-run
```

*Executing with five minutes of grace period*
```bash
quickwit gc --index-uri s3://quickwit-indexes/catalog --grace-period 5m
```

## Environment Variables

### QW_ENV

Specifies the nature of the current working environment. Currently, this environment variable is used exclusively for testing purposes, and `LOCAL` is the only supported value.

### QW_DISABLE_TELEMETRY

Disables [telemetry](telemetry.md) when set to any non-empty value.
