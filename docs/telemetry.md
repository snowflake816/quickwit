---
title: Telemetry
sidebar_position: 12
---

Quickwit, Inc. collects anonymous data regarding general usage to help us drive our development. Privacy and transparency are at the heart of Quickwit values and we only collect the minimal useful data and don't use any third party tool for the collection.

## Disabling data collection

Data collection are opt-out. To disable them, just set the environment variable `QW_DISABLE_TELEMETRY` to whatever value.

```bash
export QW_DISABLE_TELEMETRY=1
```

Look at `--help` command output to check whether telemetry is enabled or not:
```bash
quickwit --help
Quickwit 0.7
Sub-second search & analytics engine on cloud storage.
  Find more information at https://quickwit.io/docs

Telemetry enabled
```

The line `Telemetry enabled` disappears when you disable it.

## Which data are collected?

We collect the minimum amount of information to respect privacy. Here are the data collected:
- type of events among create, index, delete and serve events
- client information:
  - session uuid: uuid generated on the fly
  - quickwit version
  - os (linux, macos, freebsd, android...)
  - architecture of the CPU
  - md5 hash of host and username
  - a boolean to know if `KUBERNETES_SERVICE_HOST` is set.

All data are sent to `telemetry.quickwit.io`.

## No third party

We did not want to add any untrusted third party tool in the collection so we decided to implement and host our own metric collection server.
