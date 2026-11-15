# culprit-mssql

## Purpose

Read SQL Server and Azure SQL evidence for Culprit. The provider turns native facts into shared models; pure checks explain those facts elsewhere.

## Responsibilities

- Read database metadata, Query Store, supported cache entries, current activity and resource statistics.
- Keep query incarnations, execution populations, observation bounds and missing-source reasons explicit.
- Parse native Showplan XML and retain the evidence its operators expose.
- Supply bounded telemetry readers with query, row, byte, timeout and cancellation ownership.

## How it fits

- `culprit-engine` selects the provider and supplies connection settings and resolved credentials.
- `culprit-core` defines the models and provider contracts; `culprit-read` supplies shared reader lifecycle support.
- `culprit-checks` analyzes snapshots without opening database connections. The collector uses the provider's declared telemetry recipes.
- A bounded query is not proof of acceptable production cost. Recipe cost qualification and source availability remain separate requirements.

## Main entry points

- [`src/lib.rs`](src/lib.rs): `MssqlProvider`, its provider implementation and `telemetry_recipes()`.
- [`src/collect.rs`](src/collect.rs): snapshot acquisition and shared population collection.
- [`src/sources.rs`](src/sources.rs): source declarations and coverage requirements.
- [`src/telemetry/recipes.rs`](src/telemetry/recipes.rs): telemetry registrations and native reader selection.
- [`src/read.rs`](src/read.rs): bounded native reads and shutdown ownership.
- [`src/plan_xml`](src/plan_xml): SQL Server plan parsing.

See [provider documentation](../../../docs/providers/mssql.md), [design](../../../docs/DESIGN.md) and [architecture](../../../docs/ARCHITECTURE.md) for supported behavior and evidence rules.
