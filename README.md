<p align="center">
  <img src="docs/images/logo.svg" width="72" height="72" alt="Culprit SQL Studio">
</p>

<h1 align="center">Culprit SQL Studio</h1>

<p align="center">
  <strong>Find database bottlenecks, with the evidence to fix them.</strong><br>
  A desktop app and CLI for database performance analysis on macOS, Linux and Windows.
</p>

<p align="center">
  <img alt="SQL Server" src="https://img.shields.io/badge/SQL%20Server-CC2927?style=flat-square&logo=microsoftsqlserver&logoColor=white">
  <img alt="PostgreSQL" src="https://img.shields.io/badge/PostgreSQL-336791?style=flat-square&logo=postgresql&logoColor=white">
  <img alt="TimescaleDB" src="https://img.shields.io/badge/TimescaleDB-F4CE41?style=flat-square">
  <img alt="YugabyteDB" src="https://img.shields.io/badge/YugabyteDB-C94B25?style=flat-square">
  <img alt="MySQL" src="https://img.shields.io/badge/MySQL-00758F?style=flat-square&logo=mysql&logoColor=white">
  <img alt="MariaDB" src="https://img.shields.io/badge/MariaDB-003545?style=flat-square&logo=mariadb&logoColor=white">
  <img alt="SQLite" src="https://img.shields.io/badge/SQLite-003B57?style=flat-square&logo=sqlite&logoColor=white">
  <br>
  <img alt="MCP server" src="https://img.shields.io/badge/MCP-ready-1b1a18?style=flat-square">
  <img alt="macOS, Linux and Windows" src="https://img.shields.io/badge/macOS%20%C2%B7%20Linux%20%C2%B7%20Windows-1b1a18?style=flat-square">
  <a href="https://github.com/cristipufu/culprit-sql-studio/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/cristipufu/culprit-sql-studio/actions/workflows/ci.yml/badge.svg"></a>
  <a href="LICENSE"><img alt="MIT license" src="https://img.shields.io/badge/license-MIT-ffcf33?style=flat-square"></a>
</p>

<p align="center">
  <a href="https://github.com/cristipufu/culprit-sql-studio/releases/latest"><strong>Download</strong></a> ·
  <a href="#get-started">Quick start</a> ·
  <a href="#what-you-get">Features</a> ·
  <a href="#supported-databases">Providers</a> ·
  <a href="#coding-agents-and-ci">Agents &amp; CI</a> ·
  <a href="docs/guide.md">Guide</a> ·
  <a href="https://culpritsql.com">Website</a>
</p>

Culprit brings query statistics, execution plans, waits and schema context into one workflow. It ranks findings by impact and connects each one to evidence, a suggested fix and verification steps. Use it from the desktop app, terminal or a coding agent.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/dashboard-dark.png">
  <img alt="Database dashboard with a timeline, ranked findings, expensive queries, waits and current activity" src="docs/images/dashboard-light.png">
</picture>

## Get started

