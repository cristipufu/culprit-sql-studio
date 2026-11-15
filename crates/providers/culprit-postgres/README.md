# culprit-postgres

## Purpose

Read PostgreSQL diagnostics and native plans for Culprit.

## Responsibilities

- Collect database catalogs, statement statistics, activity, waits and resource observations.
- Parse PostgreSQL JSON plans and retain the source's measured facts and limitations.
- Share native projections between scans and optional telemetry captures.
- Bound native work and retain ownership through cancellation and connection close.

## How it fits

`culprit-engine` supplies connection settings and resolved credentials. This provider returns `culprit-core`
models; `culprit-checks` analyzes them without a connection. YugabyteDB reuses the public wire interface while
owning its own storage and source semantics.

## Main entry points

- [`PostgresProvider`](src/lib.rs): implementation of the core `Provider` interface.
- [`telemetry_recipes`](src/lib.rs): static declarations for implemented telemetry routes.
- [`plan_json`](src/plan_json.rs): native JSON plan parsing.
- [`wire`](src/wire.rs): shared PostgreSQL transport, bounded readers and activity layouts.
- [`sources`](src/sources.rs): native sources and their permissions.

See [PostgreSQL setup](../../../docs/providers/postgres.md) and the [architecture](../../../docs/ARCHITECTURE.md).
