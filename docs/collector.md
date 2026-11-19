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
`EvidenceFamily::ALL` declares the supported families: resource samples, native cumulative wait-counter samples, native activity observations, inline typed operational facts, object, query and
plan definitions, interval query runtime and query waits. Runtime and waits retain native identities, dimensions,
outcomes, exact execution counters, units and query/plan references. Definitions retain native context and plan-to-query
parents; an available SQL or plan payload is reconstructed only when the stored preview is complete and verified.
Truncated payloads require recovery. A native unavailable payload keeps its original reason.
Resource samples retain the native data-I/O accounting basis separately from the percentage, scope and limit.
Hyperscale local SSD accounting includes RBPEX and tempdb and excludes page-server I/O; unknown pool accounting
cannot borrow a member database's profile. Read native waits with `--family wait-counter-samples`: original wait
names, exact counters, reset evidence and typed log-governance reasons remain historical facts, without naming a
current limiting component or request.
Scope, reset evidence, source coverage and observation bounds remain attached to the typed records. The reader does
not select a latest head, resolve references across captures, merge epochs or create a report.
`is_read_complete()` confirms retrieval of the selected capture's evidence. Native source coverage remains
independent: a fully retrieved capture can still contain partial or unavailable native sources.

Acquisition integrity and statement-reference resolution are separate. A sealed, rechecked capture may contain
original runtime/wait rows whose query or plan definition could not be resolved. They retain no semantic `RecordId`;
physical receipts bind their exact batch, one-based ordinal and original payload digest. Such captures can finish,
advance collection and retire normally after verified delivery. Every supplied definition ID still has to resolve,
and incomplete source reads or unconfirmed shutdown cannot take this route. Informational readback preserves these
facts and uncertainty without granting trusted query totals or diagnostic escalation.

An explicitly configured MCP host can also read original local captures through `LocalCaptureReader` and the
engine's optional `OutboxCaptureReader` adapter. `culprit mcp --evidence-outbox DIRECTORY` installs that capability;
`analyze_capture` accepts a delegated connection, its exact original observation ID and the closed analysis choice
`page_server_reads`, `memory_clerks`, `hyperscale_log_components`, `performance_counters`, `database_properties`,
`instance_resources`, `compilation`, `configured_log_rates`, `connection_outcomes`, `jobs`, `request_memory`, `task_space`,
`current_request_work`, `statement_work`, `maintenance_progress`, `completed_spills`, `storage`, `blocking` or `session_census`. The existing
Outbox owner checks the journal association, Ready state, confirmed native shutdown, manifest and every artifact
in one canonical verification pass before projecting the requested analysis. Page-server reads keep runtime rows
and independently matched query definitions. Memory clerks retain all original Operational rows and source
coverage, including unrelated sources, in a captured raw carrier. Their shared-instance scope and native read
bounds establish no correlation to the current database scan or any query.
The other Operational reports use the same original owner and family-specific pure evidence rules. A missing
declared family is refused; an unavailable native source retains its unavailable state despite a complete local
file read. Query-global compilation context remains separate from physical acquisition ownership. Details return
at most five entries with separate original and presented counts. Optional SQL companions are not opened implicitly.
Grant and task reports include exact native measurements and generation-checked request context. A current
statement is not the statement that acquired a reservation or allocated temporary pages; unresolved and changed
relationships withhold its SQL. Focused work keeps the original participant's client labels and clock separate from
the later native point. SQL previews are bounded and redacted. Database-reported client names do not establish a
business-service or source-code owner.
It performs no native, recovery or warehouse read. The original native window, optional population membership,
collector clocks and actual local verification bytes remain distinct in the result.

Blocking analysis consumes the selected focused Activity closure and its exact original Activity capture under
one worker and cumulative row, byte and absolute deadline allowance. Each input passes the same canonical
journal/manifest/body verifier once. The parent comes only from the authenticated focused receipt, never a latest
lookup. Missing, changed or unsettled parents fail the analysis; native source gaps and unverified original scope
remain explicit after a complete local read. Presented chains, nodes and relationships are bounded independently
of original counts, with no live administrative action attached to historical identities.

`culprit scan CONNECTION --evidence-outbox DIRECTORY --memory-clerks-capture OBSERVATION` explicitly attaches
one verified clerk capture to the normal report after confirmed native scan shutdown. Existing captured or live
clerk input is refused rather than overwritten. Both original capture association and raw rows survive saved-report
reanalysis; ordinary scans neither discover an outbox nor acquire clerks.

`culprit scan CONNECTION --evidence-outbox DIRECTORY --hyperscale-log-components-capture OBSERVATION` attaches
one verified component capture through that same reader and scan owner. Choose exactly one capture attachment.
The original database incarnation must positively match the scan; the original member, clocks, rows and receipts
remain unchanged. Saved reports replay the evidence without retaining an outbox connection. Sharing withholds
the complete carrier and records an explicit gap.

Selected-database properties use the existing database-settings catalog read during a scan. Selected database
Operational captures retain the same finite point in their first-clock dispatch; other clock and authority reads
remain independent. `culprit collect evidence analyze --config FILE --range SHA256 --analysis database-properties`
replays that original Operational evidence without reconnecting to the source. Health folds the same shared
interpretation under **Selected database properties**. Native codes, descriptions, connected updateability,
catalog read-only flag and unknown fields remain separate. Query Store enablement is not its actual capture mode;
no property alone establishes a problem or authorizes a configuration change. Sharing deliberately withholds this
carrier and records an explicit evidence gap.

`CompletedCaptureBudget` applies to both local snapshot and query-history reads: 10,000 retained rows, 8 MiB of
all verified artifact/manifest bytes and five seconds by default, including queueing. Limits refuse rather than
truncate; unselected artifact bytes still count. Cancellation joins any started worker before releasing ownership.
The MCP response presents at most five entries, with matched redacted SQL previews capped at 512 characters and
the complete compact JSON capped at 64 KiB. An active external owner is refused; an embedding host can inject its
existing `OutboxDelivery` instead. Local native-reference positions start at zero; analytical storage ordinals
start at one, with conversion confined to the shared adapter.

Reads need warehouse query access, without an ingestion identity, plus the independently configured recovery
identity for the committed manifest and immutable projection indexes. Reopening the manifest and checking those indexes does not reread every native
payload. Keep validation indexes for as long as these warehouse investigations are required. Missing publication,
rows or proof indexes remain explicit gaps; conflicting content is rejected. By default, incomplete 1 KiB SQL/plan
previews, larger fact details and native representation precision absent from the warehouse remain explicit recovery
gaps. The reader does not download native payload bodies unless `EvidenceReadBudget.recovery_payloads` is set and
the matching recovery store is supplied. That opt-in can reconstruct only verified preview and native-representation
gaps. It cannot repair missing warehouse rows, publication or proof indexes, nor replace unavailable native data.

`RecoveryPayloadBudget` limits the complete read to 8 native objects and 64 MiB by default; accepted limits are
1–128 objects and 1 byte–256 MiB. Each admitted object is a complete native batch, so its whole byte length counts even
when only one row needs reconstruction. All required batches must fit before any payload body is opened; otherwise
the read fails with a capacity error. Payload bytes, EOF, native row identities and original receipts are verified before reconstructed records
are returned. Recovery objects and bytes are reported alongside warehouse queries, responses and proof bytes.
The existing caller deadline and cancellation own these read-only bodies; no source connection or archive writer
is opened.

