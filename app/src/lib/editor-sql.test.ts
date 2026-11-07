import { test } from "bun:test";
import {
  canOpenFixInEditor,
  canOpenQueryInEditor,
  countPlaceholders,
  locateFindingSpan,
  prepareQueryForEditor,
  splitSqlStatements,
  statementEndOffset,
} from "./editor-sql";
import type { Finding } from "../gen/Finding";
import type { QueryStat } from "../gen/QueryStat";

test("SQL editor: statement splitting, placeholders, finding spans and editor handoff", () => {
  function assert(cond: unknown, msg: string): void {
    if (!cond) throw new Error(msg);
  }

  // 1. Multi-statement splitting: lock-escalations Extended Events session + SELECT + commented WHILE template
  const escalationScript = `-- Example only: check lock footprint and required atomicity before batching
-- WHILE 1 = 1
-- BEGIN
--     UPDATE TOP (1000) dbo.orders SET /* the same changes */ WHERE /* the same filter, leaving out rows already changed */;
--     IF @@ROWCOUNT = 0 BREAK;
-- END;

-- SQL Server doesn't record which statement escalated: this session keeps the next ones with their statement.
IF NOT EXISTS (SELECT 1 FROM sys.database_event_sessions WHERE name = N'culprit_lock_escalation')
    CREATE EVENT SESSION culprit_lock_escalation ON DATABASE
    ADD EVENT sqlserver.lock_escalation (
        SET collect_statement = 1
        ACTION (sqlserver.sql_text, sqlserver.client_app_name))
    ADD TARGET package0.ring_buffer;
IF NOT EXISTS (SELECT 1 FROM sys.dm_xe_database_sessions WHERE name = N'culprit_lock_escalation')
    ALTER EVENT SESSION culprit_lock_escalation ON DATABASE STATE = START;

-- Then read what it caught
SELECT e.x.value('@timestamp', 'datetime2') AS at_utc
  FROM sys.dm_xe_database_session_targets t;

-- Once it has caught what you need, stop and remove it: DROP EVENT SESSION culprit_lock_escalation ON DATABASE;`;

  const stmts = splitSqlStatements(escalationScript);
  assert(stmts.length === 3, `expected 3 statements, got ${stmts.length}`);
  assert(stmts[0]!.includes("CREATE EVENT SESSION culprit_lock_escalation"), "missing CREATE EVENT SESSION");
  assert(stmts[1]!.includes("ALTER EVENT SESSION culprit_lock_escalation"), "missing ALTER EVENT SESSION");
  assert(stmts[2]!.startsWith("-- Then read what it caught\nSELECT e.x.value"), "missing SELECT");

  // 2. Parameterized write and 85-parameter IN list should NOT offer "Open in editor"
  const bulkDelete = `delete from [dbo].[orders] where [dbo].[orders].[id] in (${Array.from({ length: 85 }, (_, i) => `@par${i}`).join(", ")})`;
  assert(countPlaceholders(bulkDelete) === 85, "expected 85 placeholders");
  assert(canOpenQueryInEditor(bulkDelete) === false, "85-param DELETE should not offer Open in editor");
  assert(canOpenQueryInEditor("UPDATE [dbo].[orders] SET [status] = @par0 WHERE [id] = @par1") === false, "parameterized UPDATE should not offer Open in editor");
  assert(canOpenQueryInEditor("(@P1 int, @P2 int) DELETE FROM [dbo].[orders] WHERE [id] = @P1 AND [customer_id] = @P2") === false, "plan-cache parameterized DELETE with leading (@P1...) should not offer Open in editor");
  assert(canOpenQueryInEditor("SELECT [id], [status] FROM [dbo].[orders] WHERE [customer_id] = @par0 AND [status] = @par1") === true, "2-param SELECT should offer Open in editor");

  // 3. Preparing a SQL Server SELECT with compiled parameters substitutes the compiled values
  const dummyQuery: QueryStat = {
    id: "36794",
    query_hash: null,
    text: "SELECT [id], [status] FROM [dbo].[orders] WHERE [customer_id] = @par0 AND [status] = @par1",
    parameters: "@par0 nvarchar(4000), @par1 int",
    object_name: null,
    executions: 10,
    total_time_ms: 100,
    mean_time_ms: 10,
    max_time_ms: 20,
    stddev_time_ms: null,
    cpu_time_ms: 50,
    rows: 10,
    logical_reads: 100,
    physical_reads: 0,
    temp_writes: 0,
    first_seen: null,
    last_seen: null,
    plans: [{
      id: "1",
      executions: 10,
      total_time_ms: 100,
      mean_time_ms: 10,
      first_seen: null,
      last_seen: null,
      is_forced: false,
      min_time_ms: 5,
      max_time_ms: 20,
      stddev_time_ms: null,
      compiled: "@par0 = N'tenant-1', @par1 = (42)",
      variant_query: null,
      cpu_time_ms: 50,
      logical_reads: 100,
    }],
    plan: null,
    waits: [],
    trend: null,
    spread: null,
    costs: null,
    rows_affected: null,
    plan_id: "1",
    requests: null,
    resolved_write_target: null,
  };

  const prepared = prepareQueryForEditor(dummyQuery, "mssql");
  assert(prepared.includes("WHERE [customer_id] = N'tenant-1' AND [status] = 42"), "expected compiled values to be substituted");

  // 4. Non-runnable fix.sql templates, hints, my.cnf snippets, and SQLite writes
  assert(canOpenFixInEditor("-- In the server configuration (my.cnf), then restart:\n[mysqld]\nperformance_schema = ON", "mysql") === false, "my.cnf-only snippet should not offer Open in editor");
  const myCnfPlusTruncate = `-- In the server configuration (my.cnf), then restart:
[mysqld]
performance_schema_digests_size = 50000
-- or start over now:
TRUNCATE TABLE performance_schema.events_statements_summary_by_digest;`;
  assert(canOpenFixInEditor(myCnfPlusTruncate, "mysql") === true, "my.cnf + TRUNCATE should offer Open in editor");
  assert(splitSqlStatements(myCnfPlusTruncate).length === 1, "my.cnf block should be stripped leaving TRUNCATE");
  assert(splitSqlStatements(myCnfPlusTruncate)[0]!.includes("TRUNCATE TABLE"), "expected TRUNCATE TABLE statement");

  assert(canOpenFixInEditor("OPTION (MAX_GRANT_PERCENT = 5);", "mssql") === false, "bare OPTION hint should not offer Open in editor");
  assert(canOpenFixInEditor("SELECT … FROM … WITH (UPDLOCK, ROWLOCK) WHERE …;", "mssql") === false, "ellipsis deadlock template should not offer Open in editor");
  assert(canOpenFixInEditor("ALTER ROLE <app_role> SET jit = off;", "postgres") === false, "unedited <app_role> template should not offer Open in editor");
  assert(canOpenFixInEditor("-- 1. append OPTION (RECOMPILE)", "mssql") === false, "comment-only fix should not offer Open in editor");

  const tempSpillFix = `-- For the role that runs this query (persists for new connections)
ALTER ROLE <app_role> SET work_mem = '64MB';

-- Or inside the transaction around this query only
SET LOCAL work_mem = '64MB';`;
  assert(canOpenFixInEditor(tempSpillFix, "postgres") === true, "mixed <app_role> + SET LOCAL should offer Open in editor");
  assert(splitSqlStatements(tempSpillFix).length === 1 && splitSqlStatements(tempSpillFix)[0]!.includes("SET LOCAL work_mem"), "unedited <app_role> statement should be skipped when running");

  assert(canOpenFixInEditor("CREATE INDEX ix_orders_status ON orders (status);", "sqlite") === false, "SQLite DDL should not offer Open in editor on read-only connection");
  assert(canOpenFixInEditor("CREATE INDEX ix_orders_status ON orders (status);", "postgres") === true, "Postgres DDL should offer Open in editor");

  // 5. T-SQL GO batch separator support
  const goBatch = `SELECT 1 AS a\nGO\nSELECT 2 AS b\nGO`;
  assert(splitSqlStatements(goBatch).length === 2, "GO lines should split statements");

  // 6. Lint finding statement-span matching and semicolon end-offset scanning
  function makeFinding(summary: string, subjectName = "public.orders"): Pick<Finding, "summary" | "subject"> {
    return {
      summary,
      subject: { kind: "table", name: subjectName },
    };
  }

  const migrationScript = `-- Migration header with a; semicolon in comment
CREATE FUNCTION noop() RETURNS void AS $body$
BEGIN
  PERFORM 1;
END;
$body$ LANGUAGE plpgsql;

/* Block comment before the locking statement; */
CREATE INDEX idx_orders_customer
  ON public.orders (customer_id);
`;

  const indexFinding = makeFinding(
    "The statement `CREATE INDEX idx_orders_customer ON public.orders (customer_id)` builds an index without `CONCURRENTLY`.",
    "public.orders",
  );
  const span = locateFindingSpan(migrationScript, indexFinding);
  assert(span !== null, "expected locateFindingSpan to find CREATE INDEX statement");
  assert(
    migrationScript.slice(span![0], span![1]) === "CREATE INDEX idx_orders_customer",
    `expected first line of CREATE INDEX, got ${JSON.stringify(migrationScript.slice(span![0], span![1]))}`,
  );

  const stmtEnd = statementEndOffset(migrationScript, span![0], span![1]);
  assert(
    migrationScript.slice(span![0], stmtEnd) === "CREATE INDEX idx_orders_customer\n  ON public.orders (customer_id);",
    `expected full CREATE INDEX statement up to semicolon, got ${JSON.stringify(migrationScript.slice(span![0], stmtEnd))}`,
  );

  // Fallback to subject leaf name when summary has no backtick-quoted statement
  const subjectFallbackFinding = makeFinding("Table has an unindexed foreign key.", "public.orders");
  const fallbackSpan = locateFindingSpan("SELECT 1;\nALTER TABLE public.orders\n  ADD CONSTRAINT fk_cust FOREIGN KEY (customer_id) REFERENCES customers(id);", subjectFallbackFinding);
  assert(fallbackSpan !== null, "expected subject fallback to locate ALTER TABLE statement");
  assert(
    "SELECT 1;\nALTER TABLE public.orders\n  ADD CONSTRAINT fk_cust FOREIGN KEY (customer_id) REFERENCES customers(id);".slice(
      fallbackSpan![0],
      fallbackSpan![1],
    ) === "ALTER TABLE public.orders",
    "expected subject fallback to highlight first line of containing statement",
  );

  // T-SQL GO batch boundaries in locateFindingSpan and statementEndOffset
  const tsqlGoScript = `CREATE TABLE dbo.audit_log (id int)\nGO\nALTER TABLE dbo.orders\n  ADD status nvarchar(32)\n-- trailing comment before GO\nGO\nSELECT 1`;
  const tsqlSpan = locateFindingSpan(tsqlGoScript, subjectFallbackFinding);
  assert(tsqlSpan !== null, "expected locateFindingSpan to find statement after GO");
  assert(
    tsqlGoScript.slice(tsqlSpan![0], tsqlSpan![1]) === "ALTER TABLE dbo.orders",
    `expected ALTER TABLE dbo.orders after GO, got ${JSON.stringify(tsqlGoScript.slice(tsqlSpan![0], tsqlSpan![1]))}`,
  );
  const tsqlEnd = statementEndOffset(tsqlGoScript, tsqlSpan![0], tsqlSpan![1]);
  assert(
    tsqlGoScript.slice(tsqlSpan![0], tsqlEnd) === "ALTER TABLE dbo.orders\n  ADD status nvarchar(32)",
    `expected statementEndOffset to stop at last token before GO, got ${JSON.stringify(tsqlGoScript.slice(tsqlSpan![0], tsqlEnd))}`,
  );

  // Multi-line statement without a trailing semicolon at EOF, plus T-SQL [[escaped]] brackets and Unicode İ
  const noSemicolonScript = `SELECT 'İ' AS [col]]1;still_bracket];\nCREATE INDEX idx_orders_customer\n  ON public.orders (customer_id)\n  -- trailing comment`;
  assert(splitSqlStatements(noSemicolonScript).length === 2, "escaped T-SQL bracket ]] should not close bracket before ;");
  const noSemiSpan = locateFindingSpan(noSemicolonScript, indexFinding);
  assert(noSemiSpan !== null, "expected locateFindingSpan to locate statement after Unicode İ and [[bracket]]");
  assert(
    noSemicolonScript.slice(noSemiSpan![0], noSemiSpan![1]) === "CREATE INDEX idx_orders_customer",
    "expected first line of CREATE INDEX after Unicode İ",
  );
  const noSemiEnd = statementEndOffset(noSemicolonScript, noSemiSpan![0], noSemiSpan![1]);
  assert(
    noSemicolonScript.slice(noSemiSpan![0], noSemiEnd) === "CREATE INDEX idx_orders_customer\n  ON public.orders (customer_id)",
    `expected unterminated multi-line statement end to cover all tokens, got ${JSON.stringify(noSemicolonScript.slice(noSemiSpan![0], noSemiEnd))}`,
  );
});
