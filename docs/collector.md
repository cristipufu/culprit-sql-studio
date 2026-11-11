# Background collection and analytical delivery

The collector archives database observations through the CLI, using the same provider readers as scans and live
views. Database access stays in the collector's network. An analytical destination receives prepared observations;
investigations do not need a new connection to the production database.

The initial remote adapter uses private Azure Blob recovery and an ADX ingestion-ready container. The actual managed
ingestion route, cloud permissions, private networking, retention-scale costs and production source-load budgets
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
culprit collect schema --format json > adx-assets.json
```

Validation reads saved metadata, checks unattended authentication and configures the adapters. It acquires no tokens,
opens no SQL/Blob/ADX connections and creates no runtime state. `collect schema` generates DDL, mappings, canonical
functions and per-table ingestion routes without opening a workspace or applying anything to Azure.

The example includes activity, Query Store history, resource history, deadlock history and all 22 point-observation
families. Each has its own cadence and budget. A family being scheduled does not establish source availability:
permissions, engine edition, enabled native features, retention and complete visibility still govern its coverage.
Unavailable and truncated sources must remain distinguishable from complete empty results.

Collection admits one native operation at a time per workspace and keeps a one-minute minimum gap between jobs on
the same configured physical resource. Relative lateness prioritizes frequent schedules without shortening that
guard; pressure extends the delay. Cadences are minimum requested periods, not guaranteed sampling rates. Inspect
missed-work metrics and qualify the combined workload before adding targets or shortening periods. The example's
activity threshold is 30 seconds for blocking and 60 seconds for running requests, with a three-minute poll period.
Transient activity between polls is not a complete event history.

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
prevent archival of new captures. Both recent observations and older backlog receive delivery work. Verified
manifests are cached within bounded process-local limits; restarting reopens their authoritative recovery commits.

Native archive commits precede ready-object dispatch. Successful upload alone does not publish a range: readback
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
| `CulpritQueryRuntimeV1`, `CulpritQueryWaitsV1` | Native interval, outcome and replica fragments; exact counts and documented units |
| `CulpritQueryDefinitionsV1`, `CulpritPlanDefinitionsV1` | Versioned native definitions and bounded payload references |
| `CulpritResourceSamplesV1`, `CulpritWaitCountersV1` | Database/resource scope, aggregation, reset evidence and unknown values |
| `CulpritActivityV1`, `CulpritBlockingV1`, `CulpritActivityIncidentsV1` | Native session/request generations, wait owners and incident transitions |
| `CulpritDeadlocksV1` | Native event position, time, graph identity and recovery references |
| `CulpritOperationalV1`, `CulpritReplicationV1` | Typed operational facts, native counters and replication observations |
| `CulpritSettingsV1`, `CulpritTuningV1`, `CulpritObjectsV1` | Settings, tuning state and object/catalog facts with per-source coverage |
| `CulpritPublishedRangesV1` | Exact capture completion, batch membership, coverage and source-read metrics |

Incident checkpoints are recovery-only. They are not duplicated into a seventeenth analytical table. Multiple logical
datasets deliberately share a typed physical table; `Dataset`, native source and fact kind identify their meaning.

Query the generated `...Rows` functions with explicit contract, store, database and collection-time bounds. Apply
native measurement-time filters separately. Current activity and snapshot functions additionally require an
`asof` and positive maximum age. Complete empty observations replace prior sets; partial sources cannot clear them.
Native epoch changes replace the same logical activity source. Revisions use owner epoch and capture sequence,
not ingestion arrival order. Raw tables include unpublished and duplicated physical attempts and are not totals.

Preserve execution-weighted statistics (`sum(Executions * AvgDurationUs) / sum(Executions)`), native units, unknown
values, scope and reset evidence. Do not average averages or invent deltas across an unknown reset. Full SQL/XML
payloads stay in the private native recovery store; analytical rows carry bounded previews, checksums, redaction
versions and native references. Large fact detail also becomes an explicit recovery reference.

Configure managed ingestion only for each generated `ready/<table>/` prefix in the **ready container**, with its
matching table and mapping. Apply the configured storage prefix before the generated route. Subscribe to neither
native payload/manifests nor projection indexes. Ready objects use new physical names on re-drive so an expired
notification can be retried without changing logical measurement identity. Cloud-route behavior remains a live gate.

## Identity, storage and probes

The runtime identity needs source-specific read permissions, native/ready container access and ADX verification
read access. It needs no SQL write permissions, cluster management role or permission to create its own collectors.
Provisioning identities apply schemas and managed ingestion configuration separately. Grant the ingestion service
access to the ready container, not the native container. The runtime rejects using the same container for both.

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

Run this on a Linux builder. The script refuses a macOS/Windows executable before Docker, copies only the executable,
licenses and Dockerfile into a private build context, and tests the image with networking disabled, a non-root user,
read-only filesystem and dropped capabilities. Fabricated connection metadata exercises offline validation; no real
workspace is mounted. Owned test containers/images are removed; `--keep-image` retains the resulting local image.
CI performs the Linux container check. It does not publish an image or deploy the Kubernetes resources.
