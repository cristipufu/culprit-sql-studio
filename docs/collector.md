# Background collection and analytical delivery

The collector archives database observations through the CLI, using the same provider readers as scans and live
views. Database access stays in the collector's network. An analytical destination receives prepared observations;
investigations do not need a new connection to the production database.

Remote delivery supports ADX and ClickHouse through one shared preparation, verification and publication path.
Private Azure Blob or S3 storage retains native recovery evidence through the same conditional-object protocol;
ADX defaults to queued ingestion using temporary storage and queues supplied by ADX. An explicit Event Grid mode
uses a separate Azure ingestion-ready container and Event Hubs. The actual managed ingestion routes,
cloud permissions, private networking, retention-scale costs and production source-load budgets
require live qualification. The deployment example starts with **zero replicas**. Its resource requests and cadences
are starting configurations, not measured capacity claims.

## Components and ownership

| Component | Responsibility |
|---|---|
| Provider native reader | One implementation of a source's SQL, paging, scope, units and native completion |
| `TelemetrySource` / `SourceFactory` | Describe and read one bounded native observation; settle cancellation |
| `FleetBackend` / `FleetRunner` | Source authentication, static assignments, due work, backfill and coverage gaps |
| `Outbox` | Exclusive local ownership, capture revisions, private files, quota and incident checkpoints |
| `RecoveryStore` | Immutable native objects, fenced commits, historical coverage and unfinished delivery index |
| `TelemetrySink` | Destination dispatch, exact value/membership readback and completion publication |
| `DeliveryWorker` | Independent native archive and analytical loops, with separate durable progress journals |
| `CollectorService` | Stop admission and await both source and delivery owners during shutdown |
| Engine service configuration | Compose credentials, provider registry, recovery adapter, destination and probes |

Additional sources implement the existing provider/telemetry contracts. Additional recovery or analytical adapters
implement their respective interfaces; they do not acquire database connections. Interactive analysis still consumes
`Snapshot` through the existing pure analysis pipeline. Reading archived telemetry back into a Culprit report is a
separate consumer; this service does not add an ADX-backed desktop connection.

## Configure and validate

The examples are [collector configuration](../deploy/collector/config.example.json),
[connection metadata](../deploy/collector/connections.example.toml) and
[Kubernetes resources](../deploy/collector/kubernetes.yaml). Replace every `REPLACE_...` value. Use a separate static
shard and recovery namespace for each owner. A physical pool or server must not be assigned to multiple shards.

`workspace` holds saved connection metadata and the independent native admission journal. `state` holds the outbox,
fleet, archive and analytical delivery journals and encoding scratch files. They must be distinct absolute paths. Both need durable
storage; persisting only the outbox loses part of the source ownership proof.

```sh
culprit collect service validate --config collector.json
culprit collect schema --store adx --format json > adx-assets.json
culprit collect schema --store clickhouse > clickhouse-schema.sql
```

Validation reads saved metadata, checks unattended authentication and configures the adapters. It acquires no tokens,
opens no database, object-storage or warehouse connections and creates no runtime state. `collect schema` generates DDL, mappings, canonical
queries and native ingestion assets without opening a workspace or applying anything to a service.

Credential profiles are independent from source and destination selection. The example names four Azure profiles:
`source`, `recovery`, `analytics` and `ingestion`. `source_credentials` maps saved connection names to their own
unattended authentication profiles;
password-authenticated connections still use their saved password environment references. Recovery's `credential`,
ADX's query `credential` and `delivery.credential` can select different managed or workload identities. In Event Grid
mode, `delivery.credential` authenticates staging Blob writes. Explicitly selecting
the same profile reuses that credential owner and its audience caches. Nothing inherits a different role's identity.
A fleet may mix Azure and AWS sources without sharing identities. Delivery-only mode neither resolves source profiles nor requires saved source connections.

For ClickHouse, replace `target` and add a password profile:

```json
{
  "target": {
    "kind": "clickhouse",
    "query": {"endpoint": "https://warehouse.example:8443", "database": "culprit", "insert_quorum": null},
    "credential": "clickhouse"
  },
  "credentials": {
    "clickhouse": {"kind": "password-environment", "username": "culprit", "variable": "CULPRIT_WAREHOUSE_PASSWORD"}
  }
}
```

Merge that profile into the existing `credentials` object; keep the separate recovery profile. The password's value
is read only when opening the sink. It is sent in an HTTP header, never a URL, serialized configuration or error.
HTTPS is required except for literal loopback fixture endpoints. Redirects and host proxies are disabled. The
ClickHouse adapter does not depend on Azure authentication or Azure Blob; the service's chosen recovery transport
remains independent of its analytical destination.

