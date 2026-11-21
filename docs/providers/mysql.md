# MySQL

[Engine parity](../parity.md)

Culprit uses the MySQL provider for MySQL. `performance_schema` supplies statement and wait counters; a readable slow query log table provides an alternative workload source. MariaDB uses the same transport with its own [coverage page](mariadb.md).

## Connecting

Choose **MySQL / MariaDB** and use a MySQL URL, JDBC/.NET connection string, or host/database address. The server probe identifies the product and version.

```sh
culprit conn add shop --env staging --url 'mysql://app@db.example.com:3306/shop'
```

Configure TLS and credentials on the saved connection. Managed services can restrict individual statistics or administrative views even when the connection succeeds.

## Permissions and setup

Monitoring commonly needs `PROCESS` and read access to the relevant `performance_schema` and `sys` views. Statement-history consumers must be enabled for the history used to recover captured SQL. Culprit reports the source and grant or configuration it cannot use.

When `performance_schema` is unavailable, a readable slow query log table (`log_output=TABLE`) can provide workload samples without a performance-schema restart. Only logged runs belong to that workload; its coverage is not every application request. Changes to server instrumentation are shown for a person to apply.

## Workload and plans

| Feature | Coverage |
|---|---|
| Statement statistics | `performance_schema` digests, or grouped runs from the slow query log table |
| Timeline and period comparison | Differences between saved digest scans; individual timestamps for runs read from the slow log |
| Captured statements | Digest sample text on MySQL 8.0.3+; statement-history or slow-log samples when a digest has no usable sample |
| Estimated / measured plans | `EXPLAIN FORMAT=JSON`; `EXPLAIN ANALYZE` tree output on MySQL 8.0.18+ in a read-only transaction |
| Last actual plan | Not retained as a past-run plan by the collected statement counters |
| Plan import | JSON EXPLAIN variants and tree output |
| Runtime spread | Digest extrema and, where available, runtime histograms and percentile bucket bounds; slow-log runs have their own observed distribution |
| Parameter sensitivity | Supported histogram or logged-tail evidence, without inventing values for historical executions |
| Statement families | Digests and fingerprints merge literal variants |

A normalized `WHERE id = ?` text is not itself executable parameter evidence. A captured plan remains available when the provider cannot safely plan that normalized text again.

Statement-summary quantiles, a later native histogram and retained slow-log runs are separate observations. Each
keeps its own sample count and source; native bucket bounds are not exact execution times. Counts, means and extrema
are not borrowed between reads, and bucket estimates are not clamped to an earlier statement maximum. A runtime
tail can support an investigation without establishing parameter sensitivity or the cost of its slow executions.

## Live activity and waits

Live combines `PROCESSLIST`, transaction metadata and available lock-wait views. MySQL 8 uses `performance_schema.data_lock_waits`; older versions use `INNODB_LOCK_WAITS`. Statement-history consumers can identify a blocking transaction's earlier statement when the current process-list text is empty.

`EXPLAIN FOR CONNECTION` and stage instrumentation provide the current plan and operation progress where supported. Cancel (`KILL QUERY`) and terminate (`KILL`) actions are exposed outside production.

Historical waits are performance-schema totals, with lock timing attributable to statements where supplied. Live samples normally compare instrument counters and fall back to process-list sampling when those instruments provide nothing. InnoDB retains the latest detected deadlock, not a complete deadlock history; imported reports explain the captured pattern.

