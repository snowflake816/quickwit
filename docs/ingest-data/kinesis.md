---
title: Kinesis
description: A short tutorial describing how to set up Quickwit to ingest data from Kinesis in a few minutes
tags: [aws, integration]
icon_url: /img/tutorials/aws-kinesis.svg
sidebar_position: 4
---

In this tutorial, we will describe how to set up Quickwit to ingest data from Kinesis in a few minutes. First, we will create an index and configure a Kinesis source. Then, we will create a Kinesis stream and load some events from the [GH Archive](https://www.gharchive.org/) into it. Finally, we will execute some search and aggregation queries to explore the freshly ingested data.

:::caution
You will incur some charges for using the Amazon Kinesis service during this tutorial.
:::

## Prerequisites

You will need the following to complete this tutorial:
- The AWS CLI version 2 (see [Getting started with the AWS CLI](https://docs.aws.amazon.com/cli/latest/userguide/getting-started-prereqs.html) for prerequisites and installation)
- A local Quickwit [installation](/docs/get-started/installation.md)
- [jq](https://stedolan.github.io/jq/download/)
- [GNU parallel](https://www.gnu.org/software/parallel/)

:::note
`jq` is required to reshape the events into records ingestable by the Amazon Kinesis API.
:::

### Create index

First, let's create a new index. Here is the index config and doc mapping corresponding to the schema of the GH Archive events:

```yaml title="index-config.yaml"
#
# Index config file for gh-archive dataset.
#
version: 0.7

index_id: gh-archive

doc_mapping:
  field_mappings:
    - name: id
      type: text
      tokenizer: raw
    - name: type
      type: text
      fast: true
      tokenizer: raw
    - name: public
      type: bool
      fast: true
    - name: payload
      type: json
      tokenizer: default
    - name: org
      type: json
      tokenizer: default
    - name: repo
      type: json
      tokenizer: default
    - name: actor
      type: json
      tokenizer: default
    - name: other
      type: json
      tokenizer: default
    - name: created_at
      type: datetime
      fast: true
      input_formats:
        - rfc3339
      fast_precision: seconds
  timestamp_field: created_at

indexing_settings:
  commit_timeout_secs: 10

```

Execute these Bash commands to download the index config and create the `gh-archive` index.

```bash
# Download GH Archive index config.
wget -O gh-archive.yaml https://raw.githubusercontent.com/quickwit-oss/quickwit/main/config/tutorials/gh-archive/index-config.yaml

# Create index.
./quickwit index create --index-config gh-archive.yaml
```


## Create and populate Kinesis stream

Now, let's create a Kinesis stream and load some events into it.

:::tip
This step may be fairly slow depending on how much bandwidth is available. The current command limits the volume of data to ingest by taking the first 10 000 lines of every single file downloaded from the GH Archive. If you have enough bandwidth, you can remove it to ingest the whole set of files. You can also speed things up by increasing the number of shards and/or the number of jobs launched by `parallel` (`-j` option).
:::

```bash
# Create a stream named `gh-archive` with 3 shards.
aws kinesis create-stream --stream-name gh-archive --shard-count 8

# Download a few GH Archive files.
wget https://data.gharchive.org/2022-05-12-{10..12}.json.gz

# Load the events into Kinesis stream
gunzip -c 2022-05-12*.json.gz | \
head -n 10000 | \
parallel --gnu -j8 -N 500 --pipe \
'jq --slurp -c "{\"Records\": [.[] | {\"Data\": (. | tostring), \"PartitionKey\": .id }], \"StreamName\": \"gh-archive\"}" > records-{%}.json && \
aws kinesis put-records --cli-input-json file://records-{%}.json --cli-binary-format raw-in-base64-out >> out.log'
```

## Create Kinesis source

```yaml title="kinesis-source.yaml"
#
# Kinesis source config file.
#
version: 0.7
source_id: kinesis-source
source_type: kinesis
params:
  stream_name: gh-archive
```

Run these commands to download the source config file and create the source.

```bash
# Download Kinesis source config.
wget https://raw.githubusercontent.com/quickwit-oss/quickwit/main/config/tutorials/gh-archive/kinesis-source.yaml

# Create source.
./quickwit source create --index gh-archive --source-config kinesis-source.yaml
```

:::note

If this command fails with the following error message:
```
Command failed: Stream gh-archive under account XXXXXXXXX not found.

Caused by:
    0: Stream gh-archive under account XXXXXXXX not found.
    1: Stream gh-archive under account XXXXXXXX not found.
```

it means the Kinesis stream was not properly created in the previous step.
:::

## Launch indexing and search services

Finally, execute this command to start Quickwit in server mode.

```bash
# Launch Quickwit services.
./quickwit run
```

Under the hood, this command spawns an indexer and a searcher. On startup, the indexer will connect to the Kinesis stream specified by the source and start streaming and indexing events from the shards composing the stream. With the default commit timeout value (see [indexing settings](../configuration/index-config#indexing-settings)), the indexer should publish the first split after approximately 60 seconds.

You can run this command (in another shell) to inspect the properties of the index and check the current number of published splits:

```bash
# Display some general information about the index.
./quickwit index describe --index gh-archive
```

It is also possible to get index information through the [Quickwit UI](http://localhost:7280/ui/indexes/gh-archive).

Once the first split is published, you can start running search queries. For instance, we can find all the events for the Kubernetes [repository](https://github.com/kubernetes/kubernetes):

```bash
curl 'http://localhost:7280/api/v1/gh-archive/search?query=org.login:kubernetes%20AND%20repo.name:kubernetes'
```

It is also possible to access these results through the [UI](http://localhost:7280/ui/search?query=org.login%3Akubernetes+AND+repo.name%3Akubernetes&index_id=gh-archive&max_hits=10).

We can also group these events by type and count them:

```
curl -XPOST -H 'Content-Type: application/json' 'http://localhost:7280/api/v1/gh-archive/search' -d '
{
  "query":"org.login:kubernetes AND repo.name:kubernetes",
  "max_hits":0,
  "aggs":{
    "count_by_event_type":{
      "terms":{
        "field":"type"
      }
    }
  }
}'
```

## Tear down resources (optional)

Let's delete the files and resources created for the purpose of this tutorial.

```bash
# Delete Kinesis stream.
aws kinesis delete-stream --stream-name gh-archive

# Delete index.
./quickwit index delete --index gh-archive

# Delete source config.
rm kinesis-source.yaml
```

This concludes the tutorial. If you have any questions regarding Quickwit or encounter any issues, don't hesitate to ask a [question](https://github.com/quickwit-oss/quickwit/discussions) or open an [issue](https://github.com/quickwit-oss/quickwit/issues) on [GitHub](https://github.com/quickwit-oss/quickwit) or contact us directly on [Discord](https://discord.com/invite/MT27AG5EVE).