With the engine's `collector` feature, `culprit_engine::collected_evidence::analyze(&capture, population)` feeds one
verified query-history capture through the existing pure checks. `None` returns independent analyses for every
source-declared execution group; `Some(&NativeIdentity)` selects one exact captured group. It returns
`CollectedQueryAnalysis`: one original authenticated evidence `context`, the `requested_population`, and separate
`analyses` containing their own population, query inputs, workload, findings, assessments and limitations. Their
runtime totals are never pooled. Unknown execution-group coverage stays unknown, with no comparable workload or
denominator; it does not become an unpartitioned history. The captured source catalog distinguishes positively
unpartitioned history from known groups. Analysis does not open a database or manufacture a current server snapshot.

The SDK's retained-analysis limit is 256 MiB of compact JSON for the complete output, including the single original
evidence context and every independent result. It measures serialized size without allocating that JSON output and
rejects an oversized analysis with a capacity error, returning no partial analysis. Select one execution group or
read a smaller capture to stay within the limit. This ceiling is separate from the caller's archive read budget and
the CLI's `--max-output-bytes` output limit; increasing the CLI limit does not raise the SDK ceiling.

Sources can declare a plan-independent native cohort for comparable runtime fragments. Analysis
groups only the same exact query reference and declared cohort within one source interval; native execution
outcomes, replica identities, contexts and generations remain separate. Without that declaration, exact plan
references and all native qualifiers remain part of the grouping. Shared code never removes provider qualifiers
to infer comparability. Every input measurement retains its original identity and references.

Plans within a declared cohort retain execution-weighted costs and their own bounds. Missing extrema, CPU or
compiled context remain unknown; fragment standard deviations are not pooled without sufficient statistics.
Recorded variation can justify investigating inputs, estimates, waits and resource demand. It does not prove
parameter sniffing, current plan use, overlapping plan lifetimes or a safe plan to force. Unobserved forcing status
is unknown rather than false. Waits join only the exact plan population with positive runtime in that cohort.
Missing runtime rows withhold the workload denominator. Within each selected execution group, unresolved original
runtime query/plan references also withhold its denominator; another group's unresolved references do not erase
that group's independently qualified totals. Native counters
remain available in the original context and focused explanations. When the original references closed but some
definition bodies were not retrieved, known runtime stays in the conservative denominator rather than overstating
the share of the remaining statements. Unsupported metrics remain in the native context. Proven monitoring and
engine-internal statements are excluded from application totals and recommendations.

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
connection in the desktop app. Missing current metadata, schema and settings prevent executable fix SQL in both
findings and nested plan insights, after the normal analysis pipeline has annotated the plans. The explanation,
verification and original native evidence remain available. Analysis provenance identifies archived telemetry explicitly.

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
  --analysis queries --workload-population "$CULPRIT_EXECUTION_GROUP" --json
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis native-failures
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis jobs --json
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis connection-outcomes --json
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
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis compilation --json
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis compilation --definitions-range "$CULPRIT_DEFINITIONS_RANGE" --json
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis completed-spills --definitions-range "$CULPRIT_DEFINITIONS_RANGE" --json
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis maintenance --with-recovery-payloads --json
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis cleanup --json
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis cached-plans --json
culprit collect evidence read --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --family tuning-observations --json
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis tuning --json
```

`memory-failures` selects the retained SQL Server OOM analysis; `native-failures` selects native structured
diagnostics. Both call the scan's existing pure explanation functions, reference original native rows once, and
retain source qualifications separately from warehouse read gaps. Query analysis requests its four definition,
runtime and wait families. No mode invents current server metadata or query ownership.

Query analysis shows separate execution-group sections by default. Its JSON retains the native choices at
`context.source.observation.data.population_catalog`, with all original rows and proof context once. Set
`CULPRIT_EXECUTION_GROUP` to an exact ID from that captured catalog to select one group with
`--workload-population`. The option applies only to query analysis. An absent ID is rejected; if several captured
identities share that ID with different qualifiers, use the SDK with the full native identity. Neither native labels,
role history nor the connected compute member selects a group. Selection happens after the same verified
whole-capture archive read, without another source read or a change to recovery-payload opt-in and proof checks.

`request-memory` explains native execution reservations from a database-target `current-request-memory` capture.
Waiting requests are affected participants; granted and used amounts are separate measurements. A matching current
statement or native resource ID does not establish which statement acquired a reservation or caused another wait.
Unassigned observations remain unassigned rather than becoming the selected database's demand.
The same bounded read retains native queue position and other generation-checked reservations on the same resource.
Those entries describe the observed database subset; they do not establish available pool capacity, complete queue
membership or which reservation caused a wait. Native queue positions from different queues are not comparable.

`session-memory` explains tracked allocations from a shared-instance `session-memory` capture. Current allocations,
controlled allocations and session high-water marks remain distinct, with exact integer byte values. A separately
observed event, digest or database label is context, not the allocating statement or database ownership. Neither
mode sums unrelated amounts, derives a rate from one observation, constructs a synthetic database snapshot or
reconnects to the monitored server. Their report entries reference the original native facts by key and identity.

`page-waits` explains database or explicitly assigned shared-resource `page-wait-targets` captures, including
focused captures, through the scan's pure view. It distinguishes
memory-page contention from page I/O and references the original request and wait resource. Page mapping and
request rechecks are independent: checked current page metadata can remain after an original wait or request ends.
Before/after clocks distinguish a matching statement, a changed request and a wait that is no longer observed.
Statement links require matching statement checks and trusted source context; plan links require matching native
plans too. Unresolved context outside the database remains unowned. Equal page metadata does not establish
continuous physical identity, a latch holder or an exact causal share. Parallel workers keep separate capture-local
task references, native waits and raw blocking fields while retaining their coordinator's independent context.
An unverified task population leaves coordinator rows useful and marks coverage partial. A focused activity
follow-up has no original task identity and therefore cannot substitute a parallel worker for its coordinator.
Verified SQL Server headers distinguish PFS, GAM, SGAM and IAM maps from heap and index pages. Their next steps
address the observed allocation mechanism; page-I/O waits retain storage guidance. These classes do not establish
which writer caused contention or qualify an application-database reader to inspect shared tempdb.

For a focused page capture or paired native task capture, supply its declared original Activity range:

```sh
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis page-waits --activity-range "$CULPRIT_ACTIVITY_RANGE" --json
```

Both captures share one cumulative read budget and deadline. The original capture, manifest, source scope, member,
role and native generations must match the declared parent. A focused capture retains its original wait and
historical blocking-session context; all matching requests on a reported blocking session remain separate.

A paired task capture can retain earlier statement and client context only for the exact request and initial
statement token in a complete authenticated original frame. That context keeps its earlier observation time;
the later task keeps its own wait, resource and counters. Earlier blockers or wait resources never transfer to the
task. Negative session-generation evidence withdraws the affected earlier context while retaining task facts.
Missing or incomplete original bodies remain unavailable, and later SQL cannot replace them. This context does
not establish the current statement, latch acquisition, a current holder or the query that wrote the page.

`page-wait-writers` analyzes a separately captured investigation of at most eight original requests. The native
reader rechecks the waiter, mapped target and request generations, then binds selected write operators to the
original object or index. A planned write makes that statement a candidate contributor; it does not establish
physical-page access or latch ownership. Read-only and unrelated plans remain distinct. Changed or missing
requests never inherit another request's SQL. This profile adds no ordinary scan queries and remains
cost-unqualified. Its automatic stage requires authenticated original Activity/page receipts and a measured
source-cost approval. Preview-only bodies stay unavailable until explicit bounded recovery hydration.

```sh
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis page-wait-writers --with-recovery-payloads --json
```

`page-task-holders` explains a separately captured SQL Server task-holder investigation through the same SDK:

```sh
culprit collect evidence analyze --config archive.json --range "$CULPRIT_HOLDER_RANGE" \
  --analysis page-task-holders --json
