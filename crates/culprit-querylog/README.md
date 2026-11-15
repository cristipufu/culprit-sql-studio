# culprit-querylog

## Purpose

Read SQL timings from logs an application's framework already writes.

## Responsibilities

- Recognize Rails, Django, Entity Framework Core, Ecto and JSONL query logs.
- Extract statements, durations and available timestamps.
- Group statements by shape into a workload.
- Bound retained data and report partial input.

## How it fits

This optional input supplies workload history for databases such as SQLite that do not keep statement statistics. It is separate from native database telemetry collection.

## Main entry points

- [Public API](src/lib.rs): format detection and workload aggregation.
- [Formats](src/formats/mod.rs): framework-specific parsers.
- [SQLite guide](../../docs/providers/sqlite.md): how query logs fit into a scan.
