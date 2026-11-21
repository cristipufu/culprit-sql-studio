# SQL Server session test client

This helper creates connections for one explicit, local SQL Server test:

- Two active MARS requests on one connection, held by unread results of at most 4,096 rows × 4,096 payload bytes each. MARS means Multiple Active Result Sets.
- A request running in another database on the same owned test instance, held with the same output bound.
- The same connections after their requests finish.

Culprit's Rust collector reads these through its normal session-census path. The test checks native connection IDs, MARS parent/child relationships and request scope before accepting the result. This helper is not included in Culprit's runtime or release bundles.

## Build

Use the SDK pinned in `global.json` and the audited dependencies in `packages.lock.json`. Keep build output outside the checkout:

```sh
# Run from this directory.
actor_build=$(mktemp -d)
cp Program.cs MarsActor.csproj global.json packages.lock.json "$actor_build/"
cd "$actor_build"
dotnet restore --locked-mode
dotnet build --no-restore --output bin
```

## Run

From the repository root, point `CULPRIT_CENSUS_MARS_ACTOR_DLL` at the built `MarsActor.dll` and `CULPRIT_NATIVE_COST_RECEIPT_DIR` at a fresh private directory, then select:

```sh
python3 dev/smoke-providers.py mssql --test culprit-mssql activity::native_tests::census::idle_connections::owned_census_idle_connections_measure_small_medium_and_require_real_mars_and_foreign_request_witnesses
```

The existing supervisor creates and removes the disposable database/container. It supplies the ownership nonce and credentials through the environment. The helper accepts only that nonce-owned loopback target. It emits bounded readiness/completion metadata without credentials.