```

The first and final native task/request points keep their own timestamps, generations and SQL availability. A final
page-mapping check keeps its own timestamp and remains separate from the original mapping and both holder points.
SQL Server can append latch diagnostics to its `database:file:page` resource text. The shared native parser extracts
the page coordinates while retaining the complete original text. Changing diagnostic flags does not change the
physical page; a `Task` value inside that text does not establish a resource owner.
Matching endpoints do not establish continuous ownership, latch acquisition or the statement that wrote the page.
Task addresses alone do not identify a request generation. Text limits displayed entries and SQL excerpts; JSON
retains the typed report and original archive context within `--max-output-bytes`. Missing native or warehouse
evidence stays unavailable. Incomplete SQL previews require the same explicit, bounded `--with-recovery-payloads`
option; recovery cannot replace missing warehouse rows. This command opens no source database.

The SDK's `PageWaitTasks` selection binds a page-task population to an exact earlier Activity capture before native
work starts. The immutable request and manifest retain that association; outbox and recovery retain both original
receipts. A follow-up answers only that task's point question and does not resolve its coordinator's incident.
Ordinary unpaired page captures remain raw evidence. The existing investigation queue can select a paired population
from a recorded long-running incident's first authenticated Activity capture when the source catalog has no narrower
current-request reader. Sibling incidents from that frame share one population stage; later baselines do not replace
its original context. Admission still requires the exact route, original native member/process/role, independent
source-cost approval and caller budgets. The complete eligible original request roster is authenticated before
reservation. Current rows must match its database, native session/request generations and statement tokens before
using page-enrichment slots or reading page headers, SQL and index context. New or recycled requests are excluded;
a roster exceeding the existing request-size limit is refused without trimming it. The population and holder
recipes remain cost-unqualified and add no default production reads. Missing task rows leave the original incident
unexplained; they do not establish recovery.

The SQL Server shared-tempdb reader separately verifies the native instance, process, tempdb and selected files.
The affected request keeps its application database; the mapped page keeps its shared-resource owner. Scans reuse
that reader when both page-target and shared-resource collection are selected. One population supplies at most eight
targeted page addresses, within the existing read envelope. Shared source names remain private to authenticated
recovery metadata; warehouse facts retain opaque owner references. Automatic escalation from a database episode
requires its authenticated original participant, a separately captured shared owner and production-cost qualification.
The captured waiter population remains partial; an empty result cannot establish that the whole instance had no
tempdb contention.

On a qualified boxed SQL Server, PFS/GAM/SGAM maps in tempdb's primary data file can use an explicit process-lifetime
identity when the runtime file GUID and creation LSN are unavailable. The reader verifies the same native member,
process, connection and tempdb generation, with online data-file observations through the final check. This is a
bounded inference from the primary file's non-removability and tempdb's startup lifetime; it preserves missing file
GUID/LSN values. Application-database files, secondary tempdb files and ordinary object pages retain the GUID guard.
It establishes neither continuous page allocation nor a latch holder or cause. Managed variants and production
overhead need separate qualification. See Microsoft's [primary-file rules](https://learn.microsoft.com/en-us/sql/relational-databases/system-catalog-views/sys-master-files-transact-sql?view=sql-server-ver17),
[file removal restriction](https://learn.microsoft.com/en-us/sql/relational-databases/databases/database-properties-files-page?view=sql-server-ver17)
and [tempdb lifecycle](https://learn.microsoft.com/en-us/sql/relational-databases/databases/tempdb-database?view=sql-server-ver17).

`cached-plans` analyzes `cached-definitions` or `retained-plans` captures from `object-definitions`. It reuses the native
plan parsers and existing plan insights. SQL Server statement selection requires one matching native hash, or one
unambiguous statement when the hash is unavailable. Original module metadata can establish a cached or retained TVP input;
another capture's module header cannot supply missing historical metadata. Estimated rows, actual rows and cache
execution counters remain separate. Yugabyte node-cache counters retain node scope and use the Yugabyte parser.
Stored SQL Server plans mask database names, so named relations in JOINs remain unclassified without retained proof
of their original bindings. Direct and all-variable module proofs remain available. A TVP classification does not
suppress an independent native Cartesian-join warning.
When supported, the same bounded payload read also retains current table-type columns, native byte lengths,
collations and index keys. This catalog context has its own collection clock and current object/type identifiers;
it is separate from the original parameter declaration and does not establish the schema used at compilation.
Missing visibility, changed associations and collection limits remain explicit. Culprit reads no live parameter
contents, defaults or application rows. The plan details and collected SDK/CLI/MCP output preserve this distinction.
Types without indexes retain an explicit empty index list; omitted or malformed catalog collections are rejected.
Bounded built-in aggregate/scalar expressions and grouping preserve this binding, including `COUNT_BIG` over a
filtered input. One closed parameter passthrough, `(SELECT * FROM @Rows AS r) AS d`, can retain the original physical
input alias `r`; native objects and every alias still require exact independent binding. Other nested queries,
changed inner projections or populations, unknown functions and ambiguous lexical modes remain unresolved.
For a qualified TVP, a complete serial native table scan can distinguish consumed input, rows kept by its filter and
statement output. Every aggregate, compute or sort ancestor needs its own exact native serial-execution proof.
Missing end-of-scan counters, repeated or parallel execution, seeks and early-stop paths retain
access/output context. Input volume alone does not establish harmful reuse or the cause of a spill.
The shared loop diagnosis can link an underestimated TVP access to material repeated inner reads in the same
selected actual plan. It requires original TVP binding and explicit serial counter populations; one observed
worker is insufficient. Healthy single-row seeks and unsupported or missing evidence receive no TVP loop verdict.
It does not recommend statistics on a TVP or assert that a different join would be faster.
A completely observed TVP overestimate can also narrow an oversized execution-grant investigation when it is
on the same memory-consuming branch. The grant and consumed input remain separate facts. Fresh compilation
and indexed temporary-table staging are candidate experiments outside production, with compilation, tempdb,
result correctness and runtime checked together; neither is applied automatically.
Available SQL Server parameter metadata retains names, types and explicit availability states. A native comparison
describes only compiled/runtime representations in the same selected actual plan; literals and new value hashes
are not retained. That difference alone establishes neither parameter skew nor harmful cached-plan reuse.
The derived report references exact definitions and omits duplicate SQL/XML and executable change SQL. Truncated
warehouse previews remain explicit recovery gaps; this mode does not fetch the missing native body automatically.
An actual SQL Server plan can also preserve the engine's verdict that unstable adjustments disabled memory grant
feedback. The shared insight keeps prior-execution granted memory separate from current grants, spills and pressure;
healthy feedback states and plans without matching runtime evidence do not produce this diagnosis. It requires no
additional source query and does not establish harmful parameter reuse.
Spill explanations distinguish a statement's memory request from its received grant in the same actual plan.
Receiving the requested amount, or more, establishes neither sufficient operator memory nor the spill's cause.
Add `--with-recovery-payloads` to either `read` or `analyze` to permit bounded native recovery reads. For example:

```sh
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis cached-plans --with-recovery-payloads --max-recovery-objects 2 --max-recovery-bytes 16777216
```

The optional object and byte limit flags require `--with-recovery-payloads`. Limits apply across all selected
families and native batches in this command, independently of warehouse response and projection-proof budgets.
Invalid limits fail before configuration or credential resolution. A recovery budget too small for the required
batches fails the command without downloading a partial selection or presenting truncated bodies as complete.

`tuning` explains one database-target `tuning` capture from `tuning-observations`. Native recommendations,
automatic tuning options, forced-plan counters and Query Store hints retain their exact source, scope and original
row references. Unknown scopes, unavailable definitions and native source failures remain distinct from warehouse
read gaps. Recommendation-time CPU comparisons and exact aborted-run counts do not establish current savings.
The analysis does not attach another capture's workload, infer a live query from an ID, or supply executable scripts.
Large script or hint previews use the same explicit, bounded recovery option described above.

`statement-work` reads database-target `statement-counters` captures. It preserves native query identities, point
clocks and reset qualification, and shares the scan's temporary-work explanation. Typed native metrics distinguish
block traffic and recorded I/O timing from internal-table and sort-pass counts. Exact integer counters survive
readback; missing metrics remain unknown. A point does not establish an interval or safe delta, and matching a
query ID does not attach SQL from a separately captured definition. No mode reconnects to the monitored database.
Yugabyte points retain the observed node and native user/top-level statement context. Readback requires the same
qualified member, process generation and observation bounds. Scan summaries attach temporary work only when one
local native row supplies those dimensions; global or mixed-context groups do not inherit a representative row's
temporary counters. A process generation does not establish a counter-reset time.

MySQL and MariaDB scans retain statement-counter evidence from the same digest read, including exact error and
warning totals. A warning total counts warnings, not statements with warnings; an absent total remains unknown.
The Health panel starts folded and pages ten native points at a time. A disabled counter source stays unavailable
even when an independent slow-log source supplies workload evidence.

Operational file I/O rows also retain exact recorded governance stalls, logical catalog size, physical disk
allocation and the file's configured maximum as separate facts. Zero, unknown, not applicable and no fixed maximum
remain distinct. These observations establish neither free capacity, enabled autogrowth nor current pressure.
Governance stalls are not subtracted to manufacture physical storage service time. SQL Server collects the extra
columns through the same targeted read, with native version/platform guards; its changed query shape still needs
work-cost qualification. The app shows these facts in collapsed file details, keeping dashboard summaries concise.

`storage` explains the same Operational log, filegroup and tempdb reads used by a scan. Exact integer byte totals
survive saved snapshots and warehouse readback, including values above JavaScript's safe integer range. Capacity
thresholds use those original integers; the chart remains a display projection. The filegroup allocation covers
only writable online data files, while a separate `sys.database_files` census retains every visible file, including
read-only, offline and non-ROWS files. Native file types, state codes, growth settings and page counts remain
separate facts. A snapshot's maximum file size and FILESTREAM container usage do not become allocated data-file
bytes or free filesystem space. Tempdb allocation and object classes retain their shared scope. Hidden metadata,
missing row bodies and uncertain source shutdown remain explicit gaps; a visible catalog is not proof of complete
metadata visibility or current storage pressure. The Health evidence starts folded and pages ten points at a time.

`statement-grants` reads database-target `statement-grants` captures from `operational-samples` through the same
pure explanation used by scans. Each original cache entry retains its handles, offsets, generation, exact integer
counters and available statement text, separately from the native member, process generation and observation bounds
of its read. A matching query hash supplies no current request, execution-group, wait or pressure attribution.
Last/minimum/maximum values are not combined into a synthetic execution, and a balanced last execution or missing
cache row does not prove recovery. Native partial/unavailable coverage and warehouse decoding gaps stay distinct;
readable positive entries survive a gap without rewriting the original source header. Complete statement text that
exceeds the UTF-8 warehouse preview uses the same explicit bounded recovery option; no source query is executed.

`cache-entry-memory` explains selected native cache objects from `operational-samples`. Each object's bytes and
lookup/reference counters appear once even when several selected statements share it. Native cache lookups are
separate from execution and compilation counts. Different recorded cache-key attributes can explain separate
contexts for the same exact batch and statement; they do not establish wasted memory or current memory pressure.
The authenticated capture retains the exact original statement selectors. Missing selectors cannot trigger a
cache-wide search, and an ordinary snapshot cannot bypass selection or publish an empty selected-cache report.

Use `collect once --selection FILE` for an exact serialized `CollectionSelection`, with the normal output and
budget flags. The JSON file is limited to 1 MiB and replaces `--collection`, `--from` and `--until`. A cache-memory
selection uses `kind: "cache-entry-memory"` and a `focus` containing the original `NativeReadContext` and selected
`CachedStatementContext` values. Obtain those from captured source/definition evidence; a query hash alone is
insufficient. This investigation remains cost-unqualified and does not expand default scans.

`runtime-distributions` inspects a database-target `runtime-distributions` capture. Every distribution retains its
own native statement identity, observation clock, source, reset qualification and population. Native histogram
bounds and counts remain exact; an approximate percentile is not an individual execution time. Separate source
outcomes remain visible when a summary can be read but its histogram cannot. This mode does not construct a
synthetic workload, attach separately captured SQL or establish parameter-sensitive plan reuse.

`compilation` explains a database-target `query-compilation` capture through the same pure view used by scans,
the Health panel and MCP output. Exact counts and separate average, last and maximum compile-memory measurements
retain the complete native query identity and its lifetime. Collector read bounds do not replace native compilation
clocks. These records do not establish a current compiler, a memory owner or a selected-window compilation rate.
Partial, unavailable and complete-empty evidence stay distinct; an empty
per-query selection cannot erase separately collected database-wide compilation totals.
Binding and optimization duration/CPU measurements stay separate. Native averages retain floating-point precision;
last measurements remain exact microsecond counts. They are not summed into total CPU or concurrent memory use.
Compilation-only reads inspect the native profile without discovering runtime replica groups. Runtime reads
independently retain their native membership authority. A successful empty catalog can use execution-group IDs
positively observed by the same fully drained bounded runtime summary, with unknown roles and incomplete inventory.
This `ObservedWindow` route uses seven source dispatches, including final native guards. It does not authorize
history-wide absence, an unpartitioned workload, tuning management or fallback after a failed catalog read.

Use `--compilation-baseline RANGE_SHA256` with `evidence analyze --analysis compilation` to compare two original
captures. Scans reuse an eligible saved baseline through the same pure comparison. Comparisons require matching
native database incarnations and full query identities, ordered native read bounds and compatible counters.
Missing entries, partial reads, changed counters and unknown native identity remain explicit gaps. A count increase
does not measure current compiler memory or compilation cost. SQL Server deployments without an acquired native
database incarnation cannot produce a qualified interval; a connection address is insufficient.

When a scan already holds a complete statement definition with the exact native query incarnation, its explicit
`OPTION (RECOMPILE)` policy can explain repeated expensive recorded compilation independently of runtime-window
comparability. The binding reuses acquired definitions and adds no source query. The same pure explanation accepts
retained authenticated policy evidence; previews, truncated SQL, matching query numbers and neighboring captures
cannot supply it. Standalone compilation-counter capture does not fetch statement definitions, so policy remains
unavailable without a separately selected definition range. An explicit policy is not proof of every individual compilation cause, harmful plan reuse,
concurrent compilers or current memory pressure.

Use `--definitions-range RANGE_SHA256` to select independently captured Query Store definitions for compilation
analysis. Linking requires the original logical database incarnation and complete native query identity. Counter,
baseline and definition reads share one budget and deadline while retaining their own receipts and clocks. A preview
cannot supply missing SQL; `--with-recovery-payloads` permits bounded hydration of the original payload.

`completed-spills` explains a `cached-runtime` capture using exact cache entries, plan generations and native completed
spill counters. It does not merge entries by query hash or attribute historical spills to a current operator. Its
optional `--definitions-range` selects `cached-definitions` or `retained-plans` object definitions, independently from
the runtime capture. Missing SQL, partial native coverage and missing archive rows remain separate gaps.

Scans, Health and MCP reuse that explanation when a provider supplies an exact cache-entry read. Built-in automatic
acquisition still requires native cost and execution-owner qualification; an unavailable read is not a measured zero.
Retained actual plans also preserve separate per-operator page-server demand, read-ahead and LOB counters. Their
original runtime population does not identify a current request or establish page-server latency.

`page-server-reads` explains selected original Query Store runtime fragments through the same pure counter policy
as scans. It retains exact last/minimum/maximum page counts, native float averages/deviations, outcomes and replica
groups. Multiple fragments never acquire a representative last execution or combined deviation. Missing fields,
native NULL, zero and an unavailable Hyperscale capability remain distinct. These facts establish neither a current
owner nor remote-read latency or local-cache hit rate.

Its optional `--definitions-range` links independently authenticated Query Store SQL by exact logical database and
native query incarnation. For this analysis, the primary range may also supply its own QueryDefinitions family;
both family reads share one cumulative budget and deadline. Previews are explicitly shortened, and incomplete
payloads require the existing bounded recovery option. Unresolved statement identities remain inspectable as
original capture positions, with no invented query ID. SQL and detailed qualifications stay folded in the UI.
The ordinary MCP scan view reanalyzes retained plan facts; an informational archive intake action remains separate.

`compiler-admission` explains an exact affected request and independently captured optimizer gateways. It preserves
native resource-pool identities, active/waiting counts and admission policy. The request's pool membership and
current compilation-memory holders remain unknown; gateway counts do not establish memory exhaustion. Scans retain
the gateway facts they already read. A focused collector selection binds the original request, statement and native
compute generation, stays within its declared budget, and remains cost-unqualified until native qualification passes.

After a completed compiler-admission answer, the controller can retain one separate compilation-history question.
It authenticates the original request and gateway captures before admission, then uses the existing compilation
reader under an independently qualified native member, process and updateability scope. Dataset identity stays
logical. A recovered wait keeps this owed context; it does not reopen the gateway question.

A source-classified log-governance request wait can instead owe one Hyperscale component question directly,
without a fabricated compiler prerequisite. Its original Activity receipt remains the basis after recovery.
The same recipe, cost-artifact, native-scope, scheduler and shutdown checks apply; an unqualified recipe cannot
open the source. The resulting component capture supplies context, not proof that the waiting query caused it.

The result becomes one retained capture pointer through the existing offline queue and atomic checkpoint update.
Restart reconciliation cannot replace an admitted read or clear an unconfirmed shutdown. Capturing history does
not establish a current compiler, memory owner or root cause; the original rows need their own bounded analysis.

`maintenance` explains current maintenance statements and their participation in blocking from an authenticated
activity capture. It reuses captured statements, request/session generations and native wait-owner relationships;
it does not query operation progress or locks again. Complete SQL identifies the operation, while missing SQL stays
unclassified. The report distinguishes a maintenance request waiting on another owner from a session running
maintenance that blocks captured requests. A session-level blocker does not establish which request acquired its
locks, especially when several requests share that session. Raw statement text stays in the original evidence;
the report carries references, explanations and explicit gaps. Full text outside the warehouse preview requires
the same bounded recovery option. This mode does not establish operation-specific resource consumption, an object
target or completed/paused/failed maintenance history from a current statement alone.

`blocking` reads a focused native blocking capture with `--analysis blocking`. It selects only original Activity
observations and uses the same pure graph and explanation as the SDK. Native session/request generations, every
reported owner, original evidence references and read boundaries remain separate. Prepared-transaction placeholder
0 is unresolved transaction ownership, not a session or a particular GID; a same-session relationship does not
identify an owning request. Missing archived rows, changed requests and unqualified original scope remain explicit
gaps. This analysis opens no source database and never replaces the original incident with a later baseline. Text
output limits displayed relationships and chains; JSON retains the full analysis within the caller's output cap.
The existing MCP `blocking` tool reads Live; it does not read an archive range.

`maintenance-progress` reuses live readers for PostgreSQL index builds, client VACUUM, ANALYZE, CLUSTER and VACUUM FULL,
and YugabyteDB index builds. A focused selection names the original Activity participant. Captures preserve native
object IDs, commands, phases and exact work counters. PostgreSQL blocks, tuples, bytes and phase fractions remain
separate from YSQL backfill estimates; a phase fraction is not whole-statement completion. Version-specific fields
remain unknown when unavailable. Missing original compute identity prevents matched historical attribution, and an
absent progress row never establishes completion. This investigation adds no ordinary scan query and remains
cost-unqualified.

Documented PostgreSQL concurrent-index waiting phases identify maintenance as the affected request: waiting for
writers, older snapshots or readers. The reported locker PID remains a historical numeric reference. A separate
blocking observation can establish current blocker generations; it cannot identify the historical locker or prove
that the earlier phase persisted. Existing lock-wait investigation handles that question; phase text alone cannot
authorize a new owner lookup. [Owned native cases](../dev/postgres/maintenance-waits.md)
exercise a held writer and a separately acknowledged completion through the same readers.

Use `maintenance-blocking` to explain an original maintenance-progress capture together with an explicitly selected
later focused-blocking capture:

```sh
culprit collect evidence analyze --config archive.json --range "$CULPRIT_MAINTENANCE_RANGE" \
  --analysis maintenance-blocking --blocking-range "$CULPRIT_BLOCKING_RANGE" --json
