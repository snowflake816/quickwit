---
title: Kafka
description: A short tutorial describing how to set up Quickwit to ingest data from Kafka in a few minutes
tags: [kafka, integration]
icon_url: /img/tutorials/kafka.svg
sidebar_position: 2
---

In this tutorial, we will describe how to set up Quickwit to ingest data from Kafka in a few minutes. First, we will create an index and configure a Kafka source. Then, we will create a Kafka topic and load some events from the [GH Archive](https://www.gharchive.org/) into it. Finally, we will execute some search and aggregation queries to explore the freshly ingested data.

## Prerequisites

You will need the following to complete this tutorial:
- A running Kafka cluster (see Kafka [quickstart](https://kafka.apache.org/quickstart))
- A local Quickwit [installation](/docs/get-started/installation.md)

## Create index

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

Execute these Bash commands to download the index config and create the `gh-archive` index:

```bash
# Download GH Archive index config.
wget -O gh-archive.yaml https://raw.githubusercontent.com/quickwit-oss/quickwit/main/config/tutorials/gh-archive/index-config.yaml

# Create index.
./quickwit index create --index-config gh-archive.yaml
```

## Create and populate Kafka topic

Now, let's create a Kafka topic and load some events into it.

```bash
# Create a topic named `gh-archive` with 3 partitions.
bin/kafka-topics.sh --create --topic gh-archive --partitions 3 --bootstrap-server localhost:9092

# Download a few GH Archive files.
wget https://data.gharchive.org/2022-05-12-{10..15}.json.gz

# Load the events into Kafka topic.
gunzip -c 2022-05-12*.json.gz | \
bin/kafka-console-producer.sh --topic gh-archive --bootstrap-server localhost:9092
```

## Create Kafka source

:::note
This tutorial assumes that the Kafka cluster is available locally on the default port (9092). If it's not the case, please, update the `bootstrap.servers` parameter accordingly.
:::

```yaml title="kafka-source.yaml"
#
# Kafka source config file.
#
version: 0.7
source_id: kafka-source
source_type: kafka
max_num_pipelines_per_indexer: 1
desired_num_pipelines: 2
params:
  topic: gh-archive
  client_params:
    bootstrap.servers: localhost:9092
```

Run these commands to download the source config file and create the source.

```bash
# Download Kafka source config.
wget https://raw.githubusercontent.com/quickwit-oss/quickwit/main/config/tutorials/gh-archive/kafka-source.yaml

# Create source.
./quickwit source create --index gh-archive --source-config kafka-source.yaml
```
:::note

If you get the following error:

``` Command failed: Topic `gh-archive` has no partitions.```

It means the Kafka topic `gh-archive` was not properly created in the previous step.

:::



## Launch indexing and search services

Finally, execute this command to start Quickwit in server mode.

```bash
# Launch Quickwit services.
./quickwit run
```

Under the hood, this command spawns an indexer and a searcher. On startup, the indexer will connect to the Kafka topic specified by the source and start streaming and indexing events from the partitions composing the topic. With the default commit timeout value (see [indexing settings](../configuration/index-config#indexing-settings)), the indexer should publish the first split after approximately 60 seconds.

You can run this command (in another shell) to inspect the properties of the index and check the current number of published splits:

```bash
# Display some general information about the index.
./quickwit index describe --index gh-archive
```

Once the first split is published, you can start running search queries. For instance, we can find all the events for the Kubernetes [repository](https://github.com/kubernetes/kubernetes):

```bash
curl 'http://localhost:7280/api/v1/gh-archive/search?query=org.login:kubernetes%20AND%20repo.name:kubernetes'
```

It is also possible to access these results through the [Quickwit UI](http://localhost:7280/ui/search?query=org.login%3Akubernetes+AND+repo.name%3Akubernetes&index_id=gh-archive&max_hits=10).


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
# Delete Kafka topic.
bin/kafka-topics.sh --delete --topic gh-archive --bootstrap-server localhost:9092

# Delete index.
./quickwit index delete --index gh-archive

# Delete source config.
rm kafka-source.yaml
```

This concludes the tutorial. If you have any questions regarding Quickwit or encounter any issues, don't hesitate to ask a [question](https://github.com/quickwit-oss/quickwit/discussions) or open an [issue](https://github.com/quickwit-oss/quickwit/issues) on [GitHub](https://github.com/quickwit-oss/quickwit) or contact us directly on [Discord](https://discord.com/invite/MT27AG5EVE).
