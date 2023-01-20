---
title: Index a logging dataset locally
description: Index log entries and start a three-node cluster on a local machine.
tags: [self-hosted, setup]
icon_url: /img/quickwit-icon.svg
sidebar_position: 1
---

import Tabs from '@theme/Tabs';
import TabItem from '@theme/TabItem';

In this guide, we will index about 40 million log entries (13 GB decompressed) and start a three-node cluster on a local machine. If you want to start a server with indexes on AWS S3, check out the [tutorial for distributed search](tutorial-hdfs-logs-distributed-search-aws-s3.md).


Here is an example of a log entry:
```json
{
  "timestamp": 1460530013,
  "severity_text": "INFO",
  "body": "PacketResponder: BP-108841162-10.10.34.11-1440074360971:blk_1074072698_331874, type=HAS_DOWNSTREAM_IN_PIPELINE terminating",
  "resource": {
    "service": "datanode/01"
  },
  "attributes": {
    "class": "org.apache.hadoop.hdfs.server.datanode.DataNode"
  },
  "tenant_id": 58
}
```


## Install

Let's download and install Quickwit.

```bash
curl -L https://install.quickwit.io | sh
cd quickwit-v*/
```

Or pull and run the Quickwit binary in an isolated Docker container.

```bash
docker run quickwit/quickwit --version
```


## Create your index

Let's create an index configured to receive these logs.

```bash
# First, download the hdfs logs config from Quickwit repository.
curl -o hdfs_logs_index_config.yaml https://raw.githubusercontent.com/quickwit-oss/quickwit/main/config/tutorials/hdfs-logs/index-config.yaml
```

The index config defines five fields: `timestamp`, `tenant_id`, `severity_text`, `body`, and one JSON field
for the nested values `resource.service`, we could use an object field here and maintain a fixed schema, but for convenience we're going to use a JSON field.
It also sets the `default_search_fields`, the `tag_fields`, and the `timestamp_field`.
The `timestamp_field` and `tag_fields` are used by Quickwit for [splits pruning](/docs/concepts/architecture) at query time to boost search speed. 
Check out the [index config docs](/docs/configuration/index-config) for more details.

```yaml title="hdfs-logs-index.yaml"
version: 0.4

index_id: hdfs-logs

doc_mapping:
  field_mappings:
    - name: timestamp
      type: datetime
      input_formats:
        - unix_timestamp
      output_format: unix_timestamp_secs
      precision: seconds
      fast: true
    - name: tenant_id
      type: u64
    - name: severity_text
      type: text
      tokenizer: raw
    - name: body
      type: text
      tokenizer: default
      record: position
    - name: resource
      type: json
      tokenizer: raw
  tag_fields: [tenant_id]
  timestamp_field: timestamp

search_settings:
  default_search_fields: [severity_text, body]
```

Now let's create the index with the `create` subcommand (assuming you are inside Quickwit install directory):

<Tabs>

<TabItem value="cli" label="CLI">

```bash
./quickwit index create --index-config hdfs_logs_index_config.yaml
```

</TabItem>

<TabItem value="docker" label="Docker">

```bash
# Create first the data directory.
mkdir qwdata
docker run -v $(pwd)/qwdata:/quickwit/qwdata -v $(pwd)/hdfs_logs_index_config.yaml:/quickwit/hdfs_logs_index_config.yaml quickwit/quickwit index create --index-config hdfs_logs_index_config.yaml
```

</TabItem>

</Tabs>



You're now ready to fill the index.