```

Both distinct ranges use the same archive configuration, cumulative evidence/recovery budget, cancellation owner
and deadline. Recovery payloads remain opt-in with `--with-recovery-payloads`; the second read does not receive a
new allowance. This command opens no source database. The shared SDK requires the exact same original Activity
focus, including its receipt, request generation and statement token, and verifies the native scope, role and clock
bindings of both captures. Contradictory or unordered inputs are refused. Text separates the historical progress
from the later blocking observation; JSON retains both authenticated contexts and their read gaps. Missing archived
rows remain missing, and original unverified scope is never repaired with the later capture. Neither equal PIDs nor
current SQL establish historical lock acquisition, maintenance-caused pressure or successful completion.

`current-request-work` explains MySQL/MariaDB current-statement counters and linked native stages using the same
reader as live progress. The focused capture rechecks the original member, process generation, session, statement
event and timer. Exact temporary-table, sorting, row and picosecond counters retain their native units; they do not
establish spill duration, memory pressure or the incident's cause. Stage absence, missing instrumentation, permission
refusal and unlinked stages remain distinct. The existing long-running incident can retain this evidence and its
history without treating a later missing request as a resolved explanation. Both warehouses, the SDK and CLI
`--analysis current-request-work` use the same pure report. The native investigation remains cost-unqualified.

PostgreSQL 14 and later use the same `current-request-work` path for native COPY progress. The existing progress
reader retains COPY direction and transport, database/relation OIDs and exact byte/tuple counters, bracketed by the
same native request checks. A zero total is an unknown denominator. COPY TO a query can have relation OID zero;
neither case supplies a completion percentage, ETA or bottleneck diagnosis. Original unverified scope stays
unverified, and a missing later progress row does not establish completion. This investigation remains
cost-unqualified; it does not execute application SQL.

Live progress preserves unavailable request evidence as unknown and continues polling. Only directly observed idle
state supplies `running: false`. Its bounded process-list fallback carries independent session context, without a
request-owned plan. The UI caches a plan only under an opaque native request key; equal SQL text does not establish
the same execution.

`replication` explains the original replication capture through the same pure report used by scans. It retains
SQL Server database, group and replica contexts separately; PostgreSQL sender, receiver and slot positions; and
MySQL/MariaDB channel, receiver and applier facts. Exact positions, native error codes, source availability and
clock provenance survive both warehouses and offline analysis. Offset-free native dates remain offset-free.
A last-commit age is not replay latency, a queue divided by an active-period rate is not a catch-up estimate, and
an inactive synchronized slot does not establish an abandoned consumer.

Use `evidence analyze ... --analysis replication` or read `--family replication-samples`. Every acquired point is
examined for supported native states before limiting detailed presentation. Original counts and source states stay
visible when a warehouse read is incomplete or the detail limit is reached. UI details are folded and paged; CLI and
MCP use the same explanation. A stopped channel without a recorded error does not establish manual intent or
promotion. Slot guidance uses read-only inspection; Culprit does not offer slot-drop SQL or an arbitrary retention
cap. This does not establish an application query responsible for replication delay. Source query shapes still
require native work-cost and cancellation qualification before automatic production admission.

`evidence analyze ... --analysis connection-outcomes` reads `operational-samples` and uses the scan's pure
connection/session report. PostgreSQL database sessions, lost-client connections, fatal/operator terminations and
recorded state times remain separate from MySQL/MariaDB server-wide attempts, aborted connections, error categories
and thread gauges. Exact counts, NULL/absent fields, native resets, original read bounds and unrelated source rows
survive archive readback. Server uptime cannot supply a counter lifetime. These aggregates do not identify a failed
query, login or service, and recorded session time is not completed-session latency. Analysis opens no database or
recovery payload. The CLI bounds printed details; MCP's existing `waits` response uses the same retained facts and
keeps native counts and incomplete coverage visible.

`jobs` explains a PostgreSQL/Timescale `--collection jobs` capture from `operational-samples`, using the same
public-view reader and pure explanation as scan diagnostics. Installed view/column identities, native read bounds,
owner visibility, logging and selected run-window limitations survive both stores. State totals remain distinct
from individual retained runs. Missing history never establishes that no job failed, and a recorded running state
does not identify the process or query responsible. Current job labels cannot establish historical ownership.
Read [Timescale job coverage](providers/timescale.md#background-jobs) before scheduling this unqualified source.

`cleanup` explains retained cleanup horizons and version-store observations through the scan's pure view.
PostgreSQL `cleanup` captures retain sessions, prepared transactions and replication slots, their separate data
and catalog horizons, comparable transaction distances and ties. Missing coverage metadata does not discard
positive holder observations; it withholds a qualified complete boundary. A previous statement or session label
does not identify the code that opened the retaining transaction.

SQL Server `operational` captures retain independent oldest-transaction and ADR/PVS observations. Native skipped-page
counters distinguish readable-secondary retention, the oldest active transaction, long snapshot scans, online
index rebuilds and the oldest aborted transaction where those columns are available. The separate
`transaction_not_cleaned` counter is threshold context, not evidence explaining large PVS. Off-row KiB excludes
in-row versions, native sequence values are not wall clocks, and skipped pages cannot be added into a retained
footprint or cleanup rate. See [Microsoft's PVS field definitions][pvs-fields].

The existing native queries also serve scans; no extra query is needed to retain these fields. Each scan source
keeps its own read bounds and publishes facts only after confirmed shutdown. An archive report uses its captured
acquisition bounds without inventing per-source clocks. Separate PVS and transaction reads cannot establish a
SQL owner by matching transaction numbers. Both warehouse paths retain the original source outcomes and facts.

`task-space` explains database-target task allocation/deallocation captures from `operational-samples`.
Native fleet capture supports a verified writable Azure SQL Database or boxed SQL Server 2022+ source; Managed
Instance fleet capture remains unavailable. The shared native owner is checked before, after and at completion,
with the same one-second statement and five-second absolute limits. Boxed capture permits thirteen native queries
and Azure permits eight; stricter caller budgets are preserved. Recipe `mssql.task-space` version 3 remains
cost-unqualified and never enables automatic polling. It retains
exact page counters, native task tuples, request-check clocks and source coverage. The same pure view serves
scans and authenticated readback from either warehouse. Current checked SQL does not prove allocation ownership;
partial, unavailable and complete-empty sources stay distinct. The shared explanation distinguishes recorded
user-object, internal-object and mixed allocation/deallocation activity. Only four explicitly measured zero
counters establish a zero point; missing counters remain unknown, and neither a positive class nor deallocation
establishes a current footprint, spill, allocating statement or maintenance owner. Page-wait and task-space
explanations also appear as bounded entries in the MCP `waits` response without copying raw SQL or observations.
The dataset is opt-in and is not added to a default
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

The SQL Server/Managed Instance operational point includes bounded native scheduler queues, process/system memory,
grant waiters, pending I/O and current I/O waits. Explicit scan shared-resource diagnostics use the same native
reader. `instance_resources` analysis preserves every population's retained, sampled and unknown counts; an
incomplete population cannot admit optional follow-up work. These are shared instantaneous observations, not
database CPU percentages, storage latency or proof of a particular query's cause. Health keeps the evidence folded
and informational. No additional native reads run in an ordinary scan without the shared-resource selection.

SQL Server/Managed Instance `jobs` captures and explicit scan failure diagnostics share one Agent metadata reader.
It reads job/step definitions, retained outcomes, current Agent activity and service/startup context, with at most
13 queries, one second per statement and five seconds overall. Azure SQL Database reports this source unavailable
without querying `msdb`. Commands, raw history messages, credentials and proxy configuration are omitted. Historical
step numbers and names remain historical; current definitions cannot identify the command used by an earlier run.
Permission gaps, partial history and native local times remain explicit. This is metadata collection: Culprit does
not start jobs. Its recipe remains cost-unqualified until the applicable native profile is measured.

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
observations. The default scan does not run this reader. The collector uses the same native platform discovery:
`culprit collect once CONNECTION --collection current-request-memory`. Boxed SQL Server retains its qualified
instance/process owner; Azure SQL requires a verified primary identity. Other providers report this profile as
unsupported rather than returning empty evidence.

Facts distinguish waiting requests from granted reservations, retain exact KiB and worker counters, and associate
a request only when native generations, request times and handles agree. Missing associations remain unassigned
facts with partial coverage. Current SQL is request context; it does not prove which statement acquired a grant or
which holder caused another request's wait. Resource Governor pool/semaphore IDs are native resource identities,
not Azure elastic-pool identities. Optional columns and native NULLs remain explicit.

The bounded probe has no sort, aggregate or plan retrieval. Its query chain uses loop joins and one execution
thread to avoid diagnostic hash grants, following Microsoft's
[memory-grant troubleshooting guidance](https://learn.microsoft.com/en-us/troubleshoot/sql/database-engine/performance/troubleshoot-memory-grant-issues).
SQL detail uses the existing guarded activity reader;
identity checks and confirmed shutdown precede accepting the result. Scan and collector limits are 200 rows,
nine queries (at most six on Azure), three seconds per statement and ten seconds overall. These limits do not prove
low server cost: qualify the source under representative load before scheduling it. Native application, login and host labels follow the
existing activity collection policy; SQL literals are redacted. Snapshot sharing removes those client labels.

## Session and request census

SQL Server Activity retains selected-database user sessions, including idle and dormant rows, together with their
current requests and required blocker context. The same bounded metadata query prioritizes current requests, open
transactions and blockers before quiet rows; a truncated or permission-limited population stays partial. Native
client-library, client-reported process, TDS protocol and login/last-request clock facts remain session context.
Native times without a UTC offset remain explicit tokens. They are not reassigned a time zone or an execution epoch.

Use `--analysis session-census` on an archived Activity range, or `analysis: "session_census"` with the existing MCP
`analyze_capture` tool and an explicitly installed local Outbox reader. Local analysis uses the same canonical
single-capture worker as retained blocking inputs; it does not query a database or fetch a latest capture. Live
renders the same informational view with raw entries and qualifications folded. JSON retains original source,
native clocks, row counts, exact counters and individual request relationships.

A session can have several MARS requests. Session totals are not added across those rows, and neither session nor
request counts are transport-connection counts. A request's native `connection_id` is retained only on that exact
request row. Quiet rows receive no SQL enrichment. Prior session batches remain separate from current-request
SQL; no query, lock owner, service identity or incident is inferred from a session ID or a client label. The
session-only progress endpoint is withheld when several current requests share its ID. Idle or dormant sessions
alone are not faults. The Activity recipe remains cost-unqualified until its changed native template is measured
for an applicable profile.

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

Each source also declares its `SourcePartition`: its own target namespace or a native connected member and
generation. A read-only routing preference is not member identity. Qualified local members keep independent heads,
episodes and focused-work ownership; a restart cannot silently continue the previous generation. Unidentified local
points remain partial raw evidence and do not create durable episodes that could never be safely merged or resolved.
Diagnostics retain private scope bindings for every referenced episode, including older generations; public
analytical projections expose only opaque scope keys. See [SQL Server replica coverage](providers/mssql.md#read-only-replicas)
for the current native identity gates and remaining source restrictions.

Baseline sources preserve stricter caller budgets and cap retained rows at 500, statement time at one second and
total native time at five seconds. Their native phase ceilings are seven for SQL Server, six for PostgreSQL,
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
connection. For a supported focused question from the current baseline, the worker records `deferred` with `cost-unqualified`
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

Qualified page-wait and execution-grant investigations share the existing fleet scheduler. A durable immutable question owns its original
evidence pin; repeated baselines cannot enqueue competing work for that episode/stage. Periodic collection takes
precedence. Focused work must fit its native timeout, shutdown and resource gap before another periodic deadline.
After restart, a fresh baseline must establish the source behind the saved route before a focused job can run.

An execution-grant follow-up requires the original request, statement and independently qualified native member and
process generation. It reuses the existing current-request-memory reader and selects that exact participant before
SQL enrichment. A later grant row adds historical context; an empty result proves neither request disappearance nor
recovery. This question has its own finite receipt, and changing the question invalidates a different recipe's cost
approval. Its recipe remains cost-unqualified and adds no routine baseline query.

A shared-resource follow-up preserves the original database question and binds a separate positive operational
capture of its exact native member and process generation. Discovery labels, empty captures and unknown generations
cannot supply this authority. Its recipe and measured cost belong to that execution resource; the database's
activity population and generation are not substituted for shared ownership. The expected shared generation reaches
the native reader and is checked before page-header work. Native ownership comes from the existing read path, with
no additional periodic query.

The database and its explicit shared owner are separate page-wait stages, with one outstanding stage per episode.
An unresolved database lookup can progress to the shared stage; another alias for the same physical resource cannot
create duplicate work. Both stages retain their own finite completion receipts. The shared result updates the original
database incident, including after recovery, and never replaces its scope with a shared-resource head. A delayed
lookup older than a newer positive observation may add historical analysis but cannot clear that current incident.

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

The current export format is **51** (delivery schema **4**, preparation **25**). New contracts require new collector/recovery state; there is no migration of
old saved captures. SQL text and retained plans pass through the common payload inventory and redaction rules.

[pvs-fields]: https://learn.microsoft.com/en-us/sql/relational-databases/system-dynamic-management-views/sys-dm-tran-persistent-version-store-stats

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

Inspect recorded client work for one local attempt with
`culprit collect outbox overhead /path/to/outbox <observation-id>` (`--json` returns the same typed report).
The SDK exposes `culprit_engine::inspect_source_overhead(directory, observation_id)`. This read-only path uses
only the bounded manifest and local journal, so it works while the collector retains its owner lock. It does not
load result streams, discover credentials, contact the database or verify archive publication. Failed descriptions
can have no engine identity; unfinished query totals remain unknown. The report withholds SQL previews and retains
template SHA values, exact decoded rows/charged bytes, client timings and failure/cancellation accounting.
An allocated attempt without an initial manifest has no report yet. Local inspection never promotes an incomplete
attempt, clears native debt or authorizes publication.

For a committed capture, use the same report through its archive:

```sh
culprit collect evidence analyze --config archive.json --range "$CULPRIT_CAPTURE_RANGE" \
  --analysis source-overhead --json