Statement lock timers retain their native provenance. Digest `SUM_LOCK_TIME` is in picoseconds; the collector exports
that exact counter and scans present its millisecond subtotal. MySQL's [8.0.28 release notes](https://dev.mysql.com/doc/relnotes/mysql/8.0/en/news-8-0-28.html)
document corrected SQL table and data-lock accounting, although the [column reference](https://dev.mysql.com/doc/refman/8.0/en/performance-schema-events-statements-current-table.html)
still describes table locks. This does not establish the same coverage for older MySQL or MariaDB. A positive timer
does not distinguish row, table or metadata locking, identify a blocker, or establish a complete canonical lock-wait
history. Zero and unavailable timers do not prove that row-lock waiting was absent. Running-query progress shows the
native waiting state without presenting the statement's cumulative lock timer as the duration of its current wait.

The slow-log `lock_time` field is retained separately for the logged runs read between the selected native bounds.
Its `TIME(6)` value is converted to milliseconds; NULL remains unknown. These are recorded subtotals, without an
assumed fraction of execution time or an execution-plus-lock-time sum. The current `long_query_time` value does not
establish historical session thresholds, other logging filters, or that every statement was captured even when it is
zero. See the [MySQL slow-log rules](https://dev.mysql.com/doc/refman/8.0/en/slow-query-log.html) and
[MariaDB's native statement timers](https://mariadb.com/docs/server/reference/system-tables/performance-schema/performance-schema-tables/performance-schema-events_statements_current-table).

### Recorded temporary statement work

Digest scans and export share the native definitions for internal temporary tables, disk tables and sort-merge
passes. The counts retain exact unsigned integers, with unsupported or null measurements left unknown. These
fields use the existing digest read. They measure neither temporary bytes nor elapsed spill cost; disk tables can
be created directly, and MySQL's counters can omit memory-mapped overflow. Native count observations do not establish
memory exhaustion or size a new memory limit. `statement-work` explains published counter points using the same
pure view as scans, without inferring rates, SQL definitions or an allocating operator.

### Tracked session allocations

`culprit scan shop --collect-session-memory` requests native thread-allocation counters on MySQL 8.0.31+.
Default scans skip them. The shared-instance collector's `session-memory` profile uses the same reader and decoder.
It reads the current and high-water values of total and controlled tracked allocations, preserving exact integer
bytes and unknown values. These are neither execution grants nor process resident memory. Present instrumentation
settings cannot prove that tracking covered the session's earlier lifetime. See the native
[thread columns](https://dev.mysql.com/doc/refman/8.0/en/performance-schema-threads-table.html).

Current-event context has its own observation clock and native event generation. It does not identify the statement
that made an allocation. Running-statement memory peaks are not used as current allocation measurements. Native
SQL text, client labels and connection attributes are not selected by this profile. Only the data connection whose
native identity was verified is excluded; other monitoring sessions remain unclassified.

The reader makes one unsorted thread projection and, when available, one separate current-event projection. It
joins exact thread identities in memory, with no server-side join or per-thread queries. Disabled tracking leaves
measurements unknown; missing current-event permission preserves readable allocation observations with partial
coverage. Source identity and instrumentation settings are rechecked before confirmed close and publication.

Performance Schema's native container iteration is not an index seek, and a row limit does not bound all its work.
Scans and collector captures share 24-query, two-second statement, ten-second total, 1 MiB page and 8 MiB total-byte
ceilings, preserving stricter caller bounds. The profile defaults off; representative high-volume
qualification remains required. MariaDB has no qualified route for this profile. Culprit does not fall back to its
process-list memory view, which can enumerate and lock threads before applying a filter or limit.

### Retained native diagnostics

`culprit scan shop --collect-native-failures` explicitly requests metadata from MySQL 8.0.22+'s native
`performance_schema.error_log` buffer. Default scans skip it. Scans and the shared-instance collector profile
`native-failures` use one reader, decoder and context verification. MariaDB, older MySQL and disabled Performance
Schema are explicit unsupported routes; enabling logging or changing permissions is a separate operator action.

The projection reads at most 128 records plus one truncation witness, without filters, ordering, joins or server-side
JSON processing. It never selects the free-form `DATA` column, which can contain application values. UTC timestamps,
exact decimal connection IDs, native log codes, priorities and subsystems remain available. A native connection ID
has no recorded generation and is not a Performance Schema thread ID, so it cannot identify a current query owner.

The native server UUID and owned session context are rechecked before publication. They qualify the hosting read,
not the original emitter of every retained row: the buffer may load older log files. Per-entry reads are not an
atomic event history; logging filters, eviction and restart affect visibility. Equal records at different capture
positions remain separate, and an empty buffer never establishes the absence of failures. Exact native OOM/resource
codes select investigations; arbitrary messages and client numeric error codes do not supply that classification.

Scans and collector captures share the same 24-query, two-second statement, ten-second total, 1 MiB page and 8 MiB
total-byte ceilings, preserving stricter caller bounds. These are admission bounds, not a production-load
qualification. See the [native error-log reference](https://dev.mysql.com/doc/refman/8.4/en/performance-schema-error-log-table.html)
and [server log-code vocabulary](https://dev.mysql.com/doc/mysql-errors/8.4/en/server-error-reference.html).

The bounded unfiltered cursor starts with the oldest retained entries. A full buffer can therefore leave newer
failures outside every capped read until earlier entries expire. Partial reads expose that limitation; this opt-in
diagnostic sample does not provide continuous or guaranteed recent-error coverage. A recent feed requires its own
qualified acquisition policy.

### Recorded scheduled events

The existing native-diagnostics scan option also requests a bounded Event Scheduler catalog read. Default scans
skip it. The explicit `mysql.jobs` investigation profile uses the same acquisition, native context and decoder.
The canonical jobs report is shared by scans, authenticated local/store captures, CLI, MCP and the folded UI.

Only the selected database's visible `information_schema.EVENTS` metadata is read. Definitions, definers and
comments are not selected, and collection never creates, enables or executes an event. Native status, schedule,
time zones and last-start fields remain separate from outcomes. `LAST_EXECUTED` does not establish success or a
failure result; the global `event_scheduler` setting is configuration and can remain `ON` after the scheduler stops.
Native local catalog clocks are never converted to the UTC acquisition clock.

Direct global/schema `EVENT` privileges prove catalog visibility. Role-only or unknown visibility and capped
rows remain partial; a missing row or empty catalog cannot prove an absence of execution failures. The source
requires its existing native member/process markers (MySQL 5.7+ or a MariaDB release exposing `server_uid`),
rechecks the original connection, member, process, role and configuration and retains unknown reset/continuity.
The process marker has one-second resolution; event names and catalog times are not persistent incarnations.

The profile retains at most 128 rows plus one lookahead and intersects caller limits: 25 native dispatches including
owned session setup, two-second statements, ten-second total, 64 KiB pages and 512 KiB total. The scan route needs at
most 20 dispatches; retained collection pays the final verification. LIMIT bounds returned rows, not the catalog's
internal metadata work. The source remains Unqualified and requires owned native/cost acceptance before automatic
production admission. See [native columns](https://dev.mysql.com/doc/refman/8.4/en/information-schema-events-table.html),
[clock semantics](https://dev.mysql.com/doc/refman/8.4/en/events-metadata.html) and
[scheduler configuration](https://dev.mysql.com/doc/refman/8.4/en/events-configuration.html).

## Schema and indexes

`information_schema`, `performance_schema` and `sys` provide the catalog, table activity, index usage and available column histograms. Missing-index candidates are derived from workload, plans and foreign keys rather than a native missing-index DMV.

MySQL 8.0+ can make an eligible index `INVISIBLE` before removal; it remains maintained. MySQL 5.7 cannot hide it, so advice preserves the definition before suggesting removal. Unknown versions do not establish index-hiding support. Foreign-key backing indexes and unique keys are protected from ordinary cleanup advice.

The schema view, migration linter and drift use these same collected definitions and measurements.

## Limits and evidence

### Background telemetry

The collector shares the app's owned native reader and source layouts. It exports exact digest counters, bounded
catalog/operational observations, persistent activity and InnoDB's latest deadlock report. It does not execute
application statements or assume a digest's first-seen time establishes its reset. Required instrumentation and
visibility govern complete coverage. See [collector source coverage](../collector.md#other-provider-source-coverage)
for datasets, cadence policies, cancellation and retention limits.

Digest, histogram, table and index counters can have different reset histories. Histograms count their recorded period, not an arbitrary selected window. A missing histogram can mean an unsupported version, disabled instrumentation, permissions or collection failure; it is not proof of one particular cause.

Culprit does not attribute shared redo or host observations to this database's query delays without matching evidence. Transaction age does not establish when a consistent read view was acquired. Missing lock instrumentation leaves unresolved blocking rather than a clean bill of health.

A slow-log workload covers only logged runs and its own timestamps. Rates and unused-index conclusions follow the shared [evidence rules](coverage.md).
The table's returned-row limit does not bound its unindexed native scan or sort; the owned read timeout still applies.

### Group Replication observations

The explicit `mysql.group-replication` investigation recipe captures one native observer’s Group Replication view.
Use resource discovery to obtain the native shared-instance identity, then select `group-replication` with that
identity. Ordinary scans and asynchronous replication-channel reads are unchanged. MariaDB Galera is a different
native source and is not admitted by this recipe.

The reader observes MySQL 5.7.17+ and Performance Schema before compiling plugin/table references. It discovers
both table layouts, preserves unknown optional columns, and reads membership and statistics independently.
Member UUID/state/role, certification and applier queues, certification rows and transaction/conflict totals
retain their native meaning. Remote statistics propagate periodically; neither one group view nor the observer’s
process marker establishes the remote counters’ reset history. No delta, rate, lag duration, byte backlog,
application-query owner or overall-health verdict is derived.

The source intersects caller limits with 23 SQL phases (including existing session setup, preparation and final
verification), one second per statement, five seconds total, 128 retained rows per table and one MiB total.
Layout discovery retains at most 64 columns plus a lookahead. Row caps do not establish bounded server work on
these unindexed native tables. The recipe remains cost-Unqualified and is not automatically scheduled.
A missing plugin/layout or denied SELECT is unavailable, not a healthy empty group; the other readable source
can remain useful. No installation, grants or telemetry setup are performed.

Original captures replay through the canonical SDK, both offline stores, local CLI/MCP analysis and explicit
saved-scan attachment. Historical native scope/time remain independent of the current scan, and sharing
withdraws the indivisible original rather than manufacturing replacement native identities.

Native definitions: [members](https://dev.mysql.com/doc/refman/8.4/en/performance-schema-replication-group-members-table.html),
[statistics](https://dev.mysql.com/doc/refman/8.4/en/performance-schema-replication-group-member-stats-table.html),
[remote-statistics propagation](https://dev.mysql.com/doc/refman/8.4/en/group-replication-replication-group-member-stats.html)
and [SELECT privileges](https://dev.mysql.com/doc/refman/8.4/en/performance-schema-table-characteristics.html).

### Operational evidence

InnoDB redo buffer waits, auto-increment bounds and metadata blocking involving exclusive locks retain their source scope and measurement period.

Digests are aggregates, not request traces. Other metadata-lock modes and queue effects can remain unresolved; missing instrumentation is explicit. MariaDB replication and Galera evidence are documented on the [MariaDB page](mariadb.md#limits-and-evidence).

## Verification

Verified versions include MySQL 5.7.44 and 8.4; the saved fixture records MySQL 8.4.11. The [MySQL fixture](../../dev/mysql/README.md) provides repeatable schema, workload, blocking and deadlock cases.

| Version or source | Behavior |
|---|---|
| MySQL 5.7 | No `EXPLAIN ANALYZE`, digest sample text, invisible indexes or MySQL 8 lock-wait view; the provider uses the available history and lock sources instead |
| MySQL 8.0.19+ | Runtime histograms can be available when the instrumentation and permissions allow them |
| Older MySQL 5.7 TLS builds | A server offering no shared cipher suite is reported as such; only TLS modes that permit plaintext fallback can fall back |

MySQL 5.6 and earlier, Percona Server and MySQL 5.7 with `performance_schema` off have not been verified. Source declarations live in [`culprit-mysql/src/sources.rs`](../../crates/providers/culprit-mysql/src/sources.rs).

Source references: [MySQL metadata-lock view](https://dev.mysql.com/doc/refman/8.4/en/sys-schema-table-lock-waits.html).


Selected Galera observations use `mariadb.galera`, an explicit investigation recipe, independently of MySQL Group Replication and asynchronous channels. The source uses the existing MariaDB native `server_uid` availability policy and process owner, then reads a fixed allowlist of native `wsrep` status variables. Ordinary scans keep their existing five-variable cluster projection; they do not start this capture. Unsupported identity/layout, disabled `wsrep`, native NULL and partial status visibility remain separate. The original node view is shared instance evidence; it is not attributed to the current database or its queries.

Receive/send queue lengths count current write sets waiting to apply/send. Certification failures, applier aborts, transaction replays, local commits and paused nanoseconds remain independent native totals with unknown reset history. The native paused fraction covers the interval since `FLUSH STATUS`; Culprit never issues that command and does not know that reset point. Read-relative flow-control event counters and queue averages/min/max are deliberately excluded. No rates, cluster-health verdict or cause share is inferred.

The recipe is Unqualified and does not widen default or adaptive collection. Its bound includes seven existing session setup statements, three three-query native context checks and one fixed status read: at most17 queries, one exported original point,1s per statement and5s total. The status result itself is bounded to17 allowlisted variables plus one defensive overflow row and32KiB. SHOW has no row timestamp, so the original native UTC endpoints bound the status read. Matching endpoint identities/configuration, cluster UUIDs or process observations do not prove continuous membership or counter continuity.