## Index logs
The dataset is a compressed [NDJSON file](https://quickwit-datasets-public.s3.amazonaws.com/hdfs-logs-multitenants.json.gz).
Instead of downloading it and then indexing the data, we will use pipes to directly send a decompressed stream to Quickwit.
This can take up to 10 minutes on a modern machine, the perfect time for a coffee break.

<Tabs>

<TabItem value="cli" label="CLI">

```bash
curl https://quickwit-datasets-public.s3.amazonaws.com/hdfs-logs-multitenants.json.gz | gunzip | ./quickwit index ingest --index hdfs-logs
```

</TabItem>

<TabItem value="docker" label="Docker">

```bash
curl https://quickwit-datasets-public.s3.amazonaws.com/hdfs-logs-multitenants.json.gz | gunzip | docker run -v $(pwd)/qwdata:/quickwit/qwdata -i quickwit/quickwit index ingest --index hdfs-logs
```

</TabItem>

</Tabs>



If you are in a hurry, use the sample dataset that contains 10 000 documents, we will use this dataset for the example queries:

<Tabs>

<TabItem value="cli" label="CLI">

```bash
curl https://quickwit-datasets-public.s3.amazonaws.com/hdfs-logs-multitenants-10000.json | ./quickwit index ingest --index hdfs-logs
```

</TabItem>

<TabItem value="docker" label="Docker">

```bash
curl https://quickwit-datasets-public.s3.amazonaws.com/hdfs-logs-multitenants-10000.json | docker run -v $(pwd)/qwdata:/quickwit/qwdata -i quickwit/quickwit index ingest --index hdfs-logs
```

</TabItem>

</Tabs>

You can check it's working by using `search` subcommand and look for `INFO` in `severity_text` field:

<Tabs>

<TabItem value="cli" label="CLI">

```bash
./quickwit index search --index hdfs-logs  --query "severity_text:INFO"
```

</TabItem>

<TabItem value="docker" label="Docker">

```bash
docker run -v $(pwd)/qwdata:/quickwit/qwdata quickwit/quickwit index search --index hdfs-logs  --query "severity_text:INFO"
```

</TabItem>

</Tabs>

:::note

The `ingest` subcommand generates [splits](/docs/concepts/architecture) of 5 million documents. Each split is a small piece of index represented by a file in which index files and metadata files are saved.

:::


## Start your server

The command `run --service searcher --service metastore` starts a http server which provides a [REST API](/docs/reference/rest-api) 
and runs the metastore service which is required by the searcher service.

<Tabs>

<TabItem value="cli" label="CLI">

```bash
./quickwit run --service searcher --service metastore
```

</TabItem>

<TabItem value="docker" label="Docker">

```bash
docker run -v $(pwd)/qwdata:/quickwit/qwdata quickwit/quickwit run --service searcher --service metastore
```

</TabItem>

</Tabs>

Let's execute the same query on field `severity_text` but with `cURL`:

```bash
curl "http://127.0.0.1:7280/api/v1/hdfs-logs/search?query=severity_text:INFO"
```

which returns the json

```json
{
  "num_hits": 10000,
  "hits": [
    {
      "body": "Receiving BP-108841162-10.10.34.11-1440074360971:blk_1073836032_95208 src: /10.10.34.20:60300 dest: /10.10.34.13:50010",
      "resource": {
        "service": "datanode/03"
      },
      "severity_text": "INFO",
      "tenant_id": 58,
      "timestamp": 1440670490
    }
    ...
  ],
  "elapsed_time_micros": 2490
}
```

The index config shows that we can use the timestamp field parameters `start_timestamp` and `end_timestamp` and benefit from time pruning. 
Behind the scenes, Quickwit will only query [splits](/docs/concepts/architecture) that have logs in this time range.

Let's use these parameters with the following query:

```bash
curl 'http://127.0.0.1:7280/api/v1/hdfs-logs/search?query=severity_text:INFO&start_timestamp=1440670490&end_timestamp=1450670490'
```


## Clean

Let's do some cleanup by deleting the index:

<Tabs>

<TabItem value="cli" label="CLI">

```bash
./quickwit index delete --index hdfs-logs
```

</TabItem>

<TabItem value="docker" label="Docker">

```bash
docker run -v $(pwd)/qwdata:/quickwit/qwdata quickwit/quickwit index delete --index hdfs-logs
```

</TabItem>

</Tabs>

Congratz! You finished this tutorial!


To continue your Quickwit journey, check out the [tutorial for distributed search](tutorial-hdfs-logs-distributed-search-aws-s3.md) or dig into the [search REST API](/docs/reference/rest-api) or [query language](/docs/reference/query-language).

