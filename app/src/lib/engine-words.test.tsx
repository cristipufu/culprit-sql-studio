import { test } from "bun:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import demo from "../../../fixtures/pg-demo.report.json";
import type { Engine } from "../gen/Engine";
import type { Report } from "../gen/Report";
import { QueryCostsPanel } from "../components/BeyondTime";
import { PlanCatalog } from "../components/PlanCatalog";
import { RuntimeSpread } from "../components/RuntimeSpread";
import { engineLimitations, refusal } from "./engines";
import { fromRust } from "../test/fixtures";

function check(ok: boolean, message: string) {
  if (!ok) throw new Error(message);
}

/** The query costs panel for `engine`, with a finding of `check` on the query so its row leads with its takeaway. */
function costs(engine: Engine, product: string, finding: string | null): string {
  const report = fromRust<Report>(demo);
  report.snapshot.server.engine = engine;
  report.snapshot.server.product = product;
  const query = report.snapshot.workload!.queries[0]!;
  query.text = "UPDATE public.orders SET status = 'paid'";
  query.rows = 1000;
  query.costs = {
    plan_ms: 7000, plans: 214, failed: 1, aborted: null, aborted_mean_ms: null, warnings: null,
    top_error: null, jit_ms: null, jit_functions: null, log_bytes: 2_000_000_000, log_share: 0.4,
    log_full_pages: null, row_bytes: null, compile_history: null,
  };
  report.findings = finding ? [{ ...report.findings[0]!, check: finding, severity: "high" }] : [];
  return renderToStaticMarkup(createElement(QueryCostsPanel, { report, query }));
}

// What the app says from each engine's facts, word for word in English.
test("Query costs say each engine's planning step, log and notes in English", () => {
  const pg = costs("postgres", "PostgreSQL 18.6", "planning-time");
  check(pg.includes('<span class="beyond-label">Planning</span>'), pg);
  check(pg.includes('<p class="beyond-lead bad">Planning takes 9% of its time.</p>'), pg);
  check(pg.includes('<span class="beyond-label">WAL written</span>'), pg);
  check(pg.includes('<span class="beyond-note">40% of all WAL · 1.9 MB per row</span>'), pg);
  check(pg.includes('<p class="beyond-foot">PostgreSQL keeps no error count or lock time per statement; Live shows blocking as it happens.</p>'), pg);
  const pgLog = costs("postgres", "PostgreSQL 18.6", "log-volume");
  check(pgLog.includes('<p class="beyond-lead bad">It writes 1.9 MB of WAL per row it changes.</p>'), pgLog);
  const ms = costs("mssql", "SQL Server 2025", "planning-time");
  check(ms.includes('<span class="beyond-label">Compiling</span>'), ms);
  check(ms.includes('<p class="beyond-lead bad">Compiling takes 9% of its time.</p>'), ms);
  check(ms.includes('<span class="beyond-label">Log written</span>'), ms);
  check(ms.includes('<span class="beyond-note">40% of all log · 1.9 MB per row</span>'), ms);
  check(ms.includes('<p class="beyond-foot">Query Store counts errors but doesn&#x27;t keep them; the finding shows how to capture them.</p>'), ms);
  const maria = costs("mysql", "MariaDB 11.8.9", null);
  check(maria.includes('<p class="beyond-foot">MariaDB keeps no planning time or redo bytes per statement.</p>'), maria);
});

test("A small log share reads as the log's share in each engine's words", () => {
  for (const [engine, product, lead] of [
    ["postgres", "PostgreSQL 18.6", "It writes 40% of the WAL."],
    ["mssql", "SQL Server 2025", "It writes 40% of the log."],
  ] as const) {
    const report = fromRust<Report>(demo);
    report.snapshot.server.engine = engine;
    report.snapshot.server.product = product;
    const query = report.snapshot.workload!.queries[0]!;
    query.text = "UPDATE public.orders SET status = 'paid'";
    query.rows = null;
    query.costs = {
      plan_ms: null, plans: null, failed: null, aborted: null, aborted_mean_ms: null, warnings: null,
      top_error: null, jit_ms: null, jit_functions: null, log_bytes: 2_000_000, log_share: 0.4,
      log_full_pages: null, row_bytes: null, compile_history: null,
    };
    report.findings = [{ ...report.findings[0]!, check: "log-volume", severity: "high" }];
    const html = renderToStaticMarkup(createElement(QueryCostsPanel, { report, query }));
    check(html.includes(`<p class="beyond-lead bad">${lead}</p>`), html);
  }
});

test("Run-time notes, plan formats, limitations and refusals read in English", () => {
  const report = fromRust<Report>(demo);
  const query = report.snapshot.workload!.queries[0]!;
  query.plans = [];
  query.spread = null;
  const spread = (engine: Engine, product: string, kind: "statement-stats" | "plan-cache" | "query-store") =>
    renderToStaticMarkup(createElement(RuntimeSpread, { query, engine, product, windowed: false, kind }));
  const pg = spread("postgres", "PostgreSQL 18.6", "statement-stats");
  check(pg.includes("PostgreSQL keeps one fastest, average, slowest and spread per statement: enough to see a long tail, not which values caused it."), pg);
  const cache = spread("mssql", "SQL Server 2025", "plan-cache");
  check(cache.includes("The plan cache keeps the fastest and slowest run of each cached plan since it was cached, not their spread: Query Store would add it."), cache);

  const catalog = renderToStaticMarkup(createElement(PlanCatalog, { onSample: () => {} }));
  for (const format of [
    ".sqlplan or showplan XML (SSMS, Query Store)",
    "EXPLAIN or EXPLAIN ANALYZE, as text or FORMAT JSON",
    "PostgreSQL EXPLAIN, including hypertable and chunk operators",
    "EXPLAIN (ANALYZE, DIST, FORMAT JSON), including DocDB storage metrics",
    "EXPLAIN FORMAT=JSON, EXPLAIN ANALYZE or FORMAT=TREE",
    "EXPLAIN FORMAT=JSON or ANALYZE FORMAT=JSON",
    "EXPLAIN QUERY PLAN, as the sqlite3 shell prints it",
  ]) check(catalog.includes(`<span class="muted">${format}</span>`), format);

  check(engineLimitations("sqlite")[3] === "A WAL file can retain allocated space after checkpointing. Its size or growth alone does not establish checkpoint starvation or identify a reader holding a snapshot.", "SQLite's limitations");
  const busy = refusal("sqlite", "sessions");
  check(busy.message === "SQLite has no server, so no sessions to show", busy.message);
  check(busy.hint === "A writer that has to wait gets SQLITE_BUSY in the application: its logs say when.", String(busy.hint));
  const plan = refusal("postgres", "last_actual_plan");
  check(plan.message === "PostgreSQL doesn't keep the actual plan of past runs", plan.message);
  check(refusal("mysql", "operator_progress").hint === "Its plan doesn't change while it runs: ask for the plan once.", "MySQL's progress hint");
  check(refusal("sqlite", "sign_in").hint === null, "SQLite's sign-in has no hint");
});
