# culprit-mysql

## Purpose

Read MySQL and MariaDB diagnostics and native plans for Culprit.

## Responsibilities

- Collect catalogs, Performance Schema observations, available native statement history and live activity.
- Handle MySQL and MariaDB differences in plans, instrumentation, locks and timeouts.
- Reuse native layouts and decoders across scans and optional telemetry captures.
- Account for native query phases and retain cancellation ownership until responses settle.

## How it fits

`culprit-engine` supplies resolved connection inputs. This provider produces `culprit-core` facts for the pure
`culprit-checks` pipeline. Native feature and permission gaps remain explicit; a declared telemetry route does
not establish its availability or production cost on a particular server.

## Main entry points

- [`MysqlProvider`](src/lib.rs): implementation of the core `Provider` interface for both products.
- [`telemetry_recipes`](src/lib.rs): static declarations from the native dispatch catalog.
- [`plan_json`](src/plan_json.rs) and [`plan_maria`](src/plan_maria.rs): native plan parsing.
- [`read`](src/read.rs): bounded native reads, accounting and response retirement.
- [`sources`](src/sources.rs): native sources and their permissions.

See [MySQL setup](../../../docs/providers/mysql.md) and [MariaDB setup](../../../docs/providers/mariadb.md).