```

This path authenticates the capture manifest and warehouse publication without reading native result rows,
membership proofs or SQL/plan recovery payloads. Original source-query totals remain separate from the bounded
per-template task list and archive I/O. Human output shows the five templates with the most client elapsed time;
JSON retains every recorded template. Client elapsed time includes transport and decoding; it does not measure
database CPU, physical I/O, memory grants or database execution time. The archive path covers complete committed
captures; use local inspection for failed or unsettled attempts.

Where the SQL Server reader already acquires and rechecks native source authority, those same queries also retain
its own session counters. A confirmed, generation-bound pair reports integer CPU milliseconds, native read counts,
buffer-pool dirty marks and the two sampled session-memory gauges. Dirty marks are not completed storage writes;
session memory is neither an execution grant nor a peak. The interval excludes work before the first observation,
the remainder of the final boundary batch and cleanup, so it does not measure the whole capture or each template.
Missing, changed or interrupted authority observations withhold measurements without issuing replacement SQL.
Sources without that existing pair, including Query Store paths, remain unknown. These observations do not qualify
production cost or authorize another source read. Original session/source authority stays in private authenticated
metadata; the shared report and warehouse publication expose one numerical summary and an opaque proof reference.

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
| `CulpritPublishedRangesV1` | Exact capture completion, batch membership, coverage, original source-query totals and source-read metrics |

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

Generated semantic `...Rows` functions require both a present semantic `RecordId` and the published
`ReferencesComplete` flag before revision grouping. Physical eligibility/readback keeps unresolved rows and checks
that each row's flag matches its completion record. Nullable IDs therefore cannot collapse unrelated observations
into one revision. Original reference counts remain in the publication's `ReferenceCoverage` value.

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

The owned SQL Server task-space experiment exercises 1, 16 and 64 allocating sessions, idle/load comparisons,
caller row limits and cancellation. It records per-template native CPU/read/write deltas and sampled grants,
alongside preparation, observer and cleanup costs. Run it only through the disposable fixture:

```sh
export CULPRIT_NATIVE_COST_RECEIPT_DIR="$(mktemp -d)"
python3 dev/smoke-providers.py mssql --test culprit-mssql \
  activity::tempdb::native_tests::qualification::owned_task_space_cost_matrix_preserves_native_work_and_settlement
