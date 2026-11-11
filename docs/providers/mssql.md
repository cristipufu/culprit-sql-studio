# SQL Server

[Engine parity](../parity.md)

Culprit uses the SQL Server provider for SQL Server and Azure SQL Database. Query Store supplies time-windowed workload history; the plan cache and DMVs supply the evidence available when Query Store is not usable.

## Connecting

Choose **SQL Server** and paste an ADO.NET, JDBC or `mssql://` connection string. Azure SQL connections can use Microsoft Entra ID through the existing Azure CLI sign-in or Culprit's device-code flow.

```sh
culprit conn add shop --env staging --url 'Server=tcp:db.example.com,1433;Database=shop;User ID=app'
```

Passwords stay in the OS keychain. Agent permissions and production confirmations apply separately to each saved connection.

## Permissions and setup

Culprit checks the login's permissions and Query Store state before collection. SQL Server 2022 monitoring views require `VIEW SERVER PERFORMANCE STATE`; older SQL Server versions use `VIEW SERVER STATE`. Azure SQL Database uses database-scoped monitoring permissions such as `VIEW DATABASE STATE`. Individual sources can require additional access.

Query Store must be enabled and readable for interval history, recorded plans and Query Store wait statistics. The plan cache provides a fallback with a different period and retention. Missing permissions or disabled sources produce an explanation and the relevant grant or setting, rather than an empty healthy result.

## Workload and plans

| Feature | Coverage |
|---|---|
| Statement statistics | Query Store executions and runtime intervals; plan-cache counters when Query Store is unavailable |
| Timeline and period comparison | Query Store intervals support selected windows, trends and two-period comparisons; plan-cache comparisons use Culprit's own scans |
| Captured plans | Stored Query Store or cached compilation plans, with plan identities and compiled parameter values where recorded |
| Estimated / measured plans | Estimated Showplan does not execute the statement; measured plans use `STATISTICS XML`, with measured writes rolled back |
| Last actual plan | Retained last actual plans on SQL Server 2019+ when `LAST_QUERY_PLAN_STATS` is available |
| Plan import | SSMS `.sqlplan` and Showplan XML |
| Runtime spread | Recorded minimum, average, maximum and standard deviation per plan; compiled parameter values are compilation evidence, not a trace of every execution |
| Plan and query regressions | Plan history and timed runtime evidence; plans with different retention periods are not treated as interchangeable |
| Tuning recommendations | Query Store plan-forcing recommendations, forced-plan state and failures, where readable |
| Statement families | Query hashes, parameter declarations and the captured statement texts |

Planning, compile history, execution time, CPU, waits, rows, reads and log bytes retain their source's own meaning and dates. Compilation entry totals are not silently assigned to a selected runtime window.

## Live activity and waits

Live shows current sessions, running statements, idle transactions and blocking chains. Query Store attributes recorded wait categories to statements; live wait samples compare server wait counters over the sample interval.

Live identifies connections through native session IDs, login generations, application names and logins. It excludes the observing session itself; another Culprit connection can still appear as a blocker or long-running request. Local connection aliases are not server-side client identities.

Live and background activity capture use the same native reader and counter interpretation. Live requests visible activity and can show an idle transaction's previous batch; background capture keeps qualifying requests and blocker context, with redacted SQL. Request counters remain distinct from session lifetime counters. Missing database-wide permissions fail explicitly, and an unavailable SQL detail does not discard readable session metadata.

When query profiling is available, live progress shows measured operator activity. SQL Server offers **terminate** (`KILL`), not a separate cancel operation; Culprit exposes session actions outside production only.

Deadlock history comes from the readable Extended Events history, such as `system_health`. A missing or truncated event source does not establish that no deadlocks happened.

## Resource history

On Azure SQL Database, scans and background capture share the same native resource reader. Scans reuse their retained samples for the resource timeline and pressure summary. Selected export windows and workload admission request their own bounded projections, with the same units, scope, null handling and timestamps. Instance CPU and memory remain shared context. Background resource capture currently supports primary replicas; native reads preserve the connected replica's role.

## Schema and indexes

Catalog and DMV sources provide tables, columns, constraints, statistics, index definitions and usage. Missing-index requests come from the optimizer's own DMVs and are checked against the existing index set.

Background catalog capture shares the scan readers. Exports preserve native object IDs, local metadata timestamps, exact integer counters and versioned, redacted expressions. Missing statistics properties and unsupported index usage remain explicit. Full catalog coverage requires database-wide metadata visibility before and after the read.

`--collection objects` records versioned table, view, procedure, function and trigger metadata through the shared change reader. Native object IDs, creation generations, parent references and local modification dates remain facts. These observations do not establish an exact UTC change time or record dropped objects.

`--collection triggers` records user DML triggers on tables and views, including disabled and CLR triggers, native creation/parent identities, modification dates, timing and replication flags. It shares its bounded catalog query and decoder with the app's active-table trigger list. Missing event metadata remains unknown and makes scope partial; complete catalog visibility is checked before and after export. Trigger bodies are neither read nor executed. Database/server DDL triggers are outside this dataset.

