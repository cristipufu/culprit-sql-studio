# Owned maintenance wait cases

These ignored cases exercise the existing PostgreSQL Activity and retained maintenance-progress sources. They do
not qualify their production cost, enumerate all maintenance, or add routine source queries.

Run them through the owned provider supervisor with the explicit `maintenance_waits::` selection.
The supervisor supplies an ephemeral loopback port, the existing credentials and `CULPRIT_SMOKE_OWNER`.
The shared admission helper checks the actual database and the exact native
`cluster_name=culprit_owned_<24 lower-case hex owner>` marker before any fixture writes. This is the shared owned
PostgreSQL/Timescale marker, not a production identity.

```sh
python3 dev/smoke-providers.py postgres \
  --test culprit-postgres telemetry::native_tests::maintenance_waits::
```

The two exact ignored selectors are:

- `telemetry::native_tests::maintenance_waits::concurrent_index_wait_is_generation_bound_and_the_reported_locker_remains_unresolved`
- `telemetry::native_tests::maintenance_waits::acknowledged_fixture_completion_does_not_turn_a_missing_native_request_into_completion_evidence`

Each case owns one nonce-qualified schema, a one-row table, a held UPDATE transaction and a concurrent index
command. A bounded fixture-only phase check waits up to three seconds for the documented native writer-wait phase.
The actual source observations then use `collect_once`, the existing readers, canonical exported rows and the
original Activity manifest receipt. The progress source retains its ordinary actor/member/role/process rechecks.
Fixture polling is separate from the source's query counts and is not a production polling policy.

The positive case checks exact native locker counters and the same backend/request generation. The reported
locker PID remains unresolved; no other backend generation or SQL is attached. The negative case releases the
held transaction and separately acknowledges the fixture command's successful completion. A later retained
lookup must still report only `NotObserved`, with no completion claim. The original logical-target capture stays
`UnverifiedOriginalScope` even when the later progress read establishes a physical member.

Cleanup releases the held transaction, waits for the owned index command, cancels it if necessary, and drops only
the owned schema. The async case has a 25-second deadline; native statements and cleanup waits are also bounded.
An unacknowledged control command prevents connection reuse and cannot be reported as confirmed cleanup. A drop
guard aborts any remaining owned task; the supervisor remains responsible for final owned-container containment.

The phase meanings are documented in [PostgreSQL 12 progress reporting](https://www.postgresql.org/docs/12/progress-reporting.html#CREATE-INDEX-PROGRESS-REPORTING)
and [current progress reporting](https://www.postgresql.org/docs/current/progress-reporting.html#CREATE-INDEX-PROGRESS-REPORTING).
No test infers maintenance-caused pressure, a successful production outcome, elapsed wait time or aftermath from
a phase counter or a disappearing row.
