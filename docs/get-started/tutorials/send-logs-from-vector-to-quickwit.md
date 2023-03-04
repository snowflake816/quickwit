---
title: Send logs from Vector
description: A simple tutorial to send logs from Vector to Quickwit in a few minutes.
icon_url: /img/tutorials/vector-logo.png
tags: [logs, ingestion]
sidebar_position: 3
---

import Tabs from '@theme/Tabs';
import TabItem from '@theme/TabItem';

[Vector](https://vector.dev/) is an amazing piece of software (in Rust obviously) and brings a new fresh wind in the observability space,
it is well-known for collecting logs from every parts of your infrastructure, transform and aggregate them, and finally forward them to a sink.

In this guide, we will show you how to connect it to Quickwit.

## Start Quickwit server

<Tabs>

<TabItem value="cli" label="CLI">

```bash
# Create Quickwit data dir.
mkdir qwdata
./quickwit run
```

</TabItem>

<TabItem value="docker" label="Docker">

```bash
# Create Quickwit data dir.
mkdir qwdata
docker run --rm -v $(pwd)/qwdata:/quickwit/qwdata -p 127.0.0.1:7280:7280 quickwit/quickwit run
```

</TabItem>

</Tabs>

## Create an index for logs

Let's embrace the OpenTelemetry standard and create an index compatible with its [log data model](https://github.com/open-telemetry/opentelemetry-specification/blob/main/specification/logs/data-model.md).

```yaml title="index-config.yaml"
#
# Index config file for receiving logs in OpenTelemetry format.
# Link: https://github.com/open-telemetry/opentelemetry-specification/blob/main/specification/logs/data-model.md
#

version: 0.4

index_id: vector-otel-logs

doc_mapping:
  field_mappings:
    - name: timestamp
      type: datetime
      input_formats:
        - unix_timestamp
      output_format: unix_timestamp_secs
      fast: true
    - name: severity
      type: text
      tokenizer: raw
      fast: true
    - name: body
      type: text
      tokenizer: default
      record: position
    - name: attributes
      type: json
    - name: resource
      type: json
  timestamp_field: timestamp

search_settings:
  default_search_fields: [severity, body]

indexing_settings:
  commit_timeout_secs: 10
```

First create the YAML file:

```bash
curl -o vector-otel-logs.yaml https://raw.githubusercontent.com/quickwit-oss/quickwit/main/config/tutorials/vector-otel-logs/index-config.yaml
```

And then create the index with `cURL` or the `CLI`:

<Tabs>

<TabItem value="curl" label="cURL">

```bash
curl -XPOST http://localhost:7280/api/v1/indexes -H "content-type: application/yaml" --data-binary @vector-otel-logs.yaml
```

</TabItem>

<TabItem value="cli" label="CLI">

```bash
./quickwit index create --index-config vector-otel-logs.yaml
```

</TabItem>

</Tabs>


## Setup Vector

Our sink here will be Quickwit ingest API `http://127.0.0.1:7280/api/v1/otel-logs/ingest`.
To keep it simple in this tutorial, we will use a log source called `demo_logs` that generates logs in a given format. Let's choose the common `syslog` format
(Vector does not generate logs in the OpenTelemetry format directly!) and use the transform feature to map the `syslog` format into the OpenTelemetry format.


```toml title=vector.toml
[sources.generate_syslog]
type = "demo_logs"
format = "syslog"
count = 100000
interval = 0.001

[transforms.remap_syslog]
inputs = [ "generate_syslog"]
type = "remap"
source = '''
  structured = parse_syslog!(.message)
  .timestamp, err = to_unix_timestamp(structured.timestamp, unit: "milliseconds")
  .body = .message
  del(.message)
  .resource.source_type = .source_type
  .resource.host.hostname = structured.hostname
  .resource.service.name = structured.appname
  .attributes.syslog.procid = structured.procid
  .attributes.syslog.facility = structured.facility
  .attributes.syslog.version = structured.version
  del(.source_type)
  .severity = if includes(["emerg", "err", "crit", "alert"], structured.severity) {
    "ERROR"
  } else if structured.severity == "warning" {
    "WARN"
  } else if structured.severity == "debug" {
    "DEBUG"
  } else if includes(["info", "notice"], structured.severity) {
    "INFO"
  } else {
   structured.severity
  }
  .name = structured.msgid
'''

# useful to see the logs in the terminal
#[sinks.emit_syslog]
#inputs = ["remap_syslog"]
#type = "console"
#encoding.codec = "json"

[sinks.quickwit_logs]
type = "http"
method = "post"
inputs = ["remap_syslog"]
encoding.codec = "json"
framing.method = "newline_delimited"
uri = "http://host.docker.internal:7280/api/v1/vector-otel-logs/ingest"
```

Now let's start Vector to start send logs to Quickwit.

```bash
docker run -v $(pwd)/vector.toml:/etc/vector/vector.toml:ro -p 8383:8383 --add-host=host.docker.internal:host-gateway timberio/vector:0.25.0-distroless-libc
```

## Search logs

Quickwit is now ingesting logs coming from Vector and you can search them either with `curl` or by using the UI:
- `curl -XGET http://127.0.0.1:7280/api/v1/vector-otel-logs/search\?query\=severity:ERROR`
- Open your browser at `http://127.0.0.1:7280/ui/search?query=severity:ERROR&index_id=vector-otel-logs&max_hits=10` and play with it!


## Compute aggregation on severity

For aggregations, we can't use yet Quickwit UI but we can use cURL.

Let's craft a nice aggregation query to count how many `INFO`, `DEBUG`, `WARN`, and `ERROR` per minute (all datetime are stored in microseconds thus the interval of 60_000_000 microseconds) we have:

```json title=aggregation-query.json
{
  "query": "*",
  "max_hits": 0,
  "aggs": {
    "count_per_minute": {
      "histogram": {
          "field": "timestamp",
          "interval": 60000000
      },
      "aggs": {
        "severity_count": {
          "terms": {
            "field": "severity"
          }
        }
      }
    }
  }
}
```

```bash
curl -XPOST -H "Content-Type: application/json" http://127.0.0.1:7280/api/v1/vector-otel-logs/search --data @aggregation-query.json
```

## Further improvements

Coming soon: deploy Vector + Quickwit on your infrastructure, use Grafana to query Quickwit, and more!
