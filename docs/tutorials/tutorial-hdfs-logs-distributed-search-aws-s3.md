---
title: Distributed search on AWS S3
sidebar_position: 1
---

In this guide, we will index some 40 million log entries (13 GB decompressed) on AWS S3 and start a distributed search server.

Example of a log entry:
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
  }
}
```

:::caution

Before using Quickwit with an object storage, check out our [advice](../administration/cloud-env.md) for deploying on AWS S3 to avoid some bad surprises at the end of the month.

:::


## Prerequisite
- [Configure your environment](configure-aws-env.md) to let Quickwit access your S3 buckets.

All the following steps can be executed on any instance.

## Install

```bash
curl -L https://install.quickwit.io | sh
```


## Create your index

Let's create an index configured to receive these logs.

```bash
# First, download the hdfs logs config from Quickwit repository.
curl -o hdfslogs_index_config.json https://raw.githubusercontent.com/quickwit-inc/quickwit/main/examples/index_configs/hdfslogs_index_config.json
```

The index config defines 5 fields: `timestamp`, `severity_text`, `body`, and two object fields
for the nested values `resource` and `attributes`. 
It also sets the [default search field](../reference/index-config.md) and a timestamp field. 
This timestamp field will be used by Quickwit for [splits pruning](../overview/architecture.md) at query time to boost search speed. Check out the [index config docs](../reference/index-config.md) for details.


```json title="hdfslogs_index_config.json"
{
    "store_source": false, // The document source (=json) will not be stored.
    "default_search_fields": ["body", "severity_text"],
    "timestamp_field": "timestamp",
    "field_mappings": [
        {
            "name": "timestamp",
            "type": "i64",
            "fast": true // Fast field must be present when this is the timestamp field.
        },
        {
            "name": "severity_text",
            "type": "text",
            "tokenizer": "raw" // No tokeninization.
        },
        {
            "name": "body",
            "type": "text",
            "tokenizer": "default",
            "record": "position" // Record position will enable phrase query on body field.
        },
        {
            "name": "resource",
            "type": "object",
            "field_mappings": [
                {
                    "name": "service",
                    "type": "text",
                    "tokenizer": "raw"
                }
            ]
        },
        {
            "name": "attributes",
            "type": "object",
            "field_mappings": [
                {
                    "name": "class",
                    "type": "text",
                    "tokenizer": "raw"
                }
            ]
        }
    ]
}
```

Now we can create the index with the new command directly on S3:

```bash
quickwit new --index-uri s3://path-to-your-bucket/hdfs_logs --index-config-path ./hdfslogs_index_config.json
```

:::note

This step can be executed on your local machine. The `new` command creates the index locally and will then only upload a json file `quickwit.json` to your bucket at `s3://path-to-your-bucket/hdfs_logs/quickwit.json`. 

:::

## Index logs
The dataset is a compressed [ndjson file](https://quickwit-datasets-public.s3.amazonaws.com/hdfs.logs.quickwit.json.gz). Instead of downloading and indexing the data, we will use pipes to send a decompressed stream to Quickwit directly.

```bash
curl https://quickwit-datasets-public.s3.amazonaws.com/hdfs.logs.quickwit.json.gz | gunzip | quickwit index --index-uri s3://path-to-your-bucket/hdfs_logs
```

:::note

4GB of RAM is enough to index this dataset; an instance like `t4g.medium` with 4GB and 2 vCPU indexed this dataset in 20 minutes.   

This step can also be done on your local machine. The `index` command generates locally [splits](../overview/architecture.md) of 5 million documents and will upload them on your bucket. Concretely, each split is represented by a directory in which split index files are saved. Uploading a split is equivalent to uploading 9 files at `s3://path-to-your-bucket/hdfs_logs/{split_id}/`.

:::


You can check it's working by using `search` command and look for `ERROR` in `serverity_text` field:
```bash
quickwit search --index-uri s3://path-to-your-bucket/hdfs_logs --query "severity_text:ERROR"
```


## Start your servers

The command `serve` starts an http server which provides a [REST API](../reference/search-api.md).
Run it on each of your instances.

```bash
quickwit serve --index-uri s3://path-to-your-bucket/hdfs_logs --peer-seed=ip1,ip2,ip3
```

You will see in your terminal the confirmation that the instance has joined the cluster. Example of such a log:

```
INFO quickwit_cluster::cluster: Joined. host_key=019e4c0e-165a-430d-8ef6-7b7a035decac remote_host=Some(172.31.66.143:8081)
```

:::note

Quickwit by default, opens the 8080 port; it also needs the TCP and UDP 8081 port (8080+1) for cluster formation and
finally, 8082 (8080+2) for gRPC communication between instances.

In AWS, you can create a security group to group these inbound rules. Check out the [network section](configure-aws-env.md) of our AWS setup guide.

:::


## Load balancing incoming requests

Now that you have a search cluster, ideally, you will want to load balance external requests. This can quickly be done
by adding an AWS load balancer to listen to incoming HTTP or HTTPS traffic and forward it to a target group.
You can now play with your cluster, kill processes randomly, add/remove new instances, and keep calm.


## Use time pruning

Let's execute a simple query that returns only `ERROR` entries on field `severity_text`:

```bash
curl -v 'http://your-load-balancer/api/v1/hdfs_logs/search?query=severity_text:ERROR
```

which returns the json

```json
{
  "numHits": 364,
  "hits": [
    {
      "attributes.class": [
        "org.apache.hadoop.hdfs.server.datanode.DataNode"
      ],
      "body": [
        "RECEIVED SIGNAL 15: SIGTERM"
      ],
      "resource.service": [
        "datanode/02"
      ],
      "severity_text": [
        "ERROR"
      ],
      "timestamp": [
        1442629246
      ]
    }
    ...
  ],
  "numMicrosecs": 505923
}
```

You can see that this query has only 364 hits and that the server responds in 0.5 seconds.

The index config shows that we can use the timestamp field parameters `startTimestamp` and `endTimestamp` and benefit from time pruning. Behind the scenes, Quickwit will only query [splits](../overview/architecture.md) that have logs in this time range. This can have a significant impact on speed.


```bash
curl -v 'http://your-load-balancer/api/v1/hdfs_logs/search?query=severity_text:ERROR&startTimestamp=1442834249&endTimestamp=1442900000'
```

Returns 6 hits in 0.36 seconds.



## Clean

Let's do some cleanup by deleting the index:

```bash
quickwit delete --index-uri s3://path-to-your-bucket/hdfs_logs
```



Congratz! You finished this tutorial! 

To continue your Quickwit journey, check out the [search REST API reference](../reference/search-api.md) or the [query language reference](../reference/query-language.md).
