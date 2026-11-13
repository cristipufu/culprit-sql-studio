import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { WaitsView } from "../components/WaitsPanel";
import type { Engine } from "../gen/Engine";
import type { WaitCategory } from "../gen/WaitCategory";
import type { WaitsReport } from "../gen/WaitsReport";
import { partial } from "../test/fixtures";
import { ENGINES } from "./engines";

function recorded(engine: Engine, mean: number | null, executions: number): WaitsReport {
  const category = partial<WaitCategory>({
    key: "lock", label: "Native locks", wait_ms: 6_000, share: 1,
    top_wait: "native-lock", meaning: "The source's recorded native category.", detail: null, attribution: null,
    queries: [{
      id: "query-1", text: "SELECT id FROM orders", wait_ms: 6_000,
      mean_recorded_ms: mean, executions, rows_per_run: null,
    }],
  });
  return { engine, scope: "window", source: "Native source", total_ms: 6_000, categories: [category], notes: [] };
}

function show(report: WaitsReport): string {
  return renderToStaticMarkup(<WaitsView report={report} current={report.categories[0]!} onSelect={() => {}} onClose={() => {}} onSample={() => {}} />);
}

test("parallel accumulated waits keep total and average without an elapsed-share or run-duration claim", () => {
  for (const engine of ENGINES) {
    // The native counter can total 6 s for two executions whose elapsed total was only 1 s.
    const html = show(recorded(engine, 3_000, 2));
    expect(html).toContain("6.0 s recorded");
    expect(html).toContain("3.0 s accumulated per execution on average");
    expect(html).toContain("100% of recorded category totals");
    expect(html).toContain('<span class="wq-time"><b>6.0 s</b></span>');
    expect(html).toContain("2 runs");
    for (const claim of ["of its time", "elapsed", "longest", "slowest", "maximum"]) expect(html).not.toContain(claim);
  }
});

test("unknown execution count shows the native total without inventing an average or zero runs", () => {
  const html = show(recorded("mssql", null, 0));
  expect(html).toContain("6.0 s recorded");
  expect(html).toContain("average per execution unavailable");
  expect(html).not.toContain("6.0 s accumulated per execution");
  expect(html).not.toContain("0 runs");
  expect(html).not.toContain("NaN");
});

test("partial attribution keeps positive totals and its explicit limitation", () => {
  const report = recorded("postgres", 3_000, 2);
  report.notes = ["Native attribution is partial; an absent category is not a measured zero."];
  const html = show(report);
  expect(html).toContain("6.0 s recorded");
  expect(html).toContain(report.notes[0]!);
  expect(html).not.toContain("no lock waiting");
  expect(html).not.toContain("0 ms");
});

test("independent category and query totals never become a query contribution percentage", () => {
  const report = recorded("mssql", 60_000, 2);
  report.total_ms = 17_700;
  const category = report.categories[0]!;
  category.wait_ms = 17_700;
  category.queries[0]!.wait_ms = 120_000;
  const html = show(report);
  expect(html).toContain("17.7 s");
  expect(html).toContain('<span class="wq-time"><b>2 min</b></span>');
  expect(html).toContain("2 min recorded");
  expect(html).toContain("1 min accumulated per execution on average");
  expect(html).not.toContain("wq-share");
  expect(html).not.toContain("accounts for");
});
