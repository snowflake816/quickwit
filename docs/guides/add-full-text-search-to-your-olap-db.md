---
title: Full-text search on ClickHouse
description: Add full-text search to ClickHouse, using the Quickwit search streaming feature.
tags: [clickhouse, integration]
icon_url: /img/tutorials/clickhouse.svg
sidebar_position: 10
---


This guide will help you add full-text search to a well-known OLAP database, ClickHouse, using the Quickwit search streaming feature. Indeed Quickwit exposes a REST endpoint that streams ids or whatever attributes matching a search query **extremely fast** (up to 50 million in 1 second), and ClickHouse can easily use them with joins queries.

We will take the [GitHub archive dataset](https://www.gharchive.org/), which gathers more than 3 billion GitHub events: `PullRequestEvent`, `IssuesEvent`... You can dive into this [great analysis](https://ghe.clickhouse.tech/) made by ClickHouse to have a good understanding of the dataset. We also took strong inspiration from this work, and we are very grateful to them for sharing this.

## Install

```bash
curl -L https://install.quickwit.io | sh
cd quickwit-v*/
```

## Start a Quickwit server

```bash
./quickwit run
```

## Create a Quickwit index

After [starting Quickwit], we need to create an index configured to receive these events.  Let's first look at the data to ingest. Here is an event example:

```JSON
{
  "id": 11410577343,
  "event_type": "PullRequestEvent",
  "actor_login": "renovate[bot]",
  "repo_name": "dmtrKovalenko/reason-date-fns",
  "created_at": 1580515200000,
  "action": "closed",
  "number": 44,
  "title": "Update dependency rollup to ^1.31.0",
  "labels": [],
  "ref": null,
  "additions": 5,
  "deletions": 5,
  "commit_id": null,
  "body":"This PR contains the following updates..."
}
```

We don't need to index all fields described above as `title` and `body` are the fields of interest for our full-text search tutorial. 
The `id` will be helpful for making the JOINs in ClickHouse, `created_at` and `event_type` may also be beneficial for timestamp pruning and filtering.

```yaml title="gh-archive-index-config.yaml"
version: 0.6
index_id: gh-archive
# By default, the index will be stored in your data directory,
# but you can store it on s3 or on a custom path as follows:
# index_uri: s3://my-bucket/gh-archive
# index_uri: file://my-big-ssd-harddrive/
doc_mapping:
  store_source: false
  field_mappings:
    - name: id
      type: u64
      fast: true
    - name: created_at
      type: datetime
      input_formats:
        - unix_timestamp
      output_format: unix_timestamp_secs
      fast_precision: seconds
      fast: true
    - name: event_type
      type: text
      tokenizer: raw
    - name: title
      type: text
      tokenizer: default
      record: position
    - name: body
      type: text
      tokenizer: default
      record: position
  timestamp_field: created_at

search_settings:
  default_search_fields: [title, body]
```

```bash
curl -o gh-archive-index-config.yaml https://raw.githubusercontent.com/quickwit-oss/quickwit/main/config/tutorials/gh-archive/index-config-for-clickhouse.yaml
./quickwit index create --index-config gh-archive-index-config.yaml
```

## Indexing events

The dataset is a compressed [NDJSON file](https://quickwit-datasets-public.s3.amazonaws.com/gh-archive/gh-archive-2021-12.json.gz).
Let's index it.

```bash
wget https://quickwit-datasets-public.s3.amazonaws.com/gh-archive/gh-archive-2021-12-text-only.json.gz
gunzip -c gh-archive-2021-12-text-only.json.gz | ./quickwit index ingest --index gh-archive
```

You can check it's working by using the `search` command and looking for `tantivy` word:
```bash
./quickwit index search --index gh-archive --query "tantivy"
```


## Streaming IDs

We are now ready to fetch some ids with the search stream endpoint. Let's start by streaming them on a simple
query and with a `csv` output format.

```bash
curl "http://127.0.0.1:7280/api/v1/gh-archive/search/stream?query=tantivy&output_format=csv&fast_field=id"
```

We will use the `click_house` binary output format in the following sections to speed up queries.


## ClickHouse

Let's leave Quickwit for now and [install ClickHouse](https://clickhouse.com/docs/en/install). Start a ClickHouse server.

### Create database and table

Once installed, just start a client and execute the following sql statements:
```SQL
CREATE DATABASE "gh-archive";
USE "gh-archive";


CREATE TABLE github_events
(
    id UInt64,
    event_type Enum('CommitCommentEvent' = 1, 'CreateEvent' = 2, 'DeleteEvent' = 3, 'ForkEvent' = 4,
                    'GollumEvent' = 5, 'IssueCommentEvent' = 6, 'IssuesEvent' = 7, 'MemberEvent' = 8,
                    'PublicEvent' = 9, 'PullRequestEvent' = 10, 'PullRequestReviewCommentEvent' = 11,
                    'PushEvent' = 12, 'ReleaseEvent' = 13, 'SponsorshipEvent' = 14, 'WatchEvent' = 15,
                    'GistEvent' = 16, 'FollowEvent' = 17, 'DownloadEvent' = 18, 'PullRequestReviewEvent' = 19,
                    'ForkApplyEvent' = 20, 'Event' = 21, 'TeamAddEvent' = 22),
    actor_login LowCardinality(String),
    repo_name LowCardinality(String),
    created_at Int64,
    action Enum('none' = 0, 'created' = 1, 'added' = 2, 'edited' = 3, 'deleted' = 4, 'opened' = 5, 'closed' = 6, 'reopened' = 7, 'assigned' = 8, 'unassigned' = 9,
                'labeled' = 10, 'unlabeled' = 11, 'review_requested' = 12, 'review_request_removed' = 13, 'synchronize' = 14, 'started' = 15, 'published' = 16, 'update' = 17, 'create' = 18, 'fork' = 19, 'merged' = 20),
    comment_id UInt64,
    body String,
    ref LowCardinality(String),
    number UInt32,
    title String,
    labels Array(LowCardinality(String)),
    additions UInt32,
    deletions UInt32,
    commit_id String
) ENGINE = MergeTree ORDER BY (event_type, repo_name, created_at);
```

### Import events

We have created a second dataset, `gh-archive-2021-12.json.gz`, which gathers all events, even ones with no
text. So it's better to insert it into ClickHouse, but if you don't have the time, you can use the dataset
`gh-archive-2021-12-text-only.json.gz` used for Quickwit.

```bash
wget https://quickwit-datasets-public.s3.amazonaws.com/gh-archive/gh-archive-2021-12.json.gz
gunzip -c gh-archive-2021-12.json.gz | clickhouse-client -d gh-archive --query="INSERT INTO github_events FORMAT JSONEachRow"
```

Let's check it's working:
```SQL
# Top repositories by stars
SELECT repo_name, count() AS stars
FROM github_events
GROUP BY repo_name
ORDER BY stars DESC LIMIT 5

┌─repo_name─────────────────────────────────┬─stars─┐
│ test-organization-kkjeer/app-test-2       │ 16697 │
│ test-organization-kkjeer/bot-validation-2 │ 15326 │
│ microsoft/winget-pkgs                     │ 14099 │
│ conda-forge/releases                      │ 13332 │
│ NixOS/nixpkgs                             │ 12860 │
└───────────────────────────────────────────┴───────┘
```

### Use Quickwit search inside ClickHouse

ClickHouse has an exciting feature called [URL Table Engine](https://clickhouse.com/docs/en/engines/table-engines/special/url/) that queries data from a remote HTTP/HTTPS server.
This is precisely what we need: by creating a table pointing to Quickwit search stream endpoint, we will fetch ids that match a query from ClickHouse.

```SQL
SELECT count(*) FROM url('http://127.0.0.1:7280/api/v1/gh-archive/search/stream?query=log4j+OR+log4shell&fast_field=id&output_format=click_house_row_binary', RowBinary, 'id UInt64')

┌─count()─┐
│  217469 │
└─────────┘

1 row in set. Elapsed: 0.068 sec. Processed 217.47 thousand rows, 1.74 MB (3.19 million rows/s., 25.55 MB/s.)
```

We are fetching 217 469 u64 ids in 0.068 seconds. That's 3.19 million rows per second, not bad. And it's possible to increase the throughput if fast field are already cached.


Let's do another example with a more exciting query that will match `log4j` or `log4shell` and count events per day:

```SQL
SELECT
    count(*),
    toDate(fromUnixTimestamp64Milli(created_at)) AS date
FROM github_events
WHERE id IN (
    SELECT id
    FROM url('http://127.0.0.1:7280/api/v1/gh-archive/search/stream?query=log4j+OR+log4shell&fast_field=id&output_format=click_house_row_binary', RowBinary, 'id UInt64')
)
GROUP BY date

Query id: 10cb0d5a-7817-424e-8248-820fa2c425b8

┌─count()─┬───────date─┐
│      96 │ 2021-12-01 │
│      66 │ 2021-12-02 │
│      70 │ 2021-12-03 │
│      62 │ 2021-12-04 │
│      67 │ 2021-12-05 │
│     167 │ 2021-12-06 │
│     140 │ 2021-12-07 │
│     104 │ 2021-12-08 │
│     157 │ 2021-12-09 │
│   88110 │ 2021-12-10 │
│    2937 │ 2021-12-11 │
│    1533 │ 2021-12-12 │
│    5935 │ 2021-12-13 │
│  118025 │ 2021-12-14 │
└─────────┴────────────┘

14 rows in set. Elapsed: 0.124 sec. Processed 8.35 million rows, 123.10 MB (67.42 million rows/s., 993.55 MB/s.)

```

We can see two spikes on the 2021-12-10 and 2021-12-14.

## Wrapping up

We have just scratched the surface of full-text search from ClickHouse with this small subset of GitHub archive. 
You can play with the complete dataset that you can download from our public S3 bucket.
We have made available monthly gzipped ndjson files from 2015 until 2021. Here are `2015-01` links:
- full JSON dataset https://quickwit-datasets-public.s3.amazonaws.com/gh-archive/gh-archive-2015-01.json.gz
- text-only JSON dataset https://quickwit-datasets-public.s3.amazonaws.com/gh-archive/gh-archive-2015-01-text-only.json.gz

The search stream endpoint is powerful enough to stream 100 million ids to ClickHouse in less than 2 seconds on a multi TB dataset.
And you should be comfortable playing with search stream on even bigger datasets.