### Recovery providers

Recovery configuration names a provider and gives it native options; the collector does not interpret bucket,
container, identity or endpoint fields. The built-in providers are `azure-blob` and `s3`. Their Cargo features,
`recovery-azure` and `recovery-s3`, are independent of `store-adx` and `store-clickhouse`. For example:

```json
{
  "recovery": {
    "provider": "s3",
    "options": {
      "bucket": "culprit-private-recovery",
      "region": "eu-west-1",
      "prefix": "pilot/shard-0",
      "expected_owner": "123456789012"
    },
    "credential": "archive-aws"
  },
  "credentials": {
    "archive-aws": {"kind": "aws", "profile": "culprit-archive"}
  }
}
```

Merge this profile with the other role profiles. The AWS adapter resolves the named AWS CLI profile with
`aws configure export-credentials --format process`; that profile can use the AWS CLI's configured role or workload
credential source. It refreshes expiring credentials through that profile alone. Culprit never runs `aws login`
automatically or falls back to another identity. Credentials and session tokens stay out of configuration and errors.
The source's AWS IAM profile can be a different profile from recovery's, even in the same fleet.

Provision the private bucket separately. Derived AWS endpoints require the expected 12-digit owner account and sign
that constraint on every request. S3-compatible storage accepts an explicit trusted HTTPS origin through `endpoint`;
HTTP is restricted to loopback fixtures. Native object writes sign exact payload hashes and conditional headers;
redirects and proxies are disabled. A precondition failure is distinct from an ambiguous conflict or transport error.
An upload owns its opened file through native completion, including when its caller is cancelled.

Restrict access to this shard's prefix and the required GetObject/PutObject operations. Keep public access blocked,
require TLS, configure bucket encryption and any required KMS permissions, and retain the evidence for the required
recovery period. Do not expire or delete recovery objects while the collector still relies on them. Use CloudTrail
S3 data events or equivalent storage audit logs to investigate writes. Account for PUT/GET requests, retained bytes,
KMS requests and cross-region/cloud egress, particularly when S3 recovery feeds ADX. These fixtures do not establish
managed AWS durability, IAM or networking guarantees.

Embedders implement `RecoveryProvider` and register it with
`CollectorRuntime::empty().with_recovery_provider(...)` (or add it to the defaults). The provider receives an
explicit `CredentialResolver`; `CollectorRuntime::with_credentials(...)` injects an embedding profile registry.
Neither recovery nor destination adapters discover identities. `ObjectRecoveryStore` implements immutable payloads,
ETag-fenced commits and restart recovery once over `ObjectTransport`; Azure and S3 implement only native object I/O.
The same runtime instance owns validation, initialization and service execution. Normal startup never initializes a
missing namespace. Recovery fencing cannot stop an orphan database query; source admission remains independent.

`python3 -B dev/smoke-s3.py` runs the same recovery contract as Azure Blob against an owned, digest-pinned RustFS
fixture, then verifies real ClickHouse delivery and publication after restarting S3-backed recovery. Synthetic
credentials and loopback ports are local to the fixture; both containers and their volumes are removed on exit.
This complements `dev/smoke-delivery.py` (Azurite), `dev/smoke-clickhouse.py` and the final live ADX qualification.

### ADX delivery modes

Queued ingestion is the default when `delivery.transport` is omitted:

```json
{
  "target": {
    "kind": "adx",
    "query": {"endpoint": "https://CLUSTER.REGION.kusto.windows.net", "database": "culprit"},
    "credential": "analytics",
    "delivery": {"credential": "ingestion"}
  }
}
```

The query identity needs read access for exact verification. The delivery identity needs ingestion access to the
generated tables. Their profiles are explicit and independent. Schema deployment uses a separate identity.

