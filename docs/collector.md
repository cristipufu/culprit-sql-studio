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
| `Outbox` | Exclusive local ownership, capture revisions, private files, quota and diagnostic working sets |
| Shared diagnostic worker | Analyze sealed native inputs, retain episode lifecycle and analysis status, and commit separate diagnostic captures |
| `RecoveryStore` | Immutable native objects, fenced commits, historical coverage and unfinished delivery index |
| `TelemetrySink` | Destination dispatch, exact value/membership readback and completion publication |
| `DeliveryWorker` | Independent native archive and analytical loops, with separate durable progress journals |
| `CollectorService` | Stop admission and await both source and delivery owners during shutdown |
| Engine service configuration | Compose credentials, provider registry, recovery adapter, destination and probes |

Additional sources implement the existing provider/telemetry contracts. Additional recovery or analytical adapters
implement their respective interfaces; they do not acquire database connections. Interactive analysis still consumes
`Snapshot` through the existing pure analysis pipeline. Reading archived telemetry back into a Culprit report is a
separate consumer; this service does not add an ADX-backed desktop connection.

Embedders can read a specifically selected committed capture through
`culprit_collector::analytical::evidence::EvidenceReader`; the store crates expose `evidence::AdxReadStore` and
`evidence::ClickHouseReadStore`. The reader accepts an authenticated `CommittedRange`, selected evidence families and a read budget.
`EvidenceFamily::ALL` declares the supported families: resource samples, native activity observations, inline typed operational facts, query and
plan definitions, interval query runtime and query waits. Runtime and waits retain native identities, dimensions,
outcomes, exact execution counters, units and query/plan references. Definitions retain native context and plan-to-query
parents; an available SQL or plan payload is reconstructed only when the stored preview is complete and verified.
Truncated payloads require recovery. A native unavailable payload keeps its original reason.
Scope, reset evidence, source coverage and observation bounds remain attached to the typed records. The reader does
not select a latest head, resolve references across captures, merge epochs or create a report.
`is_read_complete()` confirms retrieval of the selected capture's evidence. Native source coverage remains
independent: a fully retrieved capture can still contain partial or unavailable native sources.

Reads need warehouse query access, without an ingestion identity, plus the independently configured recovery
identity for the committed manifest and immutable projection indexes. Reopening the manifest and checking those indexes does not reread every native
payload. Keep validation indexes for as long as these warehouse investigations are required. Missing publication,
rows or proof indexes remain explicit gaps; conflicting content is rejected. Large recovery-only details and native
timestamp precision absent from a warehouse projection require native recovery and cannot be reported as complete.
The initial read API reports those requirements without downloading the raw payload or connecting to the database.

With the engine's `collector` feature, `culprit_engine::collected_evidence::analyze` feeds one verified query-history
capture through the existing pure checks. It returns `CollectedQueryAnalysis`, with authenticated native inputs,
findings, unavailable assessments and explicit limitations. It does not open a database or manufacture a current
server snapshot. Runtime groups join only exact native query/plan references within that capture; missing runtime
rows withhold the workload denominator. Available statements can still be analyzed when unrelated definitions are
missing, using the complete captured runtime totals. Unsupported metrics remain in the native context.
Known monitoring and engine-internal statements are excluded from application totals as well as application
recommendations. Unresolved definitions keep their known runtime in the conservative denominator.

Sources can declare a bounded native wait vocabulary in the authenticated capture description. Both warehouses
preserve the same declaration; the analysis consumer never chooses a dictionary by engine or source name. Known
categories contribute observed totals, while unknown categories and inexact values remain explicit limitations.
Workers, CPU, compilation and memory waits are distinct classes. Native category IDs and names remain in
the retained context; a category does not identify an individual native wait type or a blocking query.

The current capture setting and historical interval qualification are independent facts. Query Store's current
`ON` setting does not prove that waits were collected throughout an earlier interval, and `OFF` does not erase
retained positive waits. The SQL Server adapter reports historical wait coverage as unverified. A zero for an absent
wait class requires independently qualified interval coverage, a complete selected wait family, exact query/plan
joins and a declaration that the source measures that class. Partial captures can still support positive findings.
Aggregate waits do not identify which executions were slow or rule out parameter-sensitive behavior.

This consumer does not combine captures, estimate database health from absent measurements or expose a warehouse
connection in the desktop app. Missing current metadata, schema and settings prevent executable fix SQL; the
finding's explanation and verification remain available. Analysis provenance identifies archived telemetry explicitly.