`--collection partitions` records table/index placement, scheme/function generations, native boundary direction, fanout and parameter types. NULL boundaries differ from functions with no boundaries. Converted display values are bounded and redacted with the existing partition-date policy; a separate native binary fingerprint detects changes that display rounding or redaction can hide. Missing fingerprints make scope partial. The app uses the same reader for heap/clustered storage; export also includes nonclustered layouts. Catalog visibility is checked before and after the read. SQL Server's final range is unbounded, so a finite last boundary does not produce an inserts-will-run-out warning. [Partition functions](https://learn.microsoft.com/en-us/sql/t-sql/statements/create-partition-function-transact-sql), [native boundary metadata](https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-partition-range-values-transact-sql).

`--collection cached-definitions` captures redacted cache SQL and estimated plans for the native statement offsets. `--collection retained-plans` independently captures last-known batch XML where the native facility already exists; it never enables capture or executes an application statement. Both reuse the app's native identity, statement slicing, payload and retained-plan components. Original-text fingerprints distinguish changes hidden by redaction; missing, encrypted, oversized or unredactable bodies have explicit states. Effective per-body limits also reserve space for JSON encoding and record metadata. Retained XML may be simplified and does not establish a measured runtime for every statement. Native statement counts and local start times retain their own meanings; database-scoped capture settings do not imply the state of server trace flags. Cache eviction and concurrent changes can leave references unresolved, so these are bounded observations rather than a complete execution history.

Compilation capture shares the scan's Query Store decoder. `--collection query-compilation` exports each retained query's exact compilation count, average compile microseconds and first/last compile timestamps. These are lifetime observations, including Culprit's retained queries, rather than costs attributed to a runtime interval. Native reset continuity remains unknown.

`--collection cached-runtime` captures completed statement-cache work without Query Store: exact execution counts and total/last/minimum/maximum CPU, elapsed, CLR, read/write and row counters. The reader shares native statement context and database ownership with the scan and memory-grant capture. Local timestamps retain their missing offset; reset continuity remains unknown. Zero native plan handles leave unqualified counters unavailable. Cache eviction can remove history between observations. [Native counter meanings and limitations](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-views/sys-dm-exec-query-stats-transact-sql).

The advisor combines those requests with workload and plan evidence, unindexed foreign keys, and duplicate, overlapping or unused indexes. Disabling an eligible index keeps its definition and stops its maintenance; `ALTER INDEX … REBUILD` restores it. Clustered, unique and constraint-backed indexes are not treated as disposable unused indexes.

Schema drift and migration linting use the saved catalog, actual table sizes and captured workload. The proposed SQL remains a suggestion for review.

## Limits and evidence

Statistics sessions use a two-second lock-wait timeout, low deadlock priority and read-uncommitted isolation. Schema and catalog locks are still possible; these settings do not cap CPU or logical reads. A statistics operation is never automatically replayed after a connection error.

Background capture owns each native request until completion or bounded cancellation handling. If shutdown cannot be confirmed, further collection in that workspace is blocked, including after restart. Artifact writes run outside the async runtime so a slow disk does not postpone source cancellation. This ownership does not coordinate separate workspaces or hosts.

On Azure SQL Database, admission for the heavier background sources checks database CPU, instance CPU, workers, data I/O and log-write pressure. Historical averages can miss a new spike. Row limits, `MAXDOP 1` and client deadlines do not establish a server CPU or I/O quota; retained-history query cost still requires qualification at the deployment's scale.

Query Store and the plan cache are aggregate histories, not application request traces. Cache eviction, resets, retention, permissions and the selected interval limit what can be compared. A compilation plan does not measure the work of the execution being investigated.

Host CPU or filesystem capacity is shown only when the relevant source supplies it. Backup recoverability, database corruption and application transaction boundaries cannot be certified from a successful statistics scan. Cloud limits retain their measured scope and period.

Rates require sufficient history, and an unused-index verdict requires at least a week of dated counters. See the shared [evidence rules](coverage.md).

### Operational evidence

Current database log occupancy and reuse reasons, writable filegroup allocation and configured growth, shared tempdb allocation by user objects, internal objects and version store, and exact identity/sequence numeric bounds are collected where readable.

Filegroup capacity is aggregated across files. Shared tempdb is context, not attributed to the selected database. Configured growth does not prove filesystem headroom. Reserved sequence values do not forecast remaining requests. Unreadable DMVs remain unavailable.

## Verification

The saved examples cover SQL Server 2022 and Azure SQL Database. The reproducible [SQL Server fixture](../../dev/mssql/README.md) plants workload, index, blocking and deadlock problems; saved snapshots exercise offline replay, plan analysis, views and agent output.

The implementation declares its sources in [`culprit-mssql/src/sources.rs`](../../crates/culprit-mssql/src/sources.rs). Version-, edition- and permission-dependent measurements remain unavailable when the server does not supply them.

Source references: [SQL Server file-space DMV](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-db-file-space-usage-transact-sql).