```

The receipt directory must be absolute and already exist. Missing grant samples do not prove zero grants or a peak;
failed or changed-owner measurements remain invalid. Warm samples, uncontrolled caches and emulated hardware do
not establish a production envelope. This experiment does not issue a qualification receipt or enable a recipe.

The source-authority experiment checks the unchanged context query before, during and after 32 unrelated idle
sessions, using both authority and page-lookup projections. It records actual-plan grant/serial checks and deadline,
shutdown and cleanup outcomes. Use a fresh receipt directory with the same runner:

```sh
export CULPRIT_NATIVE_COST_RECEIPT_DIR="$(mktemp -d)"
python3 dev/smoke-providers.py mssql --test culprit-mssql local_source::native_tests::
```

A fast completed query does not exercise active cancellation. That case stays explicitly unqualified unless the
observer sees the exact source template active. Endpoint evidence does not qualify Azure SQL or Hyperscale.

Default provider smoke tests execute the real selected-writer payload SQL with empty original selectors and verify
bounded completion without unsolicited rows. The stricter writer scenario requires a mapped coordinator wait to
remain observable while all five writer/reader controls are checked. Select it explicitly:

```sh
python3 -B dev/smoke-providers.py mssql --test culprit-mssql \
  activity::page_targets::native_tests::writers::local_native_writer_plans_keep_targeted_writes_separate_from_readers_and_other_resources