`EvidenceReadBudget` bounds rows, queries, proof bytes and caller wait. Its per-response and total byte limits cover
complete native query response bodies, including metadata and completion frames; HTTP headers, TLS and
authentication traffic are outside those byte budgets. Each request is also limited by the remaining body budget;
decoded row encodings respect the per-response ceiling, and proof objects have a separate ceiling.
Caller timeout ends the caller's wait; it does not acknowledge that a
warehouse query has stopped.

### Investigate a committed capture

`collect evidence` opens only the selected warehouse and recovery profiles. It neither opens a source database nor
initializes a namespace, acquires collection ownership, changes checkpoints or submits ingestion. The configuration
has no source inventory, workspace, delivery or staging fields. For example, `archive.json` can use ClickHouse query
access and an independent S3 proof reader:

```json
{
  "pipeline": "pilot",
  "shard": "shard-0",
  "credentials": {
    "warehouse-read": {"kind": "password-environment", "username": "culprit_reader", "variable": "CULPRIT_WAREHOUSE_PASSWORD"},
    "recovery-read": {"kind": "aws", "profile": "culprit-archive-reader"}
  },
  "recovery": {
    "provider": "s3",
    "options": {"bucket": "culprit-private-recovery", "region": "eu-west-1", "prefix": "pilot/shard-0", "expected_owner": "123456789012"},
    "credential": "recovery-read"
  },
  "warehouse": {
    "provider": "clickhouse",
    "options": {"endpoint": "https://warehouse.example:8443", "database": "culprit"},
    "credential": "warehouse-read"
  }
}
```

The pipeline, shard, destination and recovery prefix must identify the original committed namespace. Set
`CULPRIT_CAPTURE_RANGE` to its exact range ID, then select the required families or an analysis:

```sh
culprit collect evidence read --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --family operational-samples --json
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis queries --json
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis native-failures
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis request-memory --json
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis session-memory --json
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis page-waits --json
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis statement-work --json
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis task-space --json
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis runtime-distributions --json
```

`memory-failures` selects the retained SQL Server OOM analysis; `native-failures` selects native structured
diagnostics. Both call the scan's existing pure explanation functions, reference original native rows once, and
retain source qualifications separately from warehouse read gaps. Query analysis requests its four definition,
runtime and wait families. No mode invents current server metadata or query ownership.

`request-memory` explains native execution reservations from a database-target `current-request-memory` capture.
Waiting requests are affected participants; granted and used amounts are separate measurements. A matching current
statement or native resource ID does not establish which statement acquired a reservation or caused another wait.
Unassigned observations remain unassigned rather than becoming the selected database's demand.

`session-memory` explains tracked allocations from a shared-instance `session-memory` capture. Current allocations,
controlled allocations and session high-water marks remain distinct, with exact integer byte values. A separately
observed event, digest or database label is context, not the allocating statement or database ownership. Neither
mode sums unrelated amounts, derives a rate from one observation, constructs a synthetic database snapshot or
reconnects to the monitored server. Their report entries reference the original native facts by key and identity.

`page-waits` explains a database-target `page-wait-targets` capture through the scan's pure view. It distinguishes
memory-page contention from page I/O and references the original request and wait resource. Page mapping and
request rechecks are independent: checked current page metadata can remain after an original wait or request ends.
Before/after clocks distinguish a matching statement, a changed request and a wait that is no longer observed.
Statement links require matching statement checks and trusted source context; plan links require matching native
plans too. Unresolved context outside the database remains unowned. Equal page metadata does not establish
continuous physical identity, a latch holder or an exact causal share.

`statement-work` reads database-target `statement-counters` captures. It preserves native query identities, point
clocks and reset qualification, and shares the scan's temporary-work explanation. Typed native metrics distinguish
block traffic and recorded I/O timing from internal-table and sort-pass counts. Exact integer counters survive
readback; missing metrics remain unknown. A point does not establish an interval or safe delta, and matching a
query ID does not attach SQL from a separately captured definition. No mode reconnects to the monitored database.

`runtime-distributions` inspects a database-target `runtime-distributions` capture. Every distribution retains its
own native statement identity, observation clock, source, reset qualification and population. Native histogram
bounds and counts remain exact; an approximate percentile is not an individual execution time. Separate source
outcomes remain visible when a summary can be read but its histogram cannot. This mode does not construct a
synthetic workload, attach separately captured SQL or establish parameter-sensitive plan reuse.