Culprit obtains temporary storage locations, an ingestion queue and authorization evidence from the Kusto ingestion
service. It uploads each prepared batch to that temporary storage and submits its location to the queue. ADX then
downloads, batches and ingests it. This uses the established
[queued ingestion protocol](https://learn.microsoft.com/en-us/kusto/api/netfx/kusto-ingest-client-rest), rather than
the preview queued REST endpoint or synchronous inline ingestion. It requires no customer-provisioned staging
account, Event Grid subscription or Event Hub. The ingestion endpoint defaults to `https://ingest-CLUSTER.REGION.kusto.windows.net`;
set `delivery.transport` to `{"kind":"queued","endpoint":"https://ingest-ALIAS.REGION.kusto.windows.net"}` for an explicit endpoint.

Durable recovery storage remains a separate requirement. ADX consumes its temporary staging files; it does not read
the native recovery archive. With S3 recovery, queued ADX delivery requires no customer Azure Storage account.
Projection verification indexes are retained in the recovery namespace, independently of the selected ADX transport.

Service-issued SAS URLs and authorization evidence stay in memory. Their cache expires within five minutes and
earlier when storage credentials approach expiry; failed delivery invalidates it. Each client permits at most two
native deliveries concurrently. Upload and queue requests disable redirects and proxies, bound response bodies and
use service-issued Azure storage endpoints only. Caller cancellation retains the native worker, encoded file and
concurrency permit until the request settles. Batching stays enabled. Queue acceptance is only a hint: exact data and
publication readback still gate retirement, through the same lifecycle as Event Grid delivery.

ADX's default batching policy can delay each ingestion stage by five minutes. The service checks visibility every
minute and allows six minutes before emitting a new attempt, avoiding repeated uploads during normal batching.
That deadline is persisted across restart. Data and the
completion record are separate stages, so this is a historical telemetry pipeline with several minutes of latency.
Qualify table batching policies and freshness requirements before using it for urgent activity alerts. See
[ADX batching policy](https://learn.microsoft.com/en-us/kusto/management/batching-policy).

To keep the Event Grid route, configure its staging identity and container explicitly:

```json
{
  "delivery": {
    "credential": "staging",
    "transport": {
      "kind": "event-grid",
      "ready": {"account": "culpritstaging", "container": "ready", "prefix": "pilot/shard-0"}
    }
  }
}
```

This mode requires Blob Storage, an Event Grid subscription, Event Hubs and an ADX data connection. The staging
identity needs Blob access; ADX's ingestion identity needs access to the staging files and notifications. See
[Event Grid data connections](https://learn.microsoft.com/en-us/azure/data-explorer/ingest-data-event-grid-overview).
The generated `routes` describe the table mappings and ready-file filters for this optional mode. Both transports
use the same generated schema, recovery evidence, exact verification and publication records.

### ClickHouse deployment and durability

Apply the generated SQL explicitly to the configured database before starting delivery. Give the collector only
SELECT/INSERT access to the generated tables; use a separate deployment identity for DDL and preserve append-only
landing data. Ordinary collector startup
never provisions or mutates schema. The schema contains 15 analytical landing tables and one publication table;
recovery-only activity checkpoints have no analytical table. Landing tables sort by range, batch and ordinal for
bounded readback. A publication-table projection supports resource/time selection without duplicating every payload.

`--store clickhouse --replicated` emits `ReplicatedMergeTree` tables using server-configured replication paths/macros.
For that layout, configure a positive `insert_quorum` and a read/write endpoint with the required replica consistency.
Single-node tables use synchronous inserts and local fsync; that does not promise survival of node or disk loss.
Replica failover, distributed routing and cloud deployments need separate qualification. The owned native fixture
verifies single-node ClickHouse 26.8.36.3, not those deployment guarantees.

The adapter keeps at most two HTTP requests in flight. Queries have a 30-second execution limit, a 45-second transport
limit, two execution threads, 256 MiB memory/read-byte limits, one million read rows and a 16 MiB response cap. Receipt
pages also respect the shared projected-byte limit. Limit or partial-response failures retain local/recovery evidence;
a successful HTTP acknowledgement cannot retire it. These bounds may require smaller captures at high volume. Tune
capture/range sizes and warehouse capacity using measurements; do not replace exact readback with count-only checks.

Query the parameterized `Culprit<Table>Rows` views rather than raw landing tables. They admit published, complete,
unambiguous batches, collapse identical retries and select revisions by owner epoch, capture sequence and representation.
`Current` views additionally select fresh complete source heads, including successful-empty captures. Partial visibility
does not replace a complete head. Scope every view call by contract, recovery store, database and observation time;
ClickHouse datetime query parameters use UTC `YYYY-MM-DD HH:MM:SS[.nnnnnnnnn]` values. JSON facts are preserved as JSON
text; exact counters use decimal columns and timestamps retain nanoseconds. ADX's native presence columns and 100 ns
precision do not affect ClickHouse.

Run `python3 dev/smoke-clickhouse.py` for native delivery, restart/index reuse, exact values, revision/current-set
semantics and invalid-membership checks. It owns a pinned, resource-bounded loopback container and removes it on exit.
Kusto mappings/functions and Azure Blob conditional recovery have separate owned fixture gates. None of these
fixtures establish production source-load budgets or replace the final live ADX qualification.

The example includes activity, Query Store history, resource history, deadlock history and SQL Server point-observation
families. Each has its own cadence and budget. A family being scheduled does not establish source availability:
permissions, engine edition, enabled native features, retention and complete visibility still govern its coverage.
Unavailable and truncated sources must remain distinguishable from complete empty results.

Collection admits one native operation at a time per workspace and keeps a one-minute minimum gap between jobs on
the same configured physical resource. Relative lateness prioritizes frequent schedules without shortening that
guard; pressure extends the delay. Cadences are minimum requested periods, not guaranteed sampling rates. Inspect
missed-work metrics and qualify the combined workload before adding targets or shortening periods. The example's
activity threshold is 30 seconds for blocking and 60 seconds for running requests, with a three-minute poll period.
Transient activity between polls is not a complete event history.

## Database and shared subjects

A saved connection is a route. Each capture separately selects the database or an exact native shared resource.
`collect once` defaults to the connection's database. Discover supported shared resources through bounded metadata
reads, then save the selected entry's `identity` object as a JSON file:

```sh
culprit collect resources shop-staging --kind pool --json > resources.json
jq -e '.[0].identity' resources.json > pool.json
culprit collect once shop-staging --resource ./pool.json --collection resource-history \
  --from 2026-11-13T10:00:00Z --until 2026-11-13T10:05:00Z --output ./pool-capture --json
```

Choose recent completed bounds within the source's actual retained range. No discovered resource means there is
no eligible resource through that route; a permission failure remains an error. The identity file is bounded to
64 KiB, must contain the complete returned identity, and never substitutes a connection alias for native identity.
The adapter verifies the requested resource before reading its statistics. Unsupported kinds or collections are
rejected; they never fall back to collecting the member database.

A native sample can arrive after its timestamp. `source-not-ready` keeps the exact requested window pending until
the source has closed it; it neither advances the checkpoint nor records a retention gap. Only a proven missing
lower history bound permits a native-retention gap. Waiting does not count as a completed capture.

| Native route | Discovery | Capture | Meaning and limits |
|---|---|---|---|
| Azure SQL elastic pool, through a writable member database | `--kind pool` | `--resource pool.json --collection resource-history` | Pool demand/storage with explicitly broader instance context; no fabricated replica identity or fixed sampling period |
| PostgreSQL instance, through a TCP database connection | `--kind instance` | `--resource instance.json --collection operational` | Shared archive, writer, WAL, I/O, checkpointer and available statistics-eviction counters; database counters stay on database captures |

Azure pool history uses `sys.dm_elastic_pool_resource_stats`, whose documented retention is approximately
40 minutes. It is distinct from the 14-day `sys.elastic_pool_resource_stats` history in `master`. The native
source requires `VIEW SERVER STATE`; Culprit does not grant permissions. Pool identity comes from the logical
server and pool metadata, while the member's logical/physical database identities anchor capture verification.
Membership is rechecked before each page. A pool move or changed member storage prevents complete publication.
The source does not prove a pool incarnation, so continuity remains unverified. See Microsoft's
[pool-history reference](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-elastic-pool-resource-stats-azure-sql-database?view=azuresqldb-current).

PostgreSQL uses its native system identifier and server address/port, independently of the connection's database.
This identifies system lineage and the observed network endpoint, rather than a globally unique physical host.
Discovery requires access to `pg_control_system()`; unavailable permissions or a Unix-socket connection without a
native network address cannot produce a guessed identity. The instance route reads `pg_stat_statements_info`
only when that route's database exposes the extension's view. Select an appropriate existing route when this
source is needed; Culprit never installs an extension or changes its settings. PostgreSQL documents the
[control-data functions and server address](https://www.postgresql.org/docs/current/functions-info.html).

Fleet targets have a required stable `id`, a connection and an explicit `subject`. Database jobs use
`{"kind":"database"}`; shared jobs set `kind` to `shared` and `resource` to the complete returned identity.
A database target and a shared target may use the same connection, with independent schedules, checkpoints and
health. Assign the shared resource once in the complete inventory: duplicate native shared identities are rejected
even through different member aliases or shards. Keep related targets in the same `admission_group` and shard. That group enforces scheduling policy; it
does not establish native scope. Separate deployments still need coordinated assignments and state ownership.

## PostgreSQL source coverage

PostgreSQL uses the same capture, outbox, fleet and delivery path as SQL Server. Configure PostgreSQL connections
with unattended saved credentials, then schedule only the collections the provider supports. SQL Server's example
schedules must not be copied wholesale: PostgreSQL has cumulative counters rather than Query Store intervals.

| Selection | Native observations | Suggested starting cadence |
|---|---|---|
| `snapshot: statement-counters` | `pg_stat_statements(false)`: calls, duration, rows, available buffers, WAL, I/O timings, planning, JIT and runtime distributions | 5 minutes |
| `snapshot: operational` | Current-database health, conflicts, XID/MultiXact ages and subscription errors; shared counters use the explicit instance route above | 5 minutes |
| `activity` | Persistent requests, lock waits, blocker closure, idle transaction context, lock resources and incident transitions | 3 minutes |
| `snapshot: replication` | Senders, local standby state and physical/current-database logical slots; unknown lag stays unknown | 10 minutes |
| `snapshot: settings` | Allowlisted effective settings, native unit/context/source, defaults and pending restart | 1 hour |
| `snapshot: tables`, `indexes`, `statistics` | Catalog size estimates, native usage/maintenance counters and safe planner summaries | 1 hour |
| `snapshot: statement-definitions` | Separately bounded, redacted normalized SQL text | 1 hour |
| `snapshot: columns`, `foreign-keys`, `check-constraints`, `objects`, `triggers`, `partitions` | Native object identities, attributes, relationships and expression fingerprints | 6 hours |
| `snapshot: sequences`, `cleanup` | Sequence bounds and current transaction/slot/prepared-transaction cleanup horizons | 1 hour |

These cadences are starting policies, not measured production budgets. The fleet's shared resource gap still applies
across profiles. The CLI uses `--collection statement-counters` for a point observation; JSON
schedules use `{"kind":"snapshot","dataset":"statement-counters"}`. Keep definitions and catalog work out of the
fast health profile. The collector does not run application statements, EXPLAIN, table scans or expression deparsing.
It reads one bounded native response and streams provisional pages; limits, missing visibility and an identity change
prevent complete publication. Activity reads lock resources only after an aged lock wait and bounds blocker traversal.

Counter values are current native totals or gauges, not fabricated time buckets. Per-entry reset timestamps are used
when PostgreSQL exposes them; a global reset does not prove each entry's lifetime. Unknown resets cannot establish
rates, and minimum/maximum statistics retain their separate reset epoch. Disabled I/O or planning instrumentation
may produce native zero totals; consult the captured settings before interpreting those totals as observed work.
Server-scoped observations must not be summed as if each database owns them. Settings describe the collector's effective
session, including its safety overrides; they do not reveal every application's settings or a pending configured value.

Full statistics visibility is required for complete activity and restricted native sources. Planner statistics also
need visibility of the selected relations. Activity capture requires PostgreSQL 14+ for native lock-wait start times;
other version-dependent fields are detected explicitly. The complete native capture and cancellation tests run
against an owned PostgreSQL 18.6 fixture. Managed-service permissions, TimescaleDB-specific storage and production
source cost require their own qualification.

PostgreSQL supplies a deadlock counter, not retained SQL-accessible deadlock graphs. The counter is included in
operational observations. Graphs still require server logs, which the app's existing importer can parse; a scheduled
PostgreSQL `deadlock-history` collection is explicitly unsupported. Query/runtime interval history, historical wait
events, CPU utilization, application parameters and executed plans are not invented from current statistics.

## Other provider source coverage

All providers use the same capture, outbox, recovery, incident and delivery components. Native readers own SQL,
permissions, cancellation and measurement meaning; the scheduler and ADX adapter do not select database-specific
queries. Unsupported selections fail explicitly rather than returning a complete empty observation.

| Provider | Point observations | Activity and retained evidence |
|---|---|---|
| MySQL / MariaDB | Statement counters and definitions; operational, settings, tables, indexes, columns, foreign keys, check constraints, objects, triggers, partitions, statistics, replication, sequences and cleanup | Persistent requests and blockers; InnoDB's latest deadlock report, with limited retention |
| YugabyteDB | Node-local statement counters and definitions; operational, settings, tables, indexes, columns, foreign keys, constraints, objects, triggers, partitions, statistics and retained plans | Persistent YSQL requests and bounded DocDB blocker lookups; sampled ASH and retained terminations are operational observations |
| SQLite | Settings, operational file metadata, tables, indexes, columns, foreign keys, objects, triggers, statistics and sequences | No server sessions, native statement counters or retained deadlock history |

Keep the three-minute activity and five-minute health cadences as conservative starting policies, subject to the
shared resource gap. Catalogs and definitions belong in slower profiles. These are policy defaults, not measured
production overhead guarantees. Native fixture tests exercise capture, cancellation, restart/replay and ADX
projection; production load qualification and the live ADX round trip remain separate.

MySQL and MariaDB retain exact picosecond counters. Digest entry eviction and independent resets remain unknown
unless native evidence establishes them. Disabled instrumentation, insufficient visibility and digest overflow
prevent complete coverage. Histogram export contains metadata, never histogram values. The deadlock source can
miss overwritten reports between polls; repeated reads do not create new historical events.

Yugabyte exports node scope, not fabricated cluster totals or PostgreSQL heap/buffer statistics. Native fan-out
is capped before expensive distributed lookups. ASH is sampled task activity, not CPU time. QPM is an independently
evictable plan cache; its averages are not converted into exact duration totals, and it requires effective
`pg_read_all_stats` access because the verified server's foreign-role redaction path is unsafe. CPU and memory
observations describe shared hosts/TServers. Optional native features can remain unavailable.

Activity distinguishes positive blocking evidence from unavailable or capped lookups. An unresolved lookup marks
an existing incident `uncertain`, preserving its last verified timestamp and blocker context. It does not resolve
the incident or silently claim fresh evidence. Repeated uncertainty does not emit repeated change notifications.
The next complete request frame can refresh or resolve the incident.

SQLite opens files read-only with trusted schema disabled. Its background metadata read has a 250 ms ceiling,
releases its transaction and closes the file before destination paging, and validates file identity through
completion. It does not run `dbstat`, execute application statements or export schema/default bodies. Framework
query logs remain separate app-supplied evidence, not native telemetry history.

Timescale uses the PostgreSQL reader and native counter semantics. Hypertable/chunk-specific size aggregation is
not part of background telemetry; base relation observations must not be interpreted as logical hypertable totals.
SQL Server background admission remains restricted to the separately qualified Azure SQL Database topology.

The current export format is **14**. New contracts require new collector/recovery state; there is no migration of
old saved captures. SQL text and retained plans pass through the common payload inventory and redaction rules.

## Recovery and activation

Cloud resources and permissions are provisioned separately. The explicit initialization command creates only a new
recovery namespace and refuses to overwrite an existing one:

```sh
culprit collect service init --config collector.json
```

Before activating an owner, independently establish that the previous owner's SQL requests have stopped:

```sh
culprit collect service run --config collector.json --source-quiescent
```

The flag asserts source quiescence for this activation; a Blob epoch or a missing local volume does not prove it.
Existing unconfirmed native work in either journal still blocks collection; archive delivery and probes remain alive
with the source fenced. Finite `--max-work` runs return that error. Do not force-delete an owner, delete its
journals, or treat an expired storage lease as permission to resume SQL. Follow the explicit admission/outbox
acknowledgement commands in the [guide](guide.md#collect-native-telemetry-to-files) after verifying native quiescence.

To recover and deliver data without source credentials or a source-quiescence assertion, run:

```sh
culprit collect service run --config collector.json --delivery-only
```

This mode opens no database connection, does not restore native activity state or allocate a source epoch, and never
clears a native debt. It cannot be combined with `--source-quiescent` or `--max-work`. Keep the original destination
contract and recovery namespace. Source schedules are explicitly disabled in health metrics.

Startup restores the latest complete activity checkpoint for each native source before
allocating a new capture revision or opening a database session. Restored checkpoints are bounded, checksum-verified
and charged to local quota. Empty checkpoints clear older incident state. Database identity, native epoch and
incident policy must match before a checkpoint can continue an incident. Connection aliases alone do not identify
the database. No Blob request is made while restoring an incident inside an open database observation.

Unfinished analytical deliveries hydrate incrementally after startup, within a local metadata budget. Native
archiving has a separate journal and I/O loop, so stalled warehouse readback or a full analytical journal cannot
prevent archival of new captures. Current discovery walks a bounded newest-first window independently of the
historical recovery cursor, refreshing on native commits and periodically. Each lane retains at most one verified
manifest under backpressure. Historical hydration leaves 64 MiB and 128 entries of the analytical journal's budget
available for current observations; exhausting the remaining budget backpressures current work too. Both recent
observations and older backlog receive delivery work. Verified
manifests are cached within bounded process-local limits; restarting reopens their authoritative recovery commits.

Native archive commits precede analytical dispatch. Successful upload alone does not publish a range: readback
must establish exact distinct row membership and every projected field, then the exact completion record. Verified
publication removes the range from the remote unfinished index; native history and captured-range receipts remain.
Cold recovery therefore follows unfinished work instead of replaying years of published history. The index is bounded
at 20,000 ranges per namespace; exhaustion stops new remote commits and eventually source admission through quota.
One native range is limited to 100,000 exported records and 1 GiB; background configuration rejects source budgets
that could exceed the record envelope. These are hard bounds, not measured throughput or memory guarantees.

Acknowledged batches and receipt pages survive restart. A lost acknowledgement can cause an idempotent retry.
Delivery failure does not cancel an unrelated source read. Shutdown settles native ownership and the current bounded
delivery transition, leaving the remaining backlog durable rather than waiting for the destination to recover.

Incomplete captures remain local for inspection, do not advance native coverage and consume bounded quota. They are
not silently promoted to complete warehouse data. Inspect repeated failures and retained captures before restarting
an exhausted source. Unconfirmed source work is never evicted to free space.

## Tables and analytical reads

The generated contract has 15 analytical tables and one completion table:

| Tables | Evidence |
|---|---|
| `CulpritQueryRuntimeV1`, `CulpritQueryWaitsV1` | Native intervals and source-declared dimensions; optional query/plan references, exact counts and explicit metric units |
| `CulpritQueryDefinitionsV1`, `CulpritPlanDefinitionsV1` | Versioned native definitions and bounded payload references |
| `CulpritResourceSamplesV1`, `CulpritWaitCountersV1` | Database/resource scope, aggregation, reset evidence and unknown values |
| `CulpritActivityV1`, `CulpritBlockingV1`, `CulpritActivityIncidentsV1` | Native session/request generations, wait owners and incident transitions |
| `CulpritDeadlocksV1` | Native event position, time, graph identity and recovery references |
| `CulpritOperationalV1`, `CulpritReplicationV1` | Typed operational facts, native counters and replication observations |
| `CulpritSettingsV1`, `CulpritTuningV1`, `CulpritObjectsV1` | Settings, tuning state and object/catalog facts with per-source coverage |
| `CulpritPublishedRangesV1` | Exact capture completion, batch membership, coverage and source-read metrics |

Incident checkpoints are recovery-only. They are not duplicated into a seventeenth analytical table. Multiple logical
datasets deliberately share a typed physical table; `Dataset`, native source and fact kind identify their meaning.

Every capture declares its subject through `CollectionTarget`: a database, or a native instance, pool, cluster or
node with an explicit namespace and identity. The connection used to read a subject does not establish its scope.
Analytical rows and completion records carry its hashed `ResourceId` and typed `ResourceKind`; private identity
details remain in recovery. These fields are included in exact readback verification. Native readers must establish
their own subject; the shared contract does not make an unsupported scope available in a provider.

Resource metrics preserve their own measured scope and capacity denominator separately. For example, a database's
memory consumption can be relative to a pool limit without describing the pool's consumption. The prepared
`Metrics` array adds `describes_subject` so shared context collected alongside database metrics cannot be charged to
that database. This flag establishes measurement scope, not a causal link to any query. Query ownership and cause
require their own native evidence; a high resource value or a concurrent query alone establishes neither.

Activity rows and incident rows retain `WaitResources`: source-local native identities observed with the waiting
request. SQL Server distinguishes in-memory page latches from page-I/O waits and preserves `wait_resource`.
MySQL and MariaDB preserve the requested native lock ID with its storage engine; the ID's internal format is never
parsed, and locked key contents are not collected. Live and background collection use the same native decoders.
These references are included in exact analytical readback. An empty list means that no targets were identified,
not that there was no contention.

These fields keep the existing blocked/running thresholds and polling cadence. Page-to-object/index enrichment
and identification of an owning statement require separate evidence. A wait target, a concurrent writer, or the
current/last statement of a blocking session does not by itself establish the statement that acquired a resource.

Structured JSON is stored as text so ingestion preserves nested timestamp strings, identifiers and exact counter
digits. ADX's generated analytical functions parse those fields after verification; ClickHouse retains verified
JSON text for explicit typed extraction. Optional arrays use `JSONExtractArrayRaw(ifNull(WaitResources, '[]'))` in
ClickHouse. Raw-table consumers can parse the JSON explicitly. Kusto `dynamic` ingestion can promote string values to other types; see the
[dynamic type documentation](https://learn.microsoft.com/en-us/kusto/query/scalar-data-types/dynamic).

Query the generated `...Rows` functions with explicit contract, store, resource and collection-time bounds. Apply
native measurement-time filters separately. Current activity and snapshot functions additionally require an
`asof` and positive maximum age. Complete empty observations replace prior sets; partial sources cannot clear them.
Native epoch changes replace the same logical activity source. Revisions use owner epoch and capture sequence,
not ingestion arrival order. Raw tables include unpublished and duplicated physical attempts and are not totals.

Runtime rows expose a `Metrics` array with a typed key, unit, average and optional distribution statistics.
Filter the duration metric and its unit before calculating execution-weighted time
(`sum(Executions * duration_average) / sum(Executions)`); convert units explicitly. Missing metrics stay absent.
Wait rows retain their native time unit and exact integer or real totals; their native category is an opaque string.
Definitions retain source-owned identity qualifiers; SQL Server's context, compile time, hashes and compatibility
level remain native facts rather than mandatory columns for every engine.

Cumulative query totals use `statement-counters` facts in `CulpritOperationalV1`, with query identity, observation
time, unit-bearing totals and explicit reset evidence. Their enclosing observation is current state, with no
invented native interval or plan. This contract is available to source adapters; it does not enable another built-in
provider. Preserve native units, unknown values, scope and reset evidence. Do not average averages or invent
deltas across an unknown reset. Full SQL/XML payloads stay in the private native recovery store; analytical rows carry bounded previews, checksums, redaction
versions and native references. Large fact detail also becomes an explicit recovery reference.

For Event Grid delivery, configure managed ingestion only for each generated `ready/<table>/` prefix in the **ready container**, with its
matching table and mapping. Apply the configured storage prefix before the generated route. Subscribe to neither
native payload/manifests nor projection indexes. Ready objects use new physical names on re-drive so an expired
notification can be retried without changing logical measurement identity. Cloud-route behavior remains a live gate.

## Identity, storage and probes

Source identities need source-specific read permissions; recovery identities need access to the chosen recovery
namespace; warehouse query identities need verification read access. Queued ADX delivery has its own ingestion
identity. Event Grid delivery instead has a staging identity with Blob access. No role needs SQL write permissions
or a cluster management role. Provisioning identities apply schemas and managed ingestion configuration separately.
For Event Grid, grant the ingestion service access to the ready container and keep the native archive private. The
runtime rejects using the same container for both.

The Kubernetes example projects its own direct-federation token with audience `api://AzureADTokenExchange`; its token
path belongs to this manifest rather than depending on the webhook's internal mount path. Token content is reread
when exchanging a refreshed assertion. Keep the workload identity pod label and configure the service account's
issuer/subject federation. See [Microsoft's workload identity documentation](https://learn.microsoft.com/en-us/azure/aks/workload-identity-overview).

Use one StatefulSet per shard, one replica, a qualified CSI filesystem and `ReadWriteOncePod`. Do not scale a shard's
replica count to add capacity. Retain the PVC on scale-down/deletion and avoid forced ownership replacement.
`ReadWriteOnce` alone can allow several pods on one node; [Kubernetes documents the distinction](https://kubernetes.io/docs/concepts/storage/persistent-volumes/#access-modes).
Network policy/firewalls must allow only the selected SQL transport, Blob and Kusto endpoints, Entra exchange and
DNS, plus the chosen monitoring path. The examples do not install a cluster-specific network policy.

Expose `/live`, `/ready` and `/metrics` only inside the monitoring network. `/live` is process liveness and stays
independent of database/ADX availability. `/ready` also considers initialization, failed/overdue expected work,
unconfirmed source shutdown and stale local health observations. Do not use readiness to force a replacement owner.
Metrics show pending/quarantined deliveries, oldest backlog age and clock skew, local quota, native debts and
per-schedule operations. Unknown completion age is absent. Schedule labels are SHA-256 of compact UTF-8 JSON
`[connection,schedule]`; metrics contain neither raw names nor SQL. An operation completing a historical backfill is
not proof that current database telemetry is fresh: use the archived native coverage and measurement times for that.

Source read metrics retain bounded template references, decoded rows/values, client elapsed time, failures and
cancellation outcomes. Client time is **not database CPU, logical reads or network bytes**. Native cost must be
measured independently in an owned representative workload before production collection is cleared.

## Offline packaging checks

```sh
cargo build --locked --release -p culprit-cli
python3 dev/licenses.py
python3 -B dev/smoke-container.py --binary target/release/culprit
```

Run this on a Linux builder whose glibc baseline is compatible with the Debian bookworm runtime. CI uses Ubuntu 22.04,
the same baseline as the Linux release, with an isolated compiler cache. The script refuses a macOS/Windows
executable before Docker, copies only the executable, licenses and Dockerfile into a private build context, and tests
the image with networking disabled, a non-root user,
read-only filesystem and dropped capabilities. Fabricated connection metadata exercises offline validation; no real
workspace is mounted. Owned test containers/images are removed; `--keep-image` retains the resulting local image.
CI performs the Linux container check. It does not publish an image or deploy the Kubernetes resources.
