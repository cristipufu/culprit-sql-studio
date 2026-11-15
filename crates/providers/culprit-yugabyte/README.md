# culprit-yugabyte

## Purpose

Read YugabyteDB YSQL and DocDB diagnostics for Culprit.

## Responsibilities

- Collect native statement history, catalog facts, activity, waits and distributed storage observations.
- Preserve node identities and distinguish local observations from qualified cluster coverage.
- Parse YSQL plans with their DocDB and distributed execution facts.
- Expose implemented telemetry routes with bounded readers and explicit coverage gaps.

## How it fits

The provider implements the shared `culprit-core::Provider` interface. It reuses PostgreSQL wire transport and
SQL execution from `culprit-postgres::wire`; YugabyteDB modules own storage meaning, node scope and native
feature selection. The engine supplies credentials, and Checks analyzes the returned facts without database I/O.

## Main entry points

- [`YugabyteProvider`](src/lib.rs): provider construction and operation dispatch.
- [`telemetry_recipes`](src/lib.rs): static declarations for implemented telemetry routes.
- [`plan`](src/plan.rs): native YSQL plan parsing.
- [`native`](src/native/mod.rs): source projections and decoders.
- [`sources`](src/sources.rs): native sources and their permissions.

See [YugabyteDB setup](../../../docs/providers/yugabyte.md) and the [architecture](../../../docs/ARCHITECTURE.md).