`task-space` explains database-target task allocation/deallocation captures from `operational-samples`. It retains
exact page counters, native task tuples, request-check clocks and source coverage. The same pure view serves
scans and authenticated readback from either warehouse. Current checked SQL does not prove allocation ownership;
partial, unavailable and complete-empty sources stay distinct. The dataset is opt-in and is not added to a default
fleet schedule. See [SQL Server task observations](providers/mssql.md#task-level-temporary-space-observations) for
the current source support and read budgets.

For ADX, select warehouse provider `adx`, its query endpoint/database and an independently named bearer profile.
Azure recovery uses the existing recovery adapter and profile shape. Neither route needs an ingestion identity.
The warehouse configuration excludes insert policy, and changing an insert quorum does not change the destination
identity. Recovery credentials need access to committed manifests and immutable projection indexes; complete inline
readback does not reread raw archived payloads. Missing large or precision-sensitive details remain explicit gaps.

Rows, requests, response bytes, proof bytes and archive-read caller wait have separate caps. JSON output is staged
under `--max-output-bytes` before writing, so an exceeded cap emits no truncated JSON document. Text output keeps
source qualification and a short explanation. An incomplete warehouse read returns exit code 2 with its gaps;
native partial coverage can remain even when warehouse retrieval succeeds. Native transports retain their own
bounded lifecycle after caller cancellation. Pure analysis and output follow the archive-read deadline.

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
    "query": {"endpoint": "https://warehouse.example:8443", "database": "culprit"},
    "credential": "clickhouse",
    "write": {"insert_quorum": null}
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
never provisions or mutates schema. The schema contains 13 analytical landing tables and one publication table;
recovery-only diagnostic checkpoints have no analytical table. Landing tables sort by range, batch and ordinal for
bounded readback. A publication-table projection supports resource/time selection without duplicating every payload.

`--store clickhouse --replicated` emits `ReplicatedMergeTree` tables using server-configured replication paths/macros.
For that layout, configure a positive `target.write.insert_quorum` and a read/write endpoint with the required replica consistency.
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
activity analysis threshold is 30 seconds for blocking and 60 seconds for running requests, with a three-minute poll period.
These thresholds do not filter the native baseline before it is saved.
Transient activity between polls is not a complete event history.

The existing schedule journal persists due times, retry and pressure counters, fairness and resource/family
cooldowns. A reservation commits before a backend call can start; slow journal I/O cannot authorize a late start.
After an interrupted turn, its conservative reservation is restored without crediting downtime and rebased
durably before another admission. Completed schedules can credit valid elapsed wall time; a backward clock never
shortens their saved delays. Missing/corrupt scheduler rows and stale writers stop admission. This protects
restart behavior; it does not replace the existing native shutdown fence or implement automatic signal escalation.

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
| Azure SQL elastic pool or Managed Instance | `--kind pool` or `--kind instance` | `--resource resource.json --collection memory-failures` | Retained native OOM observations with verified shared identity; incomplete history, offset-free event times and no query owner |
| SQL Server or Managed Instance, through an existing database connection | `--kind instance` | `--resource instance.json --collection operational` | Memory, schedulers, performance/topology, compiler admission and tempdb context tied to the verified native instance |
| PostgreSQL instance, through a TCP database connection | `--kind instance` | `--resource instance.json --collection operational` | Shared archive, writer, WAL, I/O, checkpointer, available statistics-eviction counters and main shared-memory allocations; database counters stay on database captures |

Use the same discovery workflow for an instance, then capture its current state without historical bounds:

```sh
culprit collect resources shop-sqlserver --kind instance --json > resources.json
jq -e '.[0].identity' resources.json > instance.json
culprit collect once shop-sqlserver --resource ./instance.json --collection operational \
  --output ./instance-capture --json
```

SQL Server instance identities use native server/machine names in their observed network namespace; process identity
and startup ticks verify a capture, rather than becoming a fabricated permanent host ID. Azure SQL Database uses
the pool route for retained pool resource history. Generic current-state memory, schedulers, performance counters,
processor topology, compiler admission and tempdb capacity are not exposed through that pool route: their native
instance/host/resource-governor scope does not establish elastic-pool ownership. Database operational captures
exclude instance performance counters, processor topology, compiler
gateways and shared tempdb capacity. The shared resource is assigned once instead of reread for every database.

`memory-failures` is a separate opt-in point observation; it takes no historical `--from`/`--until` bounds and is not
part of the general operational profile. Managed Instance supports the instance route; boxed SQL Server does not
support this native DMV. The pool route requires an eligible writable member and rechecks its physical route and
pool membership. Native event time has no established UTC offset and is never fabricated from capture time.
Rows go through the existing `OperationalSamples` schema in both ADX and ClickHouse. Authenticated SDK readback
(`culprit_engine::collected_resources::memory_failures`) reuses the scan's pure explanations and retains native
rows once, their source qualification and warehouse read gaps. It creates no database snapshot or query owner.
See [the SQL Server source limits](providers/mssql.md#retained-out-of-memory-observations) before scheduling it.

Azure pool history uses `sys.dm_elastic_pool_resource_stats`, whose documented retention is approximately
40 minutes. It is distinct from the 14-day `sys.elastic_pool_resource_stats` history in `master`. The native
source requires `VIEW SERVER STATE`; Culprit does not grant permissions. Pool identity comes from the logical
server and pool metadata, while the member's logical/physical database identities anchor capture verification.
Membership is rechecked before each page. A pool move or changed member storage prevents complete publication.
The source does not prove a pool incarnation, so continuity remains unverified. See Microsoft's
[pool-history reference](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-elastic-pool-resource-stats-azure-sql-database?view=azuresqldb-current).

PostgreSQL uses its native system lineage and observed server address/port, independently of the connection's
database. This tuple is not a globally unique host identifier: physical replicas share lineage, and loopback or
overlapping networks can make endpoints ambiguous across deployments. Those topologies need separate identity
qualification before sharing an analytical namespace.
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

### Shared allocation and scheduler facts

The explicit instance `operational` route includes these observations where supported and readable:

| Source | Preserved facts | Attribution limit |
|---|---|---|
| SQL Server memory clerks | Native type/name/memory-node groups, exact clerk counts, separate page/virtual/shared/locked memory dimensions in KiB, allocation-granularity bounds in bytes | Component allocations on the engine instance; no query owner or per-database charge |
| SQL Server schedulers | Scheduler/node/CPU identities, visible/hidden and online/idle state, worker-creation state, runnable tasks, workers, work queues, pending I/O and available CPU/delay counters | Shared scheduling context; a queue or counter alone does not identify the responsible statement |
| PostgreSQL 13+ shared allocations | Native allocation name/offset, size and padded size in bytes, scoped to the observed postmaster epoch | Main shared-memory segment only; excludes dynamic shared memory and private backend/query memory |

Allocation sizes and current queue depths are gauges. Native cumulative counters retain unknown reset evidence;
an unknown unit or internal counter meaning cannot establish a rate. NULL and absent columns remain unknown, and
an unavailable clerk view never becomes healthy zero memory. Host CPU metadata is separate from SQL-visible
schedulers and does not establish instance CPU entitlement or a database quota. The PostgreSQL instance capture declares restricted
allocation metadata as unavailable while preserving its readable sibling sources. Effective role privileges are
checked; a role membership without effective privileges does not establish access.

SQL Server 2022+ requires `VIEW SERVER PERFORMANCE STATE` for these native views; earlier SQL Server uses
`VIEW SERVER STATE`. Azure SQL requirements vary by service objective and pool. PostgreSQL allocation access
requires the relevant `pg_read_all_stats` privileges and access to its view/function. Culprit changes no permissions.
See the native [memory-clerk](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-os-memory-clerks-transact-sql),
[scheduler](https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-objects/sys-dm-os-schedulers-transact-sql) and
[shared-allocation](https://www.postgresql.org/docs/current/view-pg-shmem-allocations.html) references.

Read limits bound returned rows, bytes, queries and time. Grouping clerks or selecting scheduler metadata still
visits native engine metadata; a small result is not proof of low source cost. Load qualification is required
before production scheduling. Scans can request these observations with
`culprit scan CONNECTION --collect-shared-resources`; the option defaults off until that qualification. With it off, these new readers send
no SQL and record that shared collection was not requested. These facts do not create pressure warnings solely
from size or fullness.

### Current execution-memory requests

`culprit scan CONNECTION --collect-request-memory` opts into the SQL Server family's current memory-grant
observations. The default scan does not run this reader. The collector profile requires an Azure SQL primary replica:
`culprit collect once CONNECTION --collection current-request-memory`. It writes typed operational facts through
the same native reader. Other providers report this profile as unsupported rather than returning empty evidence.

Facts distinguish waiting requests from granted reservations, retain exact KiB and worker counters, and associate
a request only when native generations, request times and handles agree. Missing associations remain unassigned
facts with partial coverage. Current SQL is request context; it does not prove which statement acquired a grant or
which holder caused another request's wait. Resource Governor pool/semaphore IDs are native resource identities,
not Azure elastic-pool identities. Optional columns and native NULLs remain explicit.

The bounded probe has no sort, aggregate or plan retrieval. Its query chain uses loop joins and one execution
thread to avoid diagnostic hash grants, following Microsoft's
[memory-grant troubleshooting guidance](https://learn.microsoft.com/en-us/troubleshoot/sql/database-engine/performance/troubleshoot-memory-grant-issues).
SQL detail uses the existing guarded activity reader;
identity checks and confirmed shutdown precede accepting the result. Scan limits are 200 rows, five queries,
three seconds per statement and ten seconds overall. These limits do not prove low server cost: qualify the
source under representative load before scheduling it. Native application, login and host labels follow the
existing activity collection policy; SQL literals are redacted. Snapshot sharing removes those client labels.

## Activity baselines and diagnostic episodes

`activity` captures a bounded metadata baseline before optional SQL or blocker investigation. It retains ordinary
actors and short waits within the provider's declared population; blocked/running thresholds belong to later
episode analysis. The baseline uses the existing native projection and decoder without a SQL payload read,
blocker closure, PostgreSQL lock-manager walk or YugabyteDB DocDB lock RPC. Native wait owners already present in
the metadata remain attached to the original activity row. An unavailable owner or statement stays unavailable.

Each description records native observation bounds, a point timestamp, population, measured scope, native row
count, retained row count and source coverage. Populations differ: PostgreSQL selects current-database client
backends; YugabyteDB selects local YSQL client backends and connection-manager workers; MySQL and MariaDB select
visible connections whose default database is the selected database; SQL Server selects active requests,
open-transaction context and blocker references under its native database predicate. These populations do not
include every system actor, parallel task or node. A connection's default database does not prove every statement
ran there. Open-transaction counts remain absent when the native source does not measure them.

A row cap can retain a positive prefix with partial coverage. Hidden rows, missing source authority and unknown
generations cannot become a complete empty observation. A fully read, sealed capture can still declare partial
native coverage; successful persistence establishes retrieval, not database health or absence. A missing native
epoch remains absent and prevents absence-based episode resolution or a focused lookup. Before/after source checks qualify
the frozen point observation; they do not prove continuous identity or atomicity between those bounds.

Baseline sources preserve stricter caller budgets and cap retained rows at 500, statement time at one second and
total native time at five seconds. Their native phase ceilings are seven for SQL Server, four for PostgreSQL,
twenty for MySQL/MariaDB and eight for YugabyteDB. Setup and prepared-statement phases count toward those limits;
the counts are not a claim about server work per SQL statement. These hard bounds do not establish production
overhead under representative load.

After the native source has settled and its capture is sealed, the same source-independent diagnostic worker runs
for fleet collection, the standalone monitor and offline recovery. It references original rows by capture, manifest,
stream and ordinal instead of copying native evidence into an inline incident stream. Episode lifecycle
(`active`, `uncertain`, `resolved`) is independent of analysis status (`pending`, `deferred`, `collecting`, `partial`,
`analyzed`, `unresolved`). Recovery can leave historical analysis pending. Failed or partial absence cannot clear
an episode. Absence-based recovery requires the matching native source, epoch, population and measurement scope;
an old epoch is not declared healthy when a different source generation is observed.

The selected provider's static catalog is read without constructing a provider, resolving credentials or opening a
connection. For a page-wait question from the current baseline, the worker records `deferred` with `cost-unqualified`
when its native recipe has no production cost qualification, or `unsupported` when no matching recipe is declared.
An offline worker with no selected catalog preserves pending analysis rather than guessing provider support.
Catalog ordering does not change the durable analysis identity; changed recipe versions or budget envelopes do.

A focused question can be reconstructed from an authenticated retained baseline after the episode recovers.
Its native source, population, epoch, actor, statement, wait resource and original observation bounds must match.
This readiness establishes a question to investigate, not a resource owner, causal explanation or permission to
execute another native read.

Diagnostic revisions are typed facts in `CulpritOperationalV1`; full working-set checkpoints stay in recovery.
Both commits consume outbox quota and retain the original input obligation until analysis commits atomically.
An exact archive acknowledgement compacts a completed diagnostic step to its replay digest and capture receipt.
Superseded steps and acknowledged input metadata retire through the existing archive/eviction owner only after
working-set, evidence, focus and native-debt obligations release them. Restored payload bytes and their quota charge
retire together. Active episodes and resolved episodes with deferred analysis keep their required references.

Cold input restoration checks the exact pending obligation in the configured recovery store and verifies the local
retirement generation before installation. An already acknowledged input cannot become new work after its local
metadata was removed. An older capture still pending in recovery remains admissible; capture age, native sequence
and owner epoch never substitute for that exact check. A checkpoint from another analysis-controller version cannot
acknowledge the current version's native obligation.

Qualified page-wait investigations share the existing fleet scheduler. A durable immutable question owns its original
evidence pin; repeated baselines cannot enqueue competing work for that episode/stage. Periodic collection takes
precedence. Focused work must fit its native timeout, shutdown and resource gap before another periodic deadline.
After restart, a fresh baseline must establish the source behind the saved route before a focused job can run.

Qualification requires an exact measured budget, reader version/revision and independently authenticated native-cost
artifact, interpreted by the selected provider's pure verifier. Client timing alone is insufficient. The reader
still rechecks native request, statement and resource identity before enrichment. Resolved incidents can retain
an owed investigation; the same observer preserves their original recovery proof when later evidence arrives.

The capture owner reports `not-started`, `quiescent` or `unconfirmed` separately from its result. Metadata-only retries
cannot clear older uncertainty. The scheduler retires a completed question after exact analysis consumption;
when recovery is configured, its archived result and analysis acknowledgement must also be verified. Cached evidence
does not count as analysis acknowledgement.

The owner writes admission lineage into the capture manifest before authentication or source description. An allocated
attempt retains its capture identity even when authentication, collection or storage fails; its manifest digest is
included when those exact bytes are available. Admission and attempt outcomes append immutable diagnostic revisions
through the existing outbox and recovery path. Checkpoints keep only the latest event digest. Failed native captures
remain incomplete and are not published as complete evidence; their diagnostic history can still be archived.
An unconfirmed shutdown keeps its source debt and scheduler obligation. The warehouse projection retains safe
identifiers, outcomes and recovery references while omitting private source targets, original SQL and qualification SQL.
`InvestigationContext` is included in analytical rows and `CulpritPublishedRangesV1`, including successful empty captures.

Built-in recipe costs remain unqualified, so this protocol does not enable automatic extra production reads.
Saving an episode or declaring a recipe does not establish a completed root-cause investigation or measured source
cost. Existing explicit page-target collection remains separately bounded.

## PostgreSQL source coverage

PostgreSQL uses the same capture, outbox, fleet and delivery path as SQL Server. Configure PostgreSQL connections
with unattended saved credentials, then schedule only the collections the provider supports. SQL Server's example
schedules must not be copied wholesale: PostgreSQL has cumulative counters rather than Query Store intervals.

| Selection | Native observations | Suggested starting cadence |
|---|---|---|
| `snapshot: statement-counters` | `pg_stat_statements(false)`: calls, duration, rows, available buffers, WAL, I/O timings, planning, JIT and runtime distributions | 5 minutes |
| `snapshot: operational` | Current-database health, conflicts, XID/MultiXact ages and subscription errors; shared counters use the explicit instance route above | 5 minutes |
| `activity` | Current-database client-backend metadata, native generations, state, transaction context and current wait names; episode analysis follows the sealed baseline | 3 minutes |
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
Native snapshots retain their explicit bounds and coverage; failed reads and identity changes cannot become complete
observations. Activity freezes and verifies its metadata before paging, preserving a capped positive prefix with
partial coverage. Optional lock resources, blocker closure and SQL are excluded from this baseline.

Counter values are current native totals or gauges, not fabricated time buckets. Per-entry reset timestamps are used
when PostgreSQL exposes them; a global reset does not prove each entry's lifetime. Unknown resets cannot establish
rates, and minimum/maximum statistics retain their separate reset epoch. Disabled I/O or planning instrumentation
may produce native zero totals; consult the captured settings before interpreting those totals as observed work.
Server-scoped observations must not be summed as if each database owns them. Settings describe the collector's effective
session, including its safety overrides; they do not reveal every application's settings or a pending configured value.

Full statistics visibility is required for complete activity and restricted native sources. Planner statistics also
need visibility of the selected relations. Metadata-only activity capture supports PostgreSQL 12+; native query IDs
are available from PostgreSQL 14 when exposed by the server. A wait name does not establish a measured wait duration
or a blocker. Other version-dependent fields are detected explicitly. Native tests run against owned fixtures;
optional package setup can upgrade PostgreSQL's base-image minor version, and the runner logs the actual restarted
server version. Managed-service permissions, TimescaleDB-specific storage and production source cost require their
own qualification.

Cleanup observations retain session, prepared-transaction and replication-slot identities, independent data and
catalog horizons, and source visibility. Qualification covers data and non-shared catalogs in the selected database;
shared catalogs remain unqualified for database-only captures, including on a primary. A standalone qualification
record survives empty holder sets. Session generation is distinct from transaction start; current/previous
query context does not identify the code that opened a transaction. Tied or partially observed horizons remain
multiple possible retainers, without a PID-only termination action or a guessed prepared-transaction name.
Standby session statistics do not expose every recovery horizon, so neither data nor catalog boundaries are qualified
there. The scan and collector consume the same native observations.

PostgreSQL supplies a deadlock counter, not retained SQL-accessible deadlock graphs. The counter is included in
operational observations. Graphs still require server logs, which the app's existing importer can parse; a scheduled
PostgreSQL `deadlock-history` collection is explicitly unsupported. Query/runtime interval history, historical wait
events, CPU utilization, application parameters and executed plans are not invented from current statistics.

## Other provider source coverage

All providers use the same capture, outbox, recovery, diagnostic and delivery components. Native readers own SQL,
permissions, cancellation and measurement meaning; the scheduler and ADX adapter do not select database-specific
queries. Unsupported selections fail explicitly rather than returning a complete empty observation.

| Provider | Point observations | Activity and retained evidence |
|---|---|---|
| MySQL / MariaDB | Statement counters and definitions; operational, settings, tables, indexes, columns, foreign keys, check constraints, objects, triggers, partitions, statistics, replication, sequences and cleanup | Default-database connection metadata baseline; InnoDB's latest deadlock report, with limited retention |
| YugabyteDB | Node-local statement counters and definitions; operational, settings, tables, indexes, columns, foreign keys, constraints, objects, triggers, partitions, statistics and retained plans | Local YSQL client/connection-manager metadata baseline; sampled ASH and retained terminations are separate operational observations |
| SQLite | Settings, operational file metadata, tables, indexes, columns, foreign keys, objects, triggers, statistics and sequences | No server sessions, native statement counters or retained deadlock history |

Keep the three-minute activity and five-minute health cadences as conservative starting policies, subject to the
shared resource gap. Catalogs and definitions belong in slower profiles. These are policy defaults, not measured
production overhead guarantees. Native fixture tests exercise capture, cancellation, restart/replay and ADX
projection; production load qualification and the live ADX round trip remain separate.

MySQL and MariaDB retain exact picosecond counters. Digest entry eviction and independent resets remain unknown
unless native evidence establishes them. Disabled instrumentation, insufficient visibility and digest overflow
prevent complete coverage. Column-statistics histogram export contains metadata, never histogram values. The
deadlock source can miss overwritten reports between polls; repeated reads do not create new historical events.

MySQL's separate `runtime-distributions` selection reads existing digest-summary quantiles and duration histograms
through the scan's native projection and decoder. Its fixed ceilings are 128 selected digests, 4,096 native bins,
256 exported distribution points, 24 owned query phases (including setup, prepare and execute), two seconds per
statement and ten seconds overall, with stricter caller limits preserved. SQL text is not fetched and application
work is not executed. Summary and histogram clocks come from their own native reads, and their counters remain
independently resettable. The bounded subset
is neither a complete workload nor a top-query inventory. Unsupported native sources remain unavailable.
These row/time/byte limits do not qualify native server cost under representative production load.

Yugabyte exports node scope, not fabricated cluster totals or PostgreSQL heap/buffer statistics. Native fan-out
is capped before expensive distributed lookups. ASH is sampled task activity, not CPU time. QPM is an independently
evictable plan cache; its averages are not converted into exact duration totals, and it requires effective
`pg_read_all_stats` access because the verified server's foreign-role redaction path is unsafe. CPU and memory
observations describe shared hosts/TServers. Optional native features can remain unavailable.

Activity retains positive native facts separately from missing ownership or optional detail. Later episode analysis
preserves the last verified evidence when a condition cannot be measured. Partial absence becomes `uncertain`,
without a fabricated fresh observation or recovery. Revisions describe evidence and lifecycle; notification
throttling is separate.

SQLite opens files read-only with trusted schema disabled. Its background metadata read has a 250 ms ceiling,
releases its transaction and closes the file before destination paging, and validates file identity through
completion. It does not run `dbstat`, execute application statements or export schema/default bodies. Framework
query logs remain separate app-supplied evidence, not native telemetry history.

Timescale uses the PostgreSQL reader and native counter semantics. Hypertable/chunk-specific size aggregation is
not part of background telemetry; base relation observations must not be interpreted as logical hypertable totals.
SQL Server profile availability and authority remain native-source decisions. Local boxed-server fixture coverage
does not qualify Azure topology, managed permissions or production overhead.

The current export format is **17**. New contracts require new collector/recovery state; there is no migration of
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

This mode opens no database connection and does not resolve source credentials. It restores and analyzes sealed native inputs,
then continues archive and analytical delivery. A diagnostic-only allocation epoch permits derived captures while
native allocation stays disabled; it never clears or bypasses native debt. The mode cannot be combined with
`--source-quiescent` or `--max-work`. Keep the original destination contract and recovery namespace. Source schedules
are explicitly disabled in health metrics.

Startup first acquires recovery ownership, restores the full verified diagnostic working set and restores the exact
original native inputs still owed analysis. Those obligations are independent of unfinished warehouse publication;
an already analyzed input is not requeued merely because its delivery is delayed. Restoration uses bounded pages,
checksums, cursor checks and local quota, preserving the capture-sequence high-water mark. It runs before source
credential binding or database sessions. The diagnostic-only epoch then permits bounded, cancellable batches of
offline analysis. Only a later, separately admitted native activation can resolve source credentials and promote
the same epoch without resetting its sequence.

Restored aliases and endpoint/database display labels do not authorize a native route. Original target, source,
epoch, generations and analysis policy remain evidence. A delayed focused result requires its exact authenticated
baseline parent; missing local evidence may be hydrated from the configured recovery store without a source
connection, while corrupt local evidence is rejected. Failed restoration cannot silently create an empty working
set or clear earlier episodes.

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

The generated contract has 13 analytical tables and one completion table:

| Tables | Evidence |
|---|---|
| `CulpritQueryRuntimeV1`, `CulpritQueryWaitsV1` | Native intervals and source-declared dimensions; optional query/plan references, exact counts and explicit metric units |
| `CulpritQueryDefinitionsV1`, `CulpritPlanDefinitionsV1` | Versioned native definitions and bounded payload references |
| `CulpritResourceSamplesV1`, `CulpritWaitCountersV1` | Database/resource scope, aggregation, reset evidence and unknown values |
| `CulpritActivityV1` | Native session/request generations, statement identities, wait targets and available wait-owner references in one authenticated activity row |
| `CulpritDeadlocksV1` | Native event position, time, graph identity and recovery references |
| `CulpritOperationalV1`, `CulpritReplicationV1` | Typed operational facts, including diagnostic revisions that reference original native evidence; native counters and replication observations |
| `CulpritSettingsV1`, `CulpritTuningV1`, `CulpritObjectsV1` | Settings, tuning state and object/catalog facts with per-source coverage |
| `CulpritPublishedRangesV1` | Exact capture completion, batch membership, coverage and source-read metrics |

Diagnostic working-set checkpoints are recovery-only. They have no analytical table. Multiple logical
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

Activity rows retain `WaitResources`: source-local native identities observed with the waiting
request. SQL Server distinguishes in-memory page latches from page-I/O waits and preserves `wait_resource`.
MySQL and MariaDB preserve the requested native lock ID with its storage engine; the ID's internal format is never
parsed, and locked key contents are not collected. Live and background collection use the same native decoders.
These references are included in exact analytical readback. An empty list means that no targets were identified,
not that there was no contention.

These fields do not apply blocked/running thresholds before baseline persistence. Page-to-object/index enrichment
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
not ingestion arrival order. These warehouse head rules do not resolve diagnostic episodes from a different native
epoch or population. Raw tables include unpublished and duplicated physical attempts and are not totals.

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
