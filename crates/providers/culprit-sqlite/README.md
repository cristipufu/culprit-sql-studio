# culprit-sqlite

## Purpose

Read a SQLite database file's schema, settings and plans for Culprit.

## Responsibilities

- Open existing files read-only and collect bounded native metadata and size observations.
- Parse `EXPLAIN QUERY PLAN` output without presenting estimates as measured execution.
- Keep file identity checked through telemetry capture and connection close.
- Interrupt owned blocking work on cancellation; report unsupported sessions and statement counters explicitly.

## How it fits

`culprit-engine` supplies the file path. This provider returns `culprit-core` models for the pure Checks pipeline.
It uses `culprit-read` for common capture and paging contracts and `rusqlite` for owned file access. It does not
discover credentials or connect to a database server.

## Main entry points

- [`SqliteProvider`](src/lib.rs): implementation of the core `Provider` interface.
- [`telemetry_recipes`](src/lib.rs): static declarations without opening a file.
- [`plan`](src/plan.rs): native plan parsing and `plan_from_rows`.
- [`read`](src/read.rs): bounded blocking work, interruption and close ownership.
- [`sources`](src/sources.rs): supported native sources.

See [SQLite setup and limits](../../../docs/providers/sqlite.md).
