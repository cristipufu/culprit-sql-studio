# PostgreSQL

[Engine parity](../parity.md)

Culprit uses PostgreSQL catalog and statistics views for PostgreSQL, including supported managed services such as Amazon RDS. Statement history comes from `pg_stat_statements` and Culprit's own saved scans.

## Connecting

Choose **PostgreSQL** and paste a PostgreSQL URL, libpq parameters, JDBC string or host/database address.

```sh
culprit conn add shop --env staging --url 'postgres://app@db.example.com:5432/shop'
```

The connection's TLS and password settings use the shared connection store. TimescaleDB uses this provider and is detected automatically; its additional behavior is documented on the [TimescaleDB page](timescale.md). YugabyteDB has a [separate native provider](yugabyte.md).

## Permissions and setup

`pg_read_all_stats` or `pg_monitor` provides broader visibility into recorded statements and monitoring views. A restricted login can see less; hidden or unreadable sources do not become zero counts.

`pg_stat_statements` must be loaded and its extension view installed in the database for statement counters. If the server already loads the module, installing the extension exposes existing statistics without another restart. `track_io_timing` and `pg_stat_statements.track_planning` control additional measurements. Hypothetical index testing requires `hypopg`.

Planning can invoke functions. Culprit restricts elevated planning to safely owned statements and refuses planning while other roles' reachable functions could borrow those privileges. Plans are obtained in read-only transactions that are rolled back.

## Workload and plans

| Feature | Coverage |
|---|---|
| Statement statistics | Cumulative `pg_stat_statements` calls, duration, rows and available read/write, planning and JIT measurements |
| Timeline and period comparison | Differences between Culprit's saved scans; the server does not retain Query Store-style runtime intervals |
| Captured plans | Safe generic plans; PostgreSQL 12–15 uses prepared statements and newer versions can use `EXPLAIN (GENERIC_PLAN)` |
| Estimated / measured plans | `EXPLAIN` for estimates; `EXPLAIN (ANALYZE, BUFFERS)` for an explicitly requested measured run |
| Last actual plan | Not retained by these statement counters; an `auto_explain` capture must come from the server log |
| Plan import | Text and JSON `EXPLAIN`, including psql output |
| Virtual indexes | `hypopg` evaluates candidate indexes in session memory without building persistent indexes |
| Runtime spread | Minimum, average, maximum and standard deviation per statement; aggregate spread does not identify the values used in slow executions |
| Statement families | Normalized texts and differences such as IN-list or VALUES lengths |

A changed query text receives its own identity. Statistics resets, eviction and scope changes constrain comparisons; the date of a Culprit scan is not the beginning of a cumulative counter's history.

Scans and statement-counter export share one native temporary-work projection: exact read/write block counts,
the source's block size, and separately available read/write timings. Adding these scan fields uses the existing
statement read. Resource totals can include planning and execution work; parallel I/O time is not an exclusive
elapsed-time component, and current `track_io_timing` cannot qualify historical zeros. Recorded temporary work is
query context, without a guessed disk speed, spilling operator or memory-limit recommendation. Actual plan spill
evidence remains separate. Archived `statement-work` analysis reads the same typed observations without SQL access.

## Live activity and waits

`pg_stat_activity` provides sessions and current activity; `pg_blocking_pids` supplies blocking relationships. Live wait sampling observes busy sessions once a second. `pg_stat_progress_*` views add progress for supported operations, while an estimated plan remains separate from measured live operator activity.

These sources do not retain a history of every wait. Recorded statement I/O timing covers timed data-file work; WAL-writer and commit work is not assumed to be part of the statement's execution time.

The database keeps a cumulative deadlock count. Import the relevant PostgreSQL log lines to reconstruct the participants and pattern. Cancel and terminate actions are available outside production, subject to the login's permissions.

## Supplied structured diagnostics

Analyze an explicitly supplied local PostgreSQL log without a saved connection, workspace or database credentials:

```sh
culprit logs ./postgres.jsonl --input-format json --json
culprit logs ./postgres.csv --input-format csv
```

`json` selects native PostgreSQL JSON logs; `csv` requires the documented 26-column PostgreSQL 14+ layout. Use
`csv13` explicitly for the older 24-column native layout. A truncated modern row is never guessed to be an older
layout. The same provider parser and pure explanations serve supplied bytes, local files and authenticated native
records; intake never calls `pg_read_file`, changes logging, reads an application's tables or executes its SQL.

SQLSTATE supplies the condition: allocation failure, deadlock, resource/connection/configuration limits, unavailable
locks, cancellation and supported termination classes. `57014` establishes cancellation without deciding whether
the client, a timeout or another cause initiated it. `25P04` identifies the separate native transaction-timeout
condition. Unknown codes remain available as unclassified diagnostics rather than acquiring a cause from message text.

