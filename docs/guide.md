# Culprit guide

The reference for the Culprit desktop app and the `culprit` CLI: what it checks, every command, how coding agents use it, what it guarantees on production, and how to build it. For the overview, see the [README](../README.md). For database-specific setup, sources and verified features, see [Engine parity](parity.md).

Culprit reads the statistics, catalog or file evidence described by each provider, ranks findings by measured impact,
and gives each finding the evidence, fix and a way to verify it. Scans do not apply fixes or run the application's
queries. The editor runs what you ask with production confirmations. The desktop app, CLI and coding-agent tools
share one Rust core.

## Contents

- [What it finds](#what-it-finds)
- [Quick start](#quick-start)
- [Coding agents](#coding-agents)
- [Speed](#speed)
- [Extending](#extending)
- [Safety](#safety)
- [Building](#building)
- [Layout](#layout)

## What it finds

These are examples; `culprit checks` lists the current checks. [Engine parity](parity.md) explains provider applicability.

| Check | What it means |
|---|---|
| `missing-index` | Index suggestions from the optimizer, merged, de-duplicated against existing indexes and linked to the queries that asked |
| `filtered-scan` | Heavy queries that read a whole table to keep a few rows, with the index that would avoid it |
| `plan-warnings` | Type mismatches that block index seeks, spills to disk, missing join predicates, missing statistics |
| `chatty-query` | Repeated small queries that may indicate polling or an N+1 pattern; request-level evidence is needed to confirm the cause |
| `heavy-query` | Single statements that dominate database time, and what their plan spends it on |
| `plan-regression` | Query Store saw a query get much slower after its plan changed |
| `regression` | A query got much slower at a point in the window: when it started, by how much, and whether a plan change coincides |
| `unstable-query` | The same statement has widely varying durations; source evidence guides the possible causes |
| `temp-spill` | Sorts and hashes exceeding `work_mem` |
| `unindexed-fk` | Foreign keys with no index on the referencing columns |
| `unused-index` | Indexes maintained on every write but never read |
| `duplicate-index` | Identical or left-prefix-redundant indexes |
| `stale-stats` | Optimizer statistics far behind the data |
| `heap-table` | Large tables without a clustered index |
| `dead-rows` | Dead tuples and lagging autovacuum |
| `xid-age` | Transaction ID wraparound risk |
| `config` | Source-backed settings advice, such as unavailable statement statistics or disabled automatic statistics |

Trends use the source's recorded intervals or differences between Culprit's saved scans. The [provider guides](parity.md)
explain which periods can be compared. `culprit drift <a> <b>` (Schema → Compare with…) lists the schema differences
between two connections, with the SQL to close the gap.

`culprit checks` lists them with the capabilities each needs. Checks that can't run with your login show up as locked, with the exact grant or setting that unlocks them.

## Quick start

```sh
# Save a connection. Secrets go to the OS keychain; Azure SQL can use your `az login` instead of a password.
culprit conn add shop-staging --env staging \
  --url 'Server=tcp:shop-staging.database.windows.net,1433;Database=shop;Authentication=Active Directory Default'

culprit conn discover --match shop --match stg --save   # or find Azure SQL, PostgreSQL and MySQL databases your az login can see
culprit conn import --dry-run                  # or bring in connections from Azure Data Studio, VS Code, DBeaver, DataGrip
culprit conn add app --url db/production.sqlite3   # a SQLite 3 file, opened read-only
culprit conn set app --query-log log/production.log   # its framework's log, when it isn't found beside the file (docs/providers/sqlite.md)

culprit scan shop-staging                      # findings, ranked
culprit scan shop-staging --window 6h          # the same over the last 6 hours (90m, 2d, 1d12h; up to 90 days)
culprit dashboard shop-staging                 # the database at a glance: what needs a look first
culprit changes shop-staging --recent 2h      # what got slower, and what changed around it
culprit mark shop-staging deploy v1.4.2         # put a deploy on the timeline (from CI, after a migration)
culprit cloud shop-staging                     # RDS / Azure metrics against their limits, via your aws or az sign-in
culprit show shop-staging '<finding id>'       # evidence, fix, verification, and a brief for your coding agent
culprit top shop-staging --by cpu -n 10        # heaviest queries
culprit explain shop-staging --sql 'SELECT …'  # estimated plan, never executes (--with-index tests virtual indexes on Postgres)
culprit lint-sql shop-staging --file migrations/ --format sarif   # lint migration DDL offline against the snapshot (--write rewrites in place)
culprit plan show slow.sqlplan                 # read a plan from another tool: .sqlplan, EXPLAIN JSON or text, MySQL JSON or tree, SQLite EXPLAIN QUERY PLAN
culprit plan diff shop-staging:42:7 shop-staging:42:9   # compare two Query Store plans of query 42 (or two files)
culprit update --check                         # is there a newer release? `culprit update` installs it
```

Or open the desktop app: saved connections are listed in the sidebar, grouped by environment. A connection opens on its **dashboard**: one sentence that says what needs a look first, then fourteen tiles (database time and its trend, findings, top queries, where time goes, sessions right now, health, deadlocks, indexes, plans, storage, connections, replication, failing queries, query statistics), each a plain takeaway, a small picture and a click into the view that explains it. Every engine gets every tile; one it can't fill says why. The ranked findings are one key away (`2`). Pick the engine and paste its connection string, or pick **SQLite** and choose the file (then **Add query log** if its framework's log isn't found beside it), use **Find Azure databases** / **Find AWS databases**, or **Import from other tools** (names, hosts and user names only; passwords are asked for once), and the first scan appears in seconds. Reopening a connection shows its last report instantly while a fresh scan runs. Rename a connection's alias with the pencil on its card or `culprit conn rename`.

Entra ID connections use your `az login`. Without the Azure CLI (or when it's signed out), Culprit signs in with a device code instead: the app shows the code, the CLI prints it when you're at a terminal. Access tokens stay in memory, the refresh token in the OS keychain; `culprit auth logout` forgets it. Scripts and agents never block on a sign-in: they get an error with a hint.

On non-production connections, Live can cancel a session's statement or end the session (`culprit live <conn> --cancel <id>` / `--kill <id>`). Production connections refuse both, in the CLI and the app.

### SSH jump hosts

Enable **SSH jump host** when adding a server connection, or open **SSH** on a saved connection's card. Enter a host such as `reader@bastion` or an alias from your OpenSSH configuration. The same settings work on connections added through Azure or AWS discovery. **Test connection** checks a draft with the saved database credentials before you save it.

```sh
culprit conn add shop --url 'postgres://app@db.internal:5432/shop' --ssh prod-bastion
culprit conn set shop --ssh reader@bastion --ssh-port 2222 --ssh-key ~/.ssh/id_ed25519
culprit conn set shop --no-ssh
```

Keep the real database hostname and port in the connection string. Culprit forwards the network connection while preserving that identity for certificate verification, AWS IAM, Entra ID and cloud metrics. It uses the system OpenSSH client, your SSH configuration and agent; host keys must already be trusted, and encrypted keys must be unlocked in the agent. See [SSH connections](ssh.md) for setup, supported authentication and lifecycle details.

### Plans: paste, compare

The **Plans** view (`7`, also without a connection) draws any plan you paste (⌘V anywhere), drop or open: SQL Server .sqlplan or showplan XML, Postgres `EXPLAIN` as text or `FORMAT JSON` (psql output included), MySQL `EXPLAIN FORMAT=JSON` or `EXPLAIN ANALYZE`. **Compare** puts two plans side by side and says what changed in plain words ("Plan 9 scans orders where Plan 7 seeks ix_orders_customer"), with every changed, added or removed operator marked in both diagrams. On a query with several Query Store plans, **Compare** in the Queries view opens the fastest and the slowest plan; **Open in Plans** keeps any plan to save it (.sqlplan opens again in SSMS) or compare it with another environment's.

### Editor and saved queries

The **Editor** keeps your query tabs across restarts (⌘T new, ⌘⇧W close, ⌘⇧[ / ⌘⇧] switch), completes schemas, tables and columns as you type, read live from the connection (aliases included: after `o.` it offers only that table's columns, with their types), and lints DDL and migration statements inline against the latest snapshot with one-click quick fixes. ⌘↵ runs the selection or the whole tab, ⌘E shows the plan without running it, ⌘S saves the tab as a named query.

**Saved queries** live in the editor's side rail, the command palette (⌘K, "Run saved: …") and the CLI (`culprit saved list | add | show | run | rm`); each remembers the connection it was saved for and can run on any other.

**What runs without asking:** a statement Culprit's classifier proves only reads runs anywhere, in a read-only transaction (rolled back on SQL Server, which has none). Anything else commits; on a **production** connection only after a confirmation that names what it does ("This UPDATE has no WHERE clause, so it changes every row in the table"), and statements that touch every row or drop objects need the connection's name typed. In the CLI, `--yes-execute-on-production` is that confirmation. `culprit catalog <conn>` prints what completion sees.

Findings whose queries stopped running well before the end of the data window (relative to how often they used to run) move to a "Stopped running" group: usually a deploy changed or fixed them.

**One thing per tab:** while a tab runs, plans or waits for a production confirmation, another run there waits its turn (open another tab to run in parallel). Closing a tab stops its batch: nothing after the statement in flight is sent, and that statement finishes on the database.

### Collect native telemetry to files

For a durable background service with private recovery storage and analytical delivery, see the
[collector deployment and recovery guide](collector.md).

`collect once` exports runtime and wait measurements with their referenced query and plan definitions for one retained, completed Query Store interval on Azure SQL Database. Use its exact native boundaries:

```sh
culprit collect once shop-staging \
  --from 2026-11-09T06:00:00Z --until 2026-11-09T07:00:00Z \
  --output ./telemetry-observation --json
```

The output directory must be new and its parent must exist. It contains `runtime.ndjson`, `waits.ndjson`, `queries.ndjson`, `plans.ndjson` and `manifest.json`. The manifest records database identity, interval, replica groups, capture policy, file hashes, each stream's completion and reference coverage. Integer counters and native identifiers are decimal strings to retain their precision. A failed or interrupted stream remains incomplete, with a `.part` file when applicable. Missing query/plan references keep the observation incomplete even when metric files finished. Exit status is 0 for complete reads and resolved references, 2 for an incomplete observation, and 1 for an error. Existing exports are never overwritten or resumed.

Defaults are 500 records and 4 MiB per page, 64 MiB total, 32 page attempts, 128 native reads, 5 seconds per statement and 60 seconds overall. The corresponding `--page-rows`, `--page-bytes`, `--max-bytes`, `--max-pages`, `--max-queries`, `--statement-timeout-ms` and `--timeout-ms` flags set these budgets. Small intervals use one data read per metric stream; oversized ranges split by replica and plan ID. Definition payloads are packed into bounded batches after their lengths are read. Missing or high pressure signals stop collection conservatively. These limits do not guarantee a fixed database CPU or I/O cost.

The command uses statistics sessions, parameterized tagged queries and the connection's existing credentials. It reads no application tables, changes no database settings, and never prompts for a device-code sign-in. Definitions are fetched only for referenced native IDs. Their lengths are read before payloads: `--max-sql-bytes` defaults to 256 KiB and `--max-plan-bytes` to 2 MiB of native UTF-16 bytes. The effective limit can be smaller to fit one complete encoded record within the page budget; increasing a payload cap may also require increasing `--page-bytes`. Oversized, restricted, encrypted, missing or unsafe payloads have explicit availability states; definitions are never truncated. SQL literals/comments and plan parameter values are redacted before files are written. Object names remain visible, so exports still require access controls. Monitoring permissions must already exist; reading native identity in an elastic pool can require server state access. Ctrl+C cancels owned work before returning. Unconfirmed cancellation blocks further collection in that workspace; verify server activity before creating another collector. Admission is local to one workspace, so run one collector for the pilot.

Each line is a versioned envelope with `capture`, `batch_id`, `record_id`, `representation_version` and the typed `record`. `capture` contains an `observation_id` and an optional outbox `revision`. Definition IDs use native compilation identity, context, hashes and the database/source namespace, independently of redacted payload bytes. Metric IDs additionally include the native interval, row, replica and execution outcome; waits include their category. An unresolved native definition leaves `record_id` absent. Replaying the same export preserves its IDs; a new collection has a new observation ID. Native database identity keeps IDs stable across connection alias and endpoint changes. The export retains native statistics rows; resolve complete observation revisions before aggregating them, and never sum repeated captures of the same interval.

Live activity retains native client application, login and host metadata. It excludes the observing session by native session ID while keeping other Culprit sessions when they are active or hold relevant transactions. Query Store and cached aggregate statistics do not retain the originating client's application name; SQL comments identify Culprit's historical statements without treating a marker inside a string or quoted name as ownership. A local connection alias is not sent as a server-side session identity. [Native session metadata](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-exec-sessions-transact-sql), [Query Store identity](https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-query-store-query-transact-sql).

Culprit's own queries remain in the raw history so monitoring overhead can be investigated. Query definitions identify application, Culprit monitoring, other tagged Culprit, engine-internal and unknown statements before SQL redaction removes comments. Monitoring definitions also retain a template ID, which excludes bound arguments. Application recommendations leave Culprit's queries out. Native runtime rows retain duration, CPU, reads, writes, rows, memory, parallelism, CLR, log and tempdb measurements with their units and explicit unknown values.

Query Store can omit monitoring statements under its capture policy, so native history alone does not establish total collector cost. Each capture's `manifest.json` also contains `read_metrics`: bounded per-template client elapsed time, decoded rows, charged decoded-value bytes, failures, timeouts, cancellations and unconfirmed cancellations. These measurements require no extra SQL, persist no bound argument values, and correlate with native history through the template ID. Client elapsed time is not database CPU; charged bytes are not disk or network I/O. Metrics unavailable from a source remain `null`; `active_tasks` and `untracked_tasks` make unfinished or capacity-limited accounting explicit.

“Complete” describes the native reads and reference closure. Unavailable payloads remain visible separately. Source-generation continuity is unverified, and pages are reconcilable observations rather than an atomic snapshot. Definition discriminators reduce native ID reuse collisions but do not prove that Query Store was never cleared. Files are flushed before completion; `directory_synced` states whether directory entries were also flushed on this platform.

`--collection resource-history` selects retained Azure SQL resource samples instead of Query Store. Its half-open window selects native sample timestamps and needs no Query Store interval boundary:

```sh
culprit collect once shop-staging --collection resource-history \
  --from 2026-11-09T14:00:00Z --until 2026-11-09T14:05:00Z \
  --outbox ./telemetry-outbox --json
```

Choose a recent past window within the source's retained history. The capture contains `resources.ndjson` and `manifest.json`, using the same budgets, cancellation ownership, outbox and replay commands. The native view retains approximately one hour of samples at 15-second intervals, shortened by failover. The description records the retained timestamp bounds, expected native row count and nominal period. A window outside those bounds remains incomplete with `retention-gap`, even when the retained subset was read successfully; it cannot advance the outbox's captured-range head. Successful reads do not prove uninterrupted sampling between native timestamps. [Native resource statistics](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-db-resource-stats-azure-sql-database?view=azuresqldb-current).

Each resource row contains typed metric samples with stable keys, units, scope, aggregation, measurement period, limit basis and an optional value. The Azure adapter exports database CPU, data I/O, log writes, memory, maximum workers/sessions, In-Memory OLTP storage and DTU/vCore limits. Instance CPU/memory remain shared server measurements; instance CPU has native one-minute granularity. NULL stays unknown. This reader currently supports Azure SQL primary replicas. It reads bounded statistics views independently of Query Store, including when workload collection is suppressed by pressure; it shares native read deadlines and confirmed cancellation. It reads no application tables and makes no fixed CPU-overhead claim.

The source does not establish a stable shared-instance identity. Shared measurements observed through different databases cannot be summed or assumed to refer to different instances.

The scan and background collector share the same native resource reader. The scan derives its timeline and pressure summary from one history read; NULL metrics stay unknown, and native fractional timestamps are preserved. Workload admission uses a latest-sample request through that reader.

Overlapping resource windows preserve the native sample's record ID. Repeated observations must not be counted as additional measurements.

Collection profiles and stream metadata are independent of database-provider and destination identities. The engine validates and persists the selected streams rather than manufacturing empty files for every known family. File metadata declares whether a stream contains intervals, samples, events, checkpoints or definitions. Native wait counters use cumulative-counter semantics, independently of interval-based Query Store waits. Exports use the current draft format only; older draft outboxes are rejected without migration.

`--collection query-compilation` captures retained Query Store compilation facts into `operations.ndjson`, without a historical window. Counts remain exact decimal strings; average duration is in microseconds. Native first/last compile timestamps and the clock comparison remain separate facts. The profile includes Culprit's own retained queries and uses the same decoder as scan diagnostics. A query ID and its initial compile time identify the retained lifetime; these totals must not be assigned to a runtime interval or summed across repeated captures. Unknown reset continuity remains explicit.

`--collection operational` captures current log space, writable filegroup capacity, tempdb allocations, sequence/identity limits and native file I/O counters, without a historical window:

```sh
culprit collect once shop-staging --collection operational --outbox ./telemetry-outbox --json
```

The capture contains `operations.ndjson` and its manifest. Spaces retain bytes, scope, configured file limits, growth settings and native reuse reasons. Sequence values remain decimal strings. Unknown measurements remain `null`; shared tempdb measurements have no established cross-database identity and must not be summed across databases. The app and background collection use the same bounded native operational reader; names are localized only for application display.

File I/O facts preserve native database/file IDs, logical names and type when visible, exact read/write counts, bytes and stall milliseconds, on-disk bytes and native uptime ticks. An unavailable name or type does not remove the native counter row. Reset history stays unknown; uptime ticks do not establish a reset epoch. The export keeps zero-I/O files and the complete bounded native file set; the app separately ranks its display. Both use the same targeted reader. Azure reads this database's files only and excludes unattributed Hyperscale page-server rows; interactive SQL Server scans also read shared tempdb explicitly. Shared observations have no proven instance identity and must not be added across database captures. [Native file statistics](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-io-virtual-file-stats-transact-sql).

Performance facts retain batches, compilations/recompilations, page-life expectancy, logins and connection resets from one shared native read. Values remain exact decimal strings with native object/name/instance/type, scope, unit, gauge/cumulative meaning and unknown reset evidence. Unknown native counter types preserve their raw value with unknown meaning and unit; they cannot become derived application ratios or rates. Instance rows never replace a global counter. Empty native results mean unavailable counters, not a measured zero. Short samples subtract exact integers before conversion and require both readings of the same counter. Missing recompilation counts cannot be substituted with zero when computing compilation ratios. [Native performance counters](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-os-performance-counters-transact-sql).

Current queue facts count memory grants and worker tasks separately, preserving exact counts, maximum observed waits and the selection threshold (100 ms for worker tasks). Database counts require the native request's database; session defaults are insufficient. Unattributed members of shared native queues remain coverage metadata and cannot be added to a database count or summed across captures. Unknown attribution makes that source partial, with no healthy zero in the app. Native task/request identity prevents MARS requests from multiplying wait rows. App and background reads use the same bounded implementation. [Native waiting tasks](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-views/sys-dm-os-waiting-tasks-transact-sql), [native task/request identity](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-views/sys-dm-os-tasks-transact-sql).

The tempdb version-store gauge preserves this database’s reserved space as exact native kibibytes. The app converts the same fact to bytes; an unavailable source never becomes zero. The reader uses aggregated space metadata, with no walk through individual row versions. ADR’s persistent version store requires separate collection. [Native tempdb version-store space](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-views/sys-dm-tran-version-store-space-usage).

The snapshot description records each native source separately: collected, empty, partial, unavailable or not applicable, its record count and scope completeness. File completion confirms the exported observation; native source availability is reported separately. Restricted metadata cannot establish an empty sequence catalog. Current absence requires a complete source; unavailable or incomplete observations preserve its earlier evidence. Pressure suppresses remaining reads after one failed admission probe. Azure SQL does not expose msdb suspect-page history; that source is explicitly not applicable. The native model carries an offset-free reported timestamp with unknown reset semantics, without assigning a UTC event time or a server-start counter epoch.

Operational capture keeps host logical processors, online SQL user schedulers and visible NUMA nodes as separate server-scoped gauges. These are shared context, rather than a database CPU quota; Azure resource history carries the database's native CPU limit separately. The app uses the available online schedulers for parallelism context. [Native scheduler meanings](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-os-schedulers-transact-sql).

On databases with ADR enabled, operational capture also keeps persistent version-store statistics separately: off-row and online-index kibibytes, aborted transaction counts, transaction watermarks and native cleanup timestamps. Off-row size excludes versions stored within data rows. The app displays it as information rather than grading every database by one size threshold. Known disabled ADR or unsupported server versions are not applicable; unreadable or missing measurements are unavailable, never a zero. Tempdb allocation is labelled explicitly and cannot measure ADR's persistent store. See [the native PVS statistics](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-tran-persistent-version-store-stats).

Operational capture also includes the transaction horizon: the oldest transaction with logged work or an explicit snapshot in this database, and separately any oldest candidate whose row-version activity has unknown database attribution. Session and transaction identities, native local begin times, exact log counts/LSNs and distinct transaction/XSN ages remain facts. An unknown age or database attribution is not a healthy zero. This current maximum does not enumerate every transaction or prove one ended when another becomes the maximum. Other Culprit sessions stay in raw telemetry for overhead investigation. Database version-store size is separate context; use the native log-reuse reason when diagnosing log growth, including on ADR-enabled databases.

`--collection wait-counters` captures all nonzero native wait types into `wait-counters.ndjson`, including idle waits that the app leaves out of its performance summary. The app's categories, pressure totals and short samples use this same native reader. Counts and total/signal/maximum wait milliseconds remain exact decimal strings, with database or server scope. The Azure export uses database scope only.

```sh
culprit collect once shop-staging --collection wait-counters --outbox ./telemetry-outbox --json
```

These views expose no reset generation: a restart is not the only way their counters can reset. The export records an unknown reset marker, never an invented startup epoch or a precomputed rate. Omitted zero rows are not independently observed zero measurements. Maximum wait time is a native maximum, not an additive total. Short interactive samples reject detected counter regressions or disappearing rows; a reset that catches up between readings remains undetectable. [Native wait counters](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-views/sys-dm-os-wait-stats-transact-sql).

Background Azure SQL captures require a confirmed primary connection. Retained/current-state readers recheck it with native database identity during verification; activity checks the replica fact on its observed frame. Read-only replicas require their own validated export profile. Interactive readers retain replica facts independently of export policy. [Replica detection](https://learn.microsoft.com/en-us/azure/azure-sql/database/read-scale-out?view=azuresql#verify-that-a-connection-is-to-a-read-only-replica).

`--collection replication` captures current native geo-replication links into `replication.ndjson`. Link GUIDs, local endpoint roles, partner names, native states and primary-only lag/time measurements remain facts; secondary lag is unknown. The app uses the same bounded native reader. SQL Server and Managed Instance scans also read database-scoped availability replicas, retaining native GUIDs, nullable direction, suspension/health, exact queue sizes and native transfer-rate units. Optional columns are selected from catalog metadata before reading. Failed reads become unavailable coverage, with no schema retry or silent empty result.

```sh
culprit collect once shop-staging --collection replication --outbox ./telemetry-outbox --json
```

Only a complete native read can establish that a replication source is empty. Unknown roles stay in raw telemetry and cannot be assigned an application lag verdict. Native replica state has local relevance and can be out of date; it does not establish cluster-wide freshness. [Availability replicas](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-hadr-database-replica-states-transact-sql), [geo-replication links](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-geo-replication-link-status-azure-sql-database).

`--collection settings` captures current database, scoped configuration and Query Store options into `settings.ndjson`. It has no historical window. Native values, active values, secondary values and default provenance remain separate; a NULL value stays unknown. Server settings are not applicable to this Azure SQL profile. The app uses the same reader and reuses options already read by its probe.

```sh
culprit collect once shop-staging --collection settings --outbox ./telemetry-outbox --json
```

Each native source has its own coverage. Configuration, changing status and usage measurements have distinct types: Query Store storage growth is not a configuration change. A partial or failed read cannot establish that settings stayed unchanged or disappeared.

`--collection plan-cache-summary` records database-filtered cache entry counts and bytes in `operations.ndjson`.
It separates total cache footprint, ad hoc entries with one recorded lookup and prepared entries with one lookup.
These are gauges, not execution counters: parameterized reuse does not increment the native lookup count.
The app's cache advice uses the same reader and excludes prepared entries. This profile scans cache metadata;
use a slow cadence and measure its cost before scheduling it across a fleet. It is excluded from `operational`.

`--collection statement-grants` streams this database's retained statement-cache memory-grant counters into
`operations.ndjson`, including Culprit's own queries for cost investigation. It preserves native handles,
statement byte offsets, compilation generations, exact counters in kibibytes and timestamps without invented
UTC offsets. Query hashes group related statements; they do not identify a cache entry or prove continuity.
The app uses the same native relation and decoder, then summarizes the selected workload separately.

One owned SQL statement supplies bounded pages; a final native completion and identity check establish the
capture's final coverage. Row, byte, query and time limits still apply. Set `--max-snapshot-rows` deliberately
(up to 100,000); the default remains 500. Truncation or a late SQL error cannot establish complete coverage.
Cache eviction or recompilation is not evidence that a query stopped running. Native execution timestamps
record start times, so the app does not use them to order concurrent completions or average unrelated last-grant
values. This metadata scan belongs on a measured slow cadence, outside the fast operational profile.
[Native cache counters](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-exec-query-stats-transact-sql).

`--collection index-operations` streams per-partition index and heap counters into `operations.ndjson`.
It retains native object, index, partition and internal rowset identities; inserts, updates, deletes, page
allocations and merges; forwarding and LOB reads; lock, latch and I/O waits; compression, versioning and
compaction counters where the native columns exist. Counters remain exact decimal strings with their units;
absent columns and native NULL values remain distinct. Hidden names do not discard the native row.
The scan derives heap-forwarding and index-contention summaries from this same relation and decoder.

This is a current metadata-cache observation, outside the fast operational profile. One owned statement supplies
bounded pages, followed by native completion and identity verification. `--max-snapshot-rows` and the other
collection budgets apply; a truncated read cannot establish complete coverage. Counters can reset when metadata
leaves the cache, independently of server startup, so captures do not claim a known reset epoch or a historical
rate. This native source excludes memory-optimized indexes. Measure source cost before fleet scheduling.
[Native index counters](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-db-index-operational-stats-transact-sql).

`--collection tuning` captures automatic tuning options, recommendations, forced-plan failures and Query Store
hints in `tuning.ndjson`. It uses the scan's native readers and reports coverage separately for each source.
Counts remain exact decimal strings, absent measurements remain unknown, and recommendation-time CPU statistics
are separate from current execution costs. Hint payloads are bounded before retrieval; oversized payloads are
explicitly unavailable. Export redacts SQL literals before writing. Query/plan IDs here are native references
within the observation; they do not establish a definition match across captures or Query Store resets.
Configured forcing and hints can still be captured with Query Store OFF. This profile does not enumerate
per-replica plan-forcing locations or establish complete tuning-event history.

### Export retained deadlock events

`--collection deadlock-history` selects a past timestamp window from Azure SQL's explicitly configured,
[database-scoped deadlock session](providers/mssql.md#azure-sql-deadlock-events), using the same native reader
and graph parser as interactive investigation:

```sh
culprit collect once shop-staging --collection deadlock-history \
  --from 2026-11-09T14:00:00Z --until 2026-11-09T14:05:00Z \
  --outbox ./telemetry-outbox --json
```

The capture contains `deadlocks.ndjson` and `manifest.json`. Events retain native session generation and event sequence, precise time,
victims, redacted statements, participants, resource edges and a typed pattern. Login, host and application identities
are omitted. The source is read under pressure and work budgets and rechecked before completion; changing membership
or a result exceeding `--max-snapshot-rows` leaves the capture incomplete. Choose smaller windows for a busy source.

`history_complete: false` and an unknown retained horizon describe a native limitation. Successful emptiness means
that no matching event was visible in that read; it does not establish complete deadlock history. No diagnostic export,
event session or database setting is enabled by the command. Ingestion and durable remote recovery are separate from
this local capture command.

### Monitor persistent activity

The Azure SQL activity monitor records long-running requests and sustained blocking, including an idle transaction that is an observed blocker. It does not archive every healthy session. Initialize a local outbox, then run:

```sh
culprit collect outbox init ./activity-outbox --json
culprit collect activity shop-staging --outbox ./activity-outbox --json
```

The default is one poll per minute, with the next interval starting after the previous attempt finishes. `--poll-seconds` accepts 60 seconds or more; failed attempts back off, up to 15 minutes or the configured interval if longer. There are no overlapping reads or catch-up bursts. `--max-polls 1` performs one bounded pilot attempt. Ctrl+C cancels owned work and stops the monitor; unconfirmed native shutdown stops further polling.

Polling and qualification are independent. Defaults are `--blocked-seconds 30`, `--running-seconds 60` and `--update-seconds 300`. An incident starts when a complete observation meets a threshold. Changes in its conditions, wait owner or query identity can emit an update; unchanged incidents are refreshed every five minutes. Counter comparisons distinguish observed change, no observed counter change and unavailable/reset counters. Unchanged counters alone do not establish that a request is hung. Events describe when Culprit observed a transition, rather than claiming the exact time it happened between polls. Requests that start and finish between polls can be missed.

Each poll uses one bounded metadata query, plus normal connection setup. Only qualifying requests and their available blocker context trigger a second bounded detail query. The collector reads statistics views, never executes the observed statements, and redacts SQL before persistence. `--max-snapshot-rows` defaults to 500; exceeding the bound, missing database-wide visibility, timeout or incomplete results makes coverage unknown. Optional SQL details have explicit unavailable states. Native session/request generations prevent a reused ID from inheriting another request's incident. SQL text requires matching native session/request generations, database, SQL handle and statement offsets; a changed or unidentified statement remains unavailable instead of being attributed to the earlier observation. A missing or zero query hash alone does not prevent a handle-verified statement from being read. Idle blockers require evidence of an open transaction and a qualifying blocked request; their previous batch is not presented as the current statement. Native metadata views are not a transactionally frozen history: changes during the read can leave a blocker explicitly unobserved.

Captures contain `activity.ndjson`, `blocking.ndjson`, `incidents.ndjson`, `activity-state.ndjson` and the manifest. The first two contain selected source observations; incident events and restart checkpoints are derived from those facts. A complete poll commits the events and active checkpoint with the same outbox capture. A failed or partial poll cannot resolve an incident or replace the last complete checkpoint. A native database epoch change starts a separate identity namespace. Empty complete captures distinguish an observed quiet database from unknown coverage. JSON mode prints one status object per attempt with unknown incident counts represented by `null`.

For a standalone observation, use `collect once CONNECTION --collection activity --output NEW_DIRECTORY --json`, without `--from` or `--until`. Standalone captures have no prior incident checkpoint. `collect activity` monitors one connection; `collect fleet` schedules multiple connections and sources. Outbox limits still apply: the default 256 retained attempts allow approximately four hours at the minimum activity interval, and a full outbox stops before another source read. There is no automatic eviction until remote recovery and acknowledgement exist.

### Collect a static fleet

`collect fleet` uses the same authenticated providers, native admission guard and durable outbox as one-off collection.
Each inventory target has a stable `id`, a connection, an explicit database or shared-resource `subject`, one static
shard and a physical admission group. A database and its shared pool/instance can use the same connection with
separate schedules and checkpoints. A shared native resource has one owner across the complete inventory, even
when several member connections can read it. Databases sharing an instance or elastic pool must use the same
admission group; a group cannot be split across shards. Only one process may own a shard's scheduling state and
outbox. The workspace permits one native operation at a time, including interactive reads. Group names do not
establish measurement scope. See [shared-subject discovery and collection](collector.md#database-and-shared-subjects)
for the available native routes.

```json
{
  "targets": [{
    "id": "shop-database",
    "connection": "shop-staging",
    "subject": { "kind": "database" },
    "shard": "alpha",
    "admission_group": "shop-elastic-pool",
    "schedules": [
      {
        "id": "live",
        "every_seconds": 60,
        "jitter_seconds": 15,
        "collection": { "kind": "activity" }
      },
      {
        "id": "queries",
        "every_seconds": 300,
        "jitter_seconds": 30,
        "collection": { "kind": "query-history", "lookback_seconds": 86400, "reconcile_seconds": 3600 }
      }
    ]
  }]
}
```

```sh
culprit collect outbox init ./fleet-outbox --json
culprit collect fleet --inventory ./inventory.json --shard alpha \
  --state ./fleet-state --outbox ./fleet-outbox \
  --azure-identity ./identity.json --json
```

Each schedule can supply a complete `budget` object with the same fields as `CollectionBudget`; omitting it uses the shared collection defaults. An activity schedule similarly defaults to the normal incident policy. Unknown configuration fields are rejected. Other collection kinds are `resource-history` and `deadlock-history` (each with `window_seconds` and `overlap_seconds`), and `snapshot` (with a `dataset` matching a point collection listed above). A capture window including overlap may span at most one day; interval metadata discovery can cover up to seven days. Provider support and coverage limits remain unchanged; scheduling does not enable missing native features.

Schedules never run faster than once a minute. Positive jitter is added to the period after a completed attempt, and each physical group waits at least a minute between native operations. Pressure suppresses the whole group with increasing delays up to fifteen minutes; failed sources back off independently. Old backfill and recent Query Store reconciliation have separate durable turns, so a long backfill cannot monopolize history collection. Confirmed older captures outside the reconciliation horizon are skipped using the outbox receipts. Failed windows and unconsumed interval pages survive restart. Backfill is bounded to seven days; uncovered older windows and native-retention failures are recorded as durable gaps, rather than reported as successful collection.

Fleet authentication requires explicitly configured unattended Azure credentials for Entra connections, or `--password-env` credentials saved on a password connection. It never falls back to the Azure CLI or a keychain prompt. A changed inventory, ownership assignment or observed database identity requires draining and replacing the scheduling state; pending captures are not redirected. Ctrl+C and SIGTERM request cancellation and await the existing native shutdown path. Unconfirmed shutdown retains the workspace fence and stops the runner. `--max-work` limits work items, including interval discovery and local skips; a discovery success is not a successful data capture.

This command captures locally. Remote delivery, automatic eviction, deployment and production overhead qualification have separate acceptance gates; an outbox capacity limit pauses source work without proving any data reached a remote destination.

### Keep captures in a local outbox

Collection supports unattended Azure authentication with `--azure-identity FILE` on `collect once`, `collect activity` and `collect fleet`. An Entra connection then uses only that configured identity, with no Azure CLI or interactive fallback. For an AKS projected workload token, the JSON file contains the tenant and application IDs and the absolute token-file path:

```json
{
  "kind": "workload-identity",
  "tenant_id": "00000000-0000-0000-0000-000000000001",
  "client_id": "00000000-0000-0000-0000-000000000002",
  "token_file": "/var/run/secrets/azure/tokens/azure-identity-token"
}
```

Replace the example IDs with the provisioned identity. For VM/VMSS managed identity through Azure IMDS, use `{"kind":"managed-identity","client_id":"YOUR-IDENTITY-CLIENT-GUID"}`; `client_id: null` explicitly selects the system-assigned identity. These files contain configuration references, never passwords or access tokens. Refresh reopens rotated projected tokens and caches SQL, Storage and Kusto credentials separately. Tokens remain in the engine process. Database permissions and network access must already exist. The direct paths have local contract tests; live workload identity/RBAC validation requires the corresponding Azure host. [Federated credentials](https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-client-creds-grant-flow), [VM managed identity](https://learn.microsoft.com/en-us/entra/identity/managed-identities-azure-resources/how-to-use-vm-token).

Without the flag, `collect once` and `collect activity` may use the Azure CLI session already authenticated by their pipeline. For strict CLI-only behavior in those commands, set `CULPRIT_ENTRA_LOGIN=azure-cli`; the Azure CLI owns token refresh in that path. `collect fleet` requires explicit unattended credentials instead.

Initialize an outbox once, then use `--outbox` instead of `--output`:

```sh
culprit collect outbox init ./telemetry-outbox --json
culprit collect once shop-staging \
  --from 2026-11-09T06:00:00Z --until 2026-11-09T07:00:00Z \
  --outbox ./telemetry-outbox --json
culprit collect outbox status ./telemetry-outbox --json
```

The outbox has one local writer. It reserves space and a capture revision in a SQLite journal before reading the source. Each stream has a stable batch ID. Only a complete, verified capture whose native work has stopped can become `ready`: its file receipts, state and exact native-range head commit in one transaction. Range heads identify individual intervals; they do not establish continuous coverage of the time between them.

Revisions contain `outbox_id`, `owner_epoch` and `sequence`. Compare the latter two as decimal integers, and only within the same intact outbox. Reopening an owner allocates a new epoch on its first capture. A retry that reads the source again gets a new revision; recovery of already sealed files preserves the original revision. A standalone `--output` export has no revision.

```sh
culprit collect outbox recover ./telemetry-outbox --json
culprit collect outbox verify ./telemetry-outbox OBSERVATION_ID --json
culprit collect outbox replay ./telemetry-outbox OBSERVATION_ID \
  --output ./replayed-observation --json
```

Recovery examines unfinished attempts and the directory inventory. It commits sealed, valid files when native work was confirmed stopped; otherwise it records an interrupted or quarantined attempt without advancing its range head. Collection runs recovery automatically before reserving its first attempt. An incomplete capture is retained for inspection; there is no resume within an interval. Repeat `collect once` explicitly to reread it. `verify` audits a ready capture against its original journal receipts and quarantines corruption. `replay` verifies and copies the original bytes to a new directory, publishing the original manifest last. These outbox commands neither open database connections nor allocate capture revisions. Recovery does not rehash the whole retained backlog; use `verify` or `replay` to audit a ready capture.

An interrupted active request or an unconfirmed cancellation blocks further collection through that outbox, including after restart. First run recovery and independently establish that the old collector's database work has stopped. Then explicitly acknowledge that observation:

```sh
culprit collect outbox acknowledge ./telemetry-outbox OBSERVATION_ID \
  --source-quiescent --json
```

The workspace also keeps a durable safety record for every capture, including standalone `--output` exports. Uncertain native work blocks all new collection in that workspace, even after restart or when using a different output directory or outbox. Inspect it without connecting to the database:

```sh
culprit collect admission status --json
```

After independently verifying that the listed observation's database work has stopped, acknowledge that exact observation:

```sh
culprit collect admission acknowledge OBSERVATION_ID --source-quiescent --json
```

For an outbox capture, acknowledge both the outbox and workspace records. These commands record the operator's assertion; they do not query or cancel the database, clear a different observation, or make an interrupted capture ready. An active owner prevents acknowledgement. An embedding application must reopen its workspace after settling uncertain work. This ownership applies only to processes using the same workspace directory; separate workspaces and hosts are not coordinated.

Defaults retain up to 1 GiB of charged capture space and 256 attempts, including incomplete and quarantined attempts. `init --max-bytes` and `--max-captures` set these limits. Each attempt reserves its total-byte budget plus 256 KiB before source work; completed attempts are charged their artifact sizes plus 4 KiB. A full outbox refuses new collection before the provider starts. The SQLite database has a separate 16 MiB ceiling plus its WAL and shared-memory files. Status is paginated with `--limit` (1–500) and `--after` using the returned `next_cursor`. These local CLI commands do not automatically deliver or evict captures.

Use a private directory on a local filesystem and preserve it as a unit; do not copy, restore or share it with an active owner. Tests cover process termination around the local commit boundary. They do not establish power-loss durability or network-filesystem support.

Embedding applications can compose a remote outbox with `ObjectRecoveryStore`, `AzureBlobTransport` and `DeliveryWorker`. Remote commits permit local eviction while the analytical destination is unavailable, and a replacement delivery journal can resume without recollecting the database. The latest activity checkpoint remains local during normal eviction. Remote ownership and capture ordering are separate from database cancellation: after total volume loss, establish source quiescence independently before resuming collection. Azurite exercises the Blob protocol locally; live Azure authorization, networking and ADX ingestion are separate acceptance checks.

Run the owned, loopback-only Blob integration fixture with `python3 dev/smoke-delivery.py`. It builds the affected engine tests, creates an isolated Azurite container with synthetic credentials, runs the conditional-write/recovery test and removes the container. It does not use Azure credentials or connect to a database.

### Upgrading

Connections, saved passwords, saved queries and editor tabs carry over. Scans and reports are read only in the format the installed version saves: when an upgrade changes that format (the release notes say so), scans an earlier version saved are set aside, and history (trends, What changed, digests) starts again with your first scan after upgrading. A saved plan is read as it was saved; a later fix to a plan parser reaches a plan when it is explained or scanned again.

## Coding agents

Culprit is built to be driven by coding agents. Every command prints JSON when stdout isn't a terminal (or SARIF 2.1.0 with `--format sarif`), finding ids are stable, and connections are referenced by name, so agents never handle credentials.

**MCP server.** `culprit mcp` serves Culprit over the Model Context Protocol on stdio (no network port, no token). Tools: `list_connections`, `scan`, `dashboard`, `findings`, `finding`, `top_queries`, `query`, `query_plan`, `plan_insights`, `last_actual_plan`, `query_progress`, `what_changed`, `events`, `cloud_metrics`, `mark`, `explain`, `lint_sql`, `live`, `blocking`, `deadlocks`, `waits`, `schema`, `drift`, `index_advice`, `verify`, `export_finding`, `run_query`. All are read-only except `mark`, which adds a note to the connection's local timeline.

```sh
claude mcp add culprit -- culprit mcp                 # Claude Code
culprit mcp --connection shop-staging                 # expose only some connections
```

Cursor (`.cursor/mcp.json`), Codex (`~/.codex/config.toml`, `[mcp_servers.culprit]`) and other clients take the same command: `culprit` with the argument `mcp`.

**What agents may do** is set per connection, in the app (the "Agents" menu on each connection) or with `culprit conn set <name> --agent off|stats|query`:

| Access | Agents can |
|---|---|
| `off` | Nothing; the connection is hidden from them |
| `stats` (default) | Read findings, query statistics, schema, live sessions and estimated plans. Nothing runs your SQL |
| `query` | Also run one read-only statement at a time, each one only after you approve that exact text in the desktop app |

`run_query` refuses anything that isn't a single statement that only reads data. It runs inside a transaction that is never committed (read-only on Postgres and MySQL), returns at most 200 rows and is cancelled after 30 seconds. Approval requests show the agent's name, the connection and the exact SQL; Deny is the default, and they are denied automatically after 60 seconds or when the app isn't open.

**Audit log.** Every tool call is appended to `audit.jsonl` in Culprit's data directory (readable by your user only), with the SQL text for `explain` and `run_query`. `culprit audit` shows the latest entries.

**Set up a repository.** `culprit agent init` writes a short Culprit section into `AGENTS.md`, a Claude Code skill (`.claude/skills/culprit/SKILL.md`) and a `.mcp.json` entry, so agents in that repo use Culprit without being told. It's safe to re-run and writes no findings, host names or secrets; pass `--connection <name>` to name the connections the project uses.

More in [agents.md](agents.md).

## Speed

Measured on an Apple-silicon MacBook (macOS 26), release build, empty profile, three launches each, time from process start to the first on-screen window:

| | Launch → window (cold / warm / warm) | Physical memory after 4 s | App size |
|---|---|---|---|
| Culprit (Tauri) | 857 / 277 / 250 ms | ~99 MB across 4 processes | 8.9 MB |
| A popular native macOS database client | 1009 / 267 / 271 ms | ~44 MB in 1 process | 383 MB |

Memory is `phys_footprint` (what macOS charges the app), not RSS, which double-counts shared system frameworks. Culprit's own process is ~28 MB; the rest is the system WebKit that renders the UI (web content ~41 MB, GPU ~23 MB, networking ~7 MB), the fixed cost of a webview UI over a native one.

Reopening a connection renders its last report from disk immediately; a fresh SQL Server scan takes 0.1–0.5 s locally and 2.5–4.5 s against Azure SQL over the internet (1.4K queries, 30 days of Query Store). `CULPRIT_TRACE_STARTUP=1` prints the app's own window-shown time.

## Extending

- **Checks:** implement `culprit_core::Check` and pass it to `culprit_checks::analyze_with` or `Workspace::with_check`; it runs, ranks and locks like a built-in check.
- **Engines:** providers implement `culprit_core::Provider`; built-in ones are cargo features, and `Workspace::with_provider` plugs one in at runtime.

## Safety

- Scans read statistics and catalog views only.
- Statements Culprit runs as reads must be exactly one statement that only reads, checked under every server setting that changes parsing; on SQL Server the server confirms it first by compiling the text without running it. For agents with `query` access on production, use a read-only login: see [agents.md](agents.md).
- Postgres sessions are read-only with statement and lock timeouts. SQL Server sessions use a short lock timeout, low deadlock priority and read-uncommitted reads of the statistics views.
- Estimated plans never execute anything. Reads run in a read-only transaction everywhere; on production, anything Culprit can't prove reads needs an explicit confirmation each time, and Explain Analyze only measures provable reads.
- Statement text from a database's own history is lexed as exactly one statement before it reaches `EXPLAIN` (MySQL plans go through prepared statements). TLS certificates are verified for every non-local host unless the connection string asks for less.
- Coding agents (`culprit mcp`) read statistics and estimated plans by default. They run statements only on connections you opt in, one approved read-only statement at a time, and every call is audited.
- Fix scripts are shown and copied, never applied. They use `ONLINE = ON` / `CONCURRENTLY` where the server supports it.
- **TLS settings.** New connections show a Connection security section. TLS options in a pasted string populate the controls; an explicit selection overrides them. For an existing connection, use **Edit connection settings** on its card, including connections imported from other tools or discovered through Azure/AWS. Test uses saved credentials without changing the connection; Save changes persists the policy. PostgreSQL/YugabyteDB and MySQL/MariaDB offer their native TLS modes and a CA certificate; SQL Server offers encryption and certificate-trust settings. Token-based authentication requires verified TLS. SQLite uses a local file and has no TLS setting. For a MariaDB server without TLS, select **Disabled · no encryption**, or use `mariadb://user@host:3306/database?ssl-mode=DISABLED` when adding it. Saved settings also live in that connection's `[connections.<name>.options]` table in `connections.toml`; update its existing TLS key rather than adding a second spelling.
- Connection metadata (`~/.config/culprit/connections.toml`) and reports (`~/.local/share/culprit/reports/`) are readable by your user only. `CULPRIT_HOME` puts both in one directory.

## Building

Requirements: Rust (stable), Bun.

```sh
cargo build --release -p culprit-cli          # target/release/culprit
cd app && bun install && bunx tauri build     # desktop app
cd app && bun run dev                         # UI in a browser, backed by fixtures/*.report.json
```

`cargo test --workspace` runs the unit tests. `dev/postgres` and `dev/mssql` contain reproducible demo databases with planted problems.

### Platforms

- **macOS.** `dev/build-local.sh` builds the app and the CLI and signs both with your Developer ID, so the keychain's "Always Allow" sticks across rebuilds.
- **Linux.** The desktop app needs WebKitGTK; on Debian and Ubuntu: `sudo apt install libwebkit2gtk-4.1-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev librsvg2-dev libayatana-appindicator3-dev libssl-dev libxdo-dev`. `bunx tauri build --bundles deb` makes a `.deb`. Passwords go to the Secret Service (GNOME Keyring, KWallet); on servers, containers and CI, which have none, pass passwords with `--password-env NAME` and set `CULPRIT_KEYCHAIN=off`. Configuration and reports follow the XDG directories (`~/.config/culprit`, `~/.local/share/culprit`).
- **Windows.** Passwords go to Windows Credential Manager, and configuration and reports to `%APPDATA%\culprit`. The Azure and AWS CLIs are run as `az.cmd` and `aws.exe`. `bunx tauri build --bundles msi` makes an installer. Shortcut labels read Ctrl where macOS shows ⌘.

CI (`.github/workflows/ci.yml`) checks formatting, runs clippy and the tests on Linux, macOS and Windows, and builds the desktop app on all three, keeping the unsigned bundles as build artifacts.

## Layout

```
crates/culprit-core                model: snapshots, plans, findings; Provider and Check contracts
crates/culprit-checks              every check, as a pure function of a snapshot
crates/providers/culprit-mssql     SQL Server provider
crates/providers/culprit-postgres  PostgreSQL provider and TimescaleDB integration
crates/providers/culprit-yugabyte  Native YugabyteDB YSQL/DocDB provider
crates/providers/culprit-mysql     MySQL and MariaDB provider
crates/providers/culprit-sqlite    SQLite 3 file provider
crates/culprit-querylog            framework query log parser (Rails, Django, EF Core, Ecto, JSONL)
crates/authentication/             credential contracts, Azure/Entra and AWS authentication providers
crates/telemetry/                  ADX and ClickHouse analytical stores
crates/recovery/                   Azure Blob and S3 conditional object recovery
crates/cloud/                      pluggable cloud discovery and metrics
crates/culprit-collector           capture, outbox, fleet scheduling and verified analytical delivery
crates/culprit-read                bounded native reads and shared source admission
crates/culprit-engine              connection store, credential composition, scans, reports, agent approvals and audit log
crates/culprit-mcp                 the MCP server for coding agents and `culprit agent init`
crates/culprit-cli                 the `culprit` binary
app/                               React UI and the Tauri shell
docs/DESIGN.md                     UX and functional design
docs/guide.md                      this guide
```