[Download the desktop app or standalone CLI](https://github.com/cristipufu/culprit-sql-studio/releases/latest). In the app, add a connection or open a SQLite file to see its dashboard.

From the terminal:

```sh
culprit conn add shop --env staging --url 'postgres://app@db.example.com:5432/shop'
culprit scan shop
culprit dashboard shop
```

Reopening a connection shows its saved report while a fresh scan runs in the background. The desktop app offers signed updates; update the standalone CLI with `culprit update`.

<details>
<summary>Try the interface with demo data</summary>

With [Bun](https://bun.sh) installed:

```sh
git clone https://github.com/cristipufu/culprit-sql-studio.git
cd culprit-sql-studio/app
bun install --frozen-lockfile
bun run dev
```

The browser demo uses included snapshots and needs no database connection.

</details>

## Supported databases

| Provider | Sources and capabilities |
|---|---|
| [**SQL Server / Azure SQL**](docs/providers/mssql.md) | Query Store, plan cache, DMVs and retained deadlock events |
| [**PostgreSQL / Amazon RDS**](docs/providers/postgres.md) | `pg_stat_statements`, catalog/statistics views, EXPLAIN and hypothetical indexes through HypoPG |
| [**TimescaleDB**](docs/providers/timescale.md) | PostgreSQL sources plus hypertable sizes and managed/compressed chunks |
| [**YugabyteDB**](docs/providers/yugabyte.md) | Native YSQL/DocDB counters, DIST plans, sharding and scoped activity sources |
| [**MySQL**](docs/providers/mysql.md) | `performance_schema`, `sys`, EXPLAIN and the slow query log |
| [**MariaDB**](docs/providers/mariadb.md) | Performance schema, slow log, user statistics, MariaDB plans and lock views |
| [**SQLite**](docs/providers/sqlite.md) | Read-only file inspection, `EXPLAIN QUERY PLAN` and framework query logs |

Features depend on the engine, enabled sources and your login’s permissions. Missing sources appear with a reason and setup guidance. See [Engine parity](docs/parity.md) for the feature comparison and verified versions.

## What you get

### Findings with evidence, fixes and verification

Each finding identifies the affected queries, the measurement window and the evidence behind its severity. Open it for a suggested change, SQL to copy when applicable, verification steps and a brief to share with a teammate or coding agent.

| Area | Examples of what Culprit checks |
|---|---|
| Query performance | Expensive joins, plan regressions, parameter-sensitive plans, index-blocking type conversions, N+1 patterns, failures, timeouts and spills |
| Indexes and schema | Missing, unused, duplicate and overlapping indexes; unindexed foreign keys; partition hazards |
| Maintenance and capacity | Stale statistics, dead tuples, table bloat, large rows, sequence exhaustion and transaction-ID wraparound risk |
| Database health | Hot rows, table-lock escalation, storage latency, page checksum corruption, connection churn, replication lag and settings |

For example, the SQL Server demo includes a plan making **1.5 billion row comparisons**. The affected query accounts for **27% of database time** over five hours. Culprit identifies its forced loop-join hint, suggests removing it, and gives steps to compare the new plan and verify the result.

Run `culprit checks` for the complete check list and required capabilities.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/findings-dark.png">
  <img alt="A finding with query evidence, a relevant plan step, a suggested fix and verification steps" src="docs/images/findings-light.png">
</picture>

### Execution plans and runtime evidence

Explore plans as a diagram, an indented tree or an operations table. Insights explain expensive lookups, row-estimate errors, inefficient joins, spills, conversions, memory grants and parallelism, with links to the relevant operators.

Inspect estimated and actual rows, operator properties and raw XML or JSON. Measure supported read statements explicitly. On SQL Server, inspect the retained last actual plan without re-running a query, or watch a running query’s operator progress when that evidence is available.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/queries-dark.png">
  <img alt="Queries ranked by database time, with a selected query’s execution plan, insights and operator details" src="docs/images/queries-light.png">
</picture>

### What changed after a deploy

Drag across the database-time chart to compare two periods: queries that got slower, new queries, improvements and statements that stopped running. See plan switches, schema or setting changes, restarts and deploy marks alongside those changes.

Cloud metrics add context such as Amazon RDS CPU credits, I/O limits and Azure tier limits, using your existing `aws` or `az` sign-in.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/what-changed-dark.png">
  <img alt="A period comparison showing slower, new and improved queries with changes in database time and execution counts" src="docs/images/what-changed-light.png">
</picture>

### Compare and share plans

Paste, drop or open a plan from another tool: SQL Server `.sqlplan`, PostgreSQL EXPLAIN text or JSON, YugabyteDB DIST output, or MySQL/MariaDB plans. Compare two versions with synchronized diagrams and highlighted changes in operators, access paths, row estimates and cost.

Comparisons also explain differences in captured parameters, statistics, parallelism and memory where the plans retain them. Share findings and plans as Markdown, a self-contained HTML page or a native plan file, with literal-value redaction enabled by default.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/compare-dark.png">
  <img alt="Two plans side by side with an explanation of their differences and highlighted changed operators" src="docs/images/compare-light.png">
</picture>

### Live sessions, blocking and deadlocks

Follow blocking chains back to the session holding them up. Inspect waits, long-running statements and transactions left open by the application. Recorded deadlocks include the participating statements, the pattern and suggested next steps.

Live activity refreshes automatically and can be paused. On non-production connections, cancel a statement or end a session from the app or CLI.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/live-dark.png">
  <img alt="Live sessions with an open transaction and the blocked sessions waiting behind it" src="docs/images/live-light.png">
</picture>

### SQL editing and database tooling

| Feature | What it includes |
|---|---|
| **Index advisor** | Workload-based index candidates, duplicate/overlapping/unused indexes, and the write cost of keeping them |
| **SQL editor** | Persistent tabs, schema-aware completion, saved queries, estimated and measured plans, and inline migration warnings with quick fixes |
| **Migration linter** | Offline checks against saved snapshots; supported DDL rewrites for concurrent or online operations |
| **Where time goes** | Wait categories connected to queries, plus a live sample to compare with the recorded workload |
| **Health signals** | Memory, compiles, connections, row locks, per-file storage latency, replication and cloud limits |
| **Schema explorer** | Table sizes, indexes and usage; compare columns, indexes, foreign keys, triggers, constraints and partition bounds across environments, with SQL to reconcile differences |
| **Connections** | Connection strings and imports from Azure Data Studio, VS Code, DBeaver and DataGrip |
| **Discovery and SSH** | Find Azure and AWS databases with your CLI login; connect through an [SSH jump host](docs/ssh.md) |
| **Offline reports** | Saved scans, trends, historical comparisons and shared snapshots you can inspect without a database connection |
| **Keyboard navigation** | A command palette (⌘K / Ctrl+K), view shortcuts, query tabs and actions |
| **Languages** | English, German, Spanish and French for the interface and human-readable CLI output |

## Coding agents and CI

### MCP and repository setup

The MCP server gives Claude Code, Cursor, Codex and other MCP clients access to findings, query plans, schema, migrations, blocking, waits, index advice and verification. Agents use saved connection names; credentials stay in Culprit.

```sh
claude mcp add culprit -- culprit mcp
culprit agent init --connection shop
```

The second command adds repository instructions and MCP configuration. Other clients can register `culprit` with the argument `mcp` directly. When query comments contain `file:line` references, including Marginalia and SqlCommenter annotations, finding briefs can point back to the application code.

Print a finding’s brief or test a proposed PostgreSQL index through HypoPG without creating the real index:

```sh
culprit show shop 'filtered-scan:public.orders'
culprit explain shop --sql "SELECT * FROM orders WHERE status = 'open'" \
  --with-index "CREATE INDEX ON orders (status ASC)"
```

Access is set per connection: off, statistics and estimated plans (the default), or approved read-only queries. Query approval shows the exact statement; tool calls are recorded in a local audit log.

<p align="center">
<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/images/agent-approval-dark.png">
  <img alt="An agent query approval showing the exact SQL, connection, row and time limits, and controls to deny or run once" src="docs/images/agent-approval-light.png" width="720">
</picture>
</p>

[Agent setup, permissions and audit details →](docs/agents.md)

### Migration checks and deploy workflows

Check migration files against a saved snapshot’s table sizes, column types, indexes and recorded workload. Findings cover blocking `CREATE INDEX` and `ALTER TABLE` operations, missing or mismatched foreign-key indexes, and `DROP` or `RENAME` changes that affect recorded queries.

```sh
culprit lint-sql prod-snapshot.json --file migrations/ --fail-on high
culprit lint-sql prod-snapshot.json --file migrations/ --format sarif > culprit.sarif
```

Use `--fail-on high` to fail CI on high-severity findings. `--write` applies supported rewrites to migration files. JSON supports scripts, SARIF supports code-review tooling, and stable finding IDs let you track the same problem across scans.

After deployment, mark the timeline, verify a finding or generate a redacted summary:

```sh
culprit mark shop deploy v2.4.0
culprit verify shop 'filtered-scan:public.orders'
culprit digest shop > digest.json
```

## Production use

- **Scans read statistics and catalog views**, with provider-specific timeouts and planning guards.
- **Database fixes are suggestions** for you to review and apply.
- **Production execution requires confirmation** for statements Culprit cannot classify as reads; destructive operations require an additional confirmation.
- **Agent query access is opt-in**, with approval for each read-only statement and a local audit trail.
- **Saved passwords use the OS keychain.** Entra ID can use your Azure sign-in; shareable exports redact literal values by default.

Use a database login with the permissions your workflow needs. The [safety guide](docs/guide.md#safety) and [provider documentation](docs/parity.md) explain the collection, planning and execution boundaries.

## Development

Culprit is built with Rust, Tauri and React. The app, CLI and MCP server share one analysis engine. Add custom checks or database providers through the Rust API without forking; [language catalogs](docs/localization.md) supply the user-facing text.

<details>
<summary>Build from source</summary>

Install [Rust stable](https://rustup.rs) and [Bun](https://bun.sh). Linux also needs the [platform dependencies](docs/guide.md#platforms).

```sh
git clone https://github.com/cristipufu/culprit-sql-studio.git
cd culprit-sql-studio

# CLI
cargo build --locked --release -p culprit-cli

# Desktop app
cd app
bun install --frozen-lockfile
bunx tauri build
```

</details>

## Learn more

- **[Guide](docs/guide.md):** every check and command, the agent setup, measured speed, safety details and how to extend Culprit
- **[Architecture](docs/ARCHITECTURE.md):** the analysis pipeline, the evidence rules every check follows and the crate map
- **[Design](docs/DESIGN.md):** how the product behaves and why
- **[Coding agents](docs/agents.md):** MCP tools, permissions, approvals and the audit log
- **[Engine parity](docs/parity.md):** feature comparison, provider guides, setup and verified versions

Culprit is open source under the [MIT license](LICENSE).

<br>

<p align="center">
  <img src="docs/images/logo.svg" width="28" height="28" alt=""><br>
  <sub>Made for the people who get paged when the database is slow.</sub>
</p>