Raw messages, detail/context strings and client labels are omitted from the result; retained SQL has its literals
redacted. A qualified deadlock graph contains only reported participants and lock edges. Unsupported or malformed
details withhold the graph while retaining an otherwise valid native `40P01` condition and affected statement.

Native event time keeps its original text and is qualified as UTC only when an explicit supported offset establishes
it. Local intake dates never replace an unknown native clock. The database name is affected context, not proof of a
resource owner. Session IDs and second-resolution starts are native quasi-identities; CSV and JSON line counters are
independent. Neither a recycled PID nor matching session text creates a current-session/query join.

Input bytes, record/field sizes, record counts and JSON output each have a cap. Defaults read at most 1 MiB, decode
at most 1,024 records, and bound records/fields to 64/32 KiB. Valid conditions survive malformed neighboring records
with explicit partial qualification and exit code 2. The artifact digest identifies the supplied prefix, not a native
incident or host, and equal records remain distinct captured positions. Local analysis does not publish an
unverified host/database association into a warehouse or claim complete log history. Managed feed collection still
needs independently qualified acquisition, identity and retention handling.

Source format references: [PostgreSQL structured logging](https://www.postgresql.org/docs/18/runtime-config-logging.html#RUNTIME-CONFIG-LOGGING-JSONLOG)
and [SQLSTATE conditions](https://www.postgresql.org/docs/18/errcodes-appendix.html).

## Schema and indexes

The catalog supplies tables, columns, constraints, statistics and index definitions. `pg_stat_user_indexes` provides usage; the collector records counter history where it can establish the reset period. PostgreSQL does not emit SQL Server-style missing-index requests: advice comes from captured plans, foreign keys, scans and the current index set.

Ordinary tables can use concurrent index builds or drops where PostgreSQL supports them. Invalid indexes are distinguished from indexes that are disabled or no longer maintained. Table-level statistics, autovacuum, dead tuples, HOT updates, transaction IDs, MultiXacts, sequence allocation and replication evidence retain their own scope and timestamps.

For managed hypertables and chunks, use the separate [TimescaleDB coverage](timescale.md); an empty parent heap is not hypertable storage.

## Limits and evidence

PostgreSQL heap and buffer measurements describe PostgreSQL storage. They are not reused for engines that share its SQL grammar but use different storage.

Statement counters are aggregates, not traces. A plan captured today uses the collector's current role and settings and need not match the application's earlier custom plan. Statement errors, deadlock graphs, historical parameters and `auto_explain` output require log or application evidence.

Per-day rates require enough dated history. A restart alone does not prove that PostgreSQL counters were reset, and absence claims require a week of counters. Shared [evidence rules](coverage.md) govern those conclusions.

### Operational evidence

Operational sources cover independent database/table MultiXact ages, shared archive failures, current-database subscription errors on PostgreSQL 15+, standby recovery cancellations, sequence allocation bounds and cleanup retained by a replication slot's `catalog_xmin`.

Counters retain their own reset clocks. New event findings require a comparable interval inside the requested period. Catalog-only retention does not establish retained application rows. Generic plans do not identify the plan used by historical requests, or prove that every listed partition ran.

### Background telemetry

The CLI can archive 16 snapshot datasets and persistent activity through the shared collector service. See the
[PostgreSQL collection table](../collector.md#postgresql-source-coverage) for selections, native sources and starting
cadences. Scans and collection share the native readers; the collector uses bounded read-only sessions, catalog-only
search paths, statement/lock timeouts, disabled parallel query and bounded working memory. A canceled connection is
not reused, and an unconfirmed shutdown blocks subsequent native work.

Counter polling omits query text. Definitions are collected separately and redacted for export. Catalog collection
retains native attributes and expression fingerprints without deparsing user expressions or reading table contents.
Size estimates remain labeled as estimates. Current blocking is an observation at poll time; short events between
polls and deadlock graphs require other evidence. Background capture does not run EXPLAIN or measured queries.

## Verification

Coverage has been exercised on PostgreSQL 15–18, including Amazon RDS; saved fixtures include PostgreSQL 18.6. The [PostgreSQL fixture](../../dev/postgres/README.md) reproduces workload and schema problems. Parser fixtures and provider/embedding tests cover captured plans, missing measurements and offline replay.

The provider's declared sources are in [`culprit-postgres/src/sources.rs`](../../crates/providers/culprit-postgres/src/sources.rs). Optional timing and extension features are tested independently of the base catalog.

Source references: [PostgreSQL vacuum thresholds and MultiXacts](https://www.postgresql.org/docs/18/routine-vacuuming.html), [sequence allocation](https://www.postgresql.org/docs/18/view-pg-sequences.html).
