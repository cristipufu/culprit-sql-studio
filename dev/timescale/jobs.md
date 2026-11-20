# Native jobs conformance

These ignored tests exercise the same `native::jobs::capture` reader used by scans and retained collection. They
use `collect_once` and the existing owned `ReadSession`; they do not run a job, modify permissions or write data.
Passing these tests does not qualify production cost or automatic collection.

## Owned fixture setup

Use the existing `Database` supervisor in `dev/smoke-providers.py` and its pinned Timescale 2.31.0 fixture. Retain
the supervisor's generated passwords, loopback port, nonce label and `finally` cleanup. Do not use a saved
connection, an existing database service, or the ordinary development database.

Before starting this disposable instance, supply the PostgreSQL startup option
`-c cluster_name=culprit_owned_<Database.owner>`. `Database.owner` is the supervisor's 24-character lowercase hex
nonce. Run the normal fixture setup first so `smoke_reader` exists, then run `dev/timescale/jobs.sql` through that
same supervisor-owned container's SQL control path. Keep the container cleanup owner alive before starting setup;
on cancellation or setup failure remove the owned container rather than declaring the jobs settled.

The setup refuses a different database, marker, extension version or pre-existing test schema. It registers two
custom jobs under different native owners, lets each background worker fail once, checks the real public history
and exact statistics, then pauses both jobs. Its setup polling has a 30-second deadline. It writes no internal
statistics or history records. A foreground `CALL run_job(...)` is insufficient for this fixture because scheduler
start/end accounting is a separate native path. See the
[native job API](https://github.com/timescale/timescaledb/blob/2.31.0/tsl/src/bgw_policy/job_api.c) and
[background worker lifecycle](https://github.com/timescale/timescaledb/blob/2.31.0/src/bgw/job.c).

The same guarded setup also registers one separately tracked paused job with a real
[`check_config` function](https://www.tigerdata.com/docs/reference/timescaledb/jobs-automation/add_job).
Registration invokes that function once, incrementing an owned sequence. The native acceptance compares the
sequence before and after the actual source factory capture and requires exactly that one invocation. It also
requires the original schema/name and genuine absence of a checker on the two ordinary jobs. This is a required
positive case, not a skip when the installed view or fixture is unavailable. The checker job is never scheduled;
its objects belong to the same disposable schema and supervisor cleanup. No job config or private error is retained.

Only this disposable fixture revokes PUBLIC access to `job_history`. The limited login can still read its native
owner-filtered `job_errors`; the source must retain that weaker layout and missing verdict fields. The
[2.31 public view definitions](https://github.com/timescale/timescaledb/blob/2.31.0/sql/views.sql) list all jobs in
`jobs`/`job_stats` and filter retained runs by owner. `pg_monitor` does not substitute for that ownership rule.

## Environment and selection

Reuse the supervisor's existing `CULPRIT_SMOKE_OWNER`, `CULPRIT_PREVIEW_PG_DATABASE`, `CULPRIT_PREVIEW_PG_PORT`,
`CULPRIT_PREVIEW_PG_PASSWORD`, `CULPRIT_SMOKE_LIMITED_USER` and `CULPRIT_SMOKE_LIMITED_PASSWORD`. No URL or password is
printed or retained in receipts. Test admission checks the exact native cluster marker before source acquisition.
Fixture-oracle reads and setup work are separate from the source's measured query accounting. The checker case
uses the existing native admission reader (setup plus one nonce/database read) and the existing fixture admin
reader retained across capture (setup, job map and two counter reads: four dispatches). The latter keeps its
one-second statement limit, eight-call cap and the existing 20-second fixture-case deadline; the production source
still has its five-second total limit. The source receipt contains eight dispatches: one session setup and seven native acquisition calls,
including one bounded read of the existing unpopulated continuous-aggregate catalog. The source's eight-call cap and 256-row state cap plus one lookahead remain
unchanged. The aggregate read is capped at 64 rows plus one lookahead within the same total row/byte budget. It
selects only public catalog names and settings; it never reads application rows or calls a refresh, watermark or
size function. The catalog does not measure freshness or bind historical runs to current objects. Sequence registration and the five additional setup statements belong to the existing supervisor SQL
connection and are outside that source receipt. Passing conformance does not qualify server cost.

The supervisor skips these optional jobs cases in a broad ordinary PostgreSQL selection. Run the installed
Timescale cases through the owned fixture supervisor:

```sh
python3 dev/smoke-providers.py timescale \
  --test culprit-postgres telemetry::native_tests::jobs::compatibility:: \
  --test culprit-postgres telemetry::native_tests::jobs::limits::
```

- `telemetry::native_tests::jobs::compatibility::` — three installed-view, native-state/failure, limited-visibility and non-invoking configuration-check cases.
- `telemetry::native_tests::jobs::limits::` — four row/query/byte, frozen paging and retirement cases.

Each test is individually ignored. The existing supervisor runs the crate library with `--ignored --test-threads=1`
and refuses zero matching tests. Its standard deadline still applies. No new driver, image or test runner is needed.

The optional `telemetry::native_tests::jobs::absent::` case runs separately on a fresh owned PostgreSQL fixture
without Timescale, with database `culprit_smoke` and the same nonce-bound `cluster_name` startup option. It needs no
jobs setup or permission changes. It expects NotApplicable coverage, no manufactured empty success and no SQL
against absent job views. Do not include that case in the installed Timescale selection.

```sh
python3 dev/smoke-providers.py postgres \
  --test culprit-postgres telemetry::native_tests::jobs::absent::
```

The exact new positive selector is:

```sh
python3 dev/smoke-providers.py timescale \
  --test culprit-postgres telemetry::native_tests::jobs::compatibility::configured_validation_function_is_recorded_without_invocation_during_collection
```