```

An expired or unqualified wait fails that positive qualification; it does not by itself establish a collector regression.
Retain failed and successful runs separately. A pass establishes neither a direct latch holder, a per-query causal
share nor representative production cost.

Local recipe qualification uses the selected provider's trusted native issuer and the existing capture owner:

```sh
culprit collect qualification run CONNECTION --recipe RECIPE --selection request.json --outbox DIRECTORY
culprit collect qualification inspect CONNECTION --outbox DIRECTORY
culprit collect qualification revoke CONNECTION --recipe RECIPE --outbox DIRECTORY
```

The selection file requests a measurement; it cannot grant approval. An accepted native result is pinned and
installed for that saved connection, route, recipe and native scope. Restart verifies the original artifact again;
revocation or a changed route withdraws its authority. Only explicitly registered executable provider policy can
issue approval. The built-in SQL Server experiment measures the original eight-query Azure SQL compilation route
but currently refuses approval: its native measurement window is partial, peak-grant evidence is unavailable and
active cancellation remains unproved. Its recorded work is evidence, not permission for automatic collection.

Optional replication fixtures exercise the same native readers used by scans and retained collection:

```sh
python3 -B dev/smoke-providers.py postgres-replication postgres-16-replication
python3 -B dev/smoke-providers.py mysql-replication mysql-no-performance-schema \
  mariadb-replication mariadb-no-performance-schema mysql-moving-replication
export CULPRIT_NATIVE_COST_RECEIPT_DIR="$(mktemp -d)"
python3 -B dev/smoke-providers.py mssql-replication
```

PostgreSQL uses physical streaming, a native slot and a restricted monitor, including revoked replay-function
permissions. MySQL and MariaDB exercise native channel status with Performance Schema on and off; the separate
MySQL moving pair verifies actual source-to-replica position movement. SQL Server uses a populated availability
group and records native work, complete/partial member visibility, query/row limits and confirmed reader shutdown.
Its physical hostnames meet Linux's fifteen-character limit; native admission checks both full owner nonces in
the database name as well as each member's exact hostname.
Each topology owns its generated credentials, loopback ports, labelled containers and network, and removes them on
exit. These tests never select saved connection profiles or cloud resources.

These small disposable topologies prove native semantics and lifecycle behavior, not representative production
cost. A cancellation attempt is verified only when the exact active template was observed and its cancellation
confirmed; a quiet connection after a fast query does not establish that. Unverified cancellation remains explicit,
and these experiments do not enable automatic production recipes.

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


### Recorded native cache ratios

`culprit collect evidence analyze --analysis counter-ratios` uses only the original `OperationalSamples` capture and
its source coverage. Scans expose the same shared explanation in `Report.counter_ratios`. RBPEX inputs come from the
existing performance-counter read: native object, instance, counter types and shared scope must match. Raw integers
and rejected signed native inputs remain exact; missing inputs, an unknown profile, a zero base or a numerator above
its base withhold the percentage. There is no selected-database denominator, recent rate, pressure grade or query-cause
claim. Native read time is independent of counter reset/lifetime, and unavailable archive rows remain partial.

The query retains at most 128 native counter rows and adds no default roundtrip. That cap does not bound native counter
enumeration work. Actual Hyperscale compatibility and measured native cost remain unqualified; boxed SQL Server
fixtures cannot establish positive RBPEX support.

### Performance counters and Hyperscale log components

`--analysis performance-counters` explains the original execution-memory, optimizer/cache-memory,
parameterization, temporary-table, plan-cache and log-flush counters. Their 34 fixed names share the existing
native SELECT; plan-cache instances are finite and database log counters use only `_Total`. Native types determine
units and gauge/cumulative/fraction meaning. Shared counters remain shared; missing fields remain unknown.
Collection time does not supply a reset boundary or justify a recent rate. The SDK, CLI, Health and MCP reuse
the same interpreter and existing cache-ratio policy.

For an explicit Hyperscale read into an existing outbox:

```sh
culprit collect once CONNECTION --collection hyperscale-log-components --outbox DIRECTORY
```

This dedicated source reads `sys.dm_hs_database_log_rate(DB_ID())` for the selected database. It validates the
native schema, permissions and updateability before reading, and rechecks the same authority afterward.
Its ceilings are 1,024 rows, 2 MiB, one second per statement and five seconds overall; the source declares
at most ten queries including admission and rechecks. A standalone read uses eight; its cost evidence cannot
qualify the ten-query incident route. These limits do not establish server CPU/I/O cost approval.
See the [native function and required permission](https://learn.microsoft.com/en-us/sql/relational-databases/system-functions/sys-dm-hs-database-log-rate).

`--analysis hyperscale-log-components` replays committed evidence through the same pure view. Maximum log rates,
catch-up rates and unapplied-log bytes retain their separate units, zeroes and unknowns. Component GUIDs and
native role codes remain independent. A report clock without an offset is not relabeled UTC or matched to a wait
clock. These reports alone establish neither query ownership, measured throughput nor a catch-up estimate.
