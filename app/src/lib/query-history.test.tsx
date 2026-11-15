import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { QueryHistoryEmpty } from "../components/QueryHistoryEmpty";
import { WorkloadUnlock } from "../components/WorkloadUnlock";
import type { Report } from "../gen/Report";
import type { Tile } from "../gen/Tile";
import type { Workload } from "../gen/Workload";
import { partial } from "../test/fixtures";
import { GROUP_A, POPULATIONS } from "../test/populations";
import { StatisticsTile } from "../views/dashboard/Tiles";
import { language, setLanguage, t } from "./i18n";
import { queryHistoryEmptyState } from "./query-history";

function overview(): Report {
  return partial<Report>({
    snapshot: {
      connection: "shop", workload: null, options: { window_minutes: 60 },
      runtime_populations: structuredClone(POPULATIONS), sources: [],
      server: { engine: "postgres", missing: [{
        capability: "pg-stat-statements", reason: "Native permission failure.",
        action: { sql: "CREATE EXTENSION pg_stat_statements;", detail: "Explicit source instruction." },
      }] },
    },
    summary: { no_workload: "This scan has no query history.", no_workload_capability: "pg-stat-statements" },
  });
}

function inEnglish(check: () => void): void {
  const previous = language();
  setLanguage("en");
  try { check(); } finally { setLanguage(previous); }
}

test("available execution groups ask for a selection across history and unlock surfaces without feature repair", () => inEnglish(() => {
  const report = overview();
  const tile: Tile = {
    key: "statistics", title: "Statistics", status: "unavailable", takeaway: "No query history",
    detail: "Enable query statistics.", fix_sql: "CREATE EXTENSION pg_stat_statements;", metrics: [], items: [], next: null,
  };
  for (const element of [<QueryHistoryEmpty key="empty" report={report} />, <WorkloadUnlock key="unlock" report={report} />, <StatisticsTile key="tile" tile={tile} report={report} />]) {
    const html = renderToStaticMarkup(element);
    expect(html).toContain(t("ui.population_picker.required"));
    expect(html).not.toContain("No query history");
    expect(html).not.toContain("This scan has no query history");
    expect(html).not.toContain("CREATE EXTENSION");
    expect(html).not.toContain("Enable query statistics");
    expect(html).toContain("Native permission failure.");
  }
  expect(renderToStaticMarkup(<QueryHistoryEmpty report={report} />)).toContain(`<h3>${t("ui.population_picker.label")}</h3>`);
}));

test("a selection prompt keeps the exact source failure visible and escaped", () => inEnglish(() => {
  const report = overview();
  report.snapshot.sources = [
    { source: "unrelated-source", state: "unavailable", reason: "Unrelated failure must stay separate." },
    { source: POPULATIONS.source, state: "partial", reason: "Native read failed <img onerror=alert(1)>" },
  ];
  const html = renderToStaticMarkup(<QueryHistoryEmpty report={report} />);
  expect(html).toContain(t("ui.population_picker.required"));
  expect(html).toContain("Native read failed &lt;img");
  expect(html).not.toContain("<img");
  expect(html).not.toContain("Unrelated failure");
}));

test("explicit missing groups retain source failures and their existing explicit action", () => inEnglish(() => {
  const report = overview();
  if (!report.snapshot.options) throw new Error("The fixture has explicit options");
  report.snapshot.options.workload_population = GROUP_A;
  report.summary.no_workload = "The selected group could not be read.";
  const html = renderToStaticMarkup(<QueryHistoryEmpty report={report} />);
  expect(html).toContain("The selected group could not be read.");
  expect(html).not.toContain(t("ui.population_picker.required"));
  const unlock = renderToStaticMarkup(<WorkloadUnlock report={report} />);
  expect(unlock).toContain("Native permission failure.");
  expect(unlock).toContain("CREATE EXTENSION pg_stat_statements;");
}));

test("a group requirement needs a populated partitioned catalog and never replaces an available workload", () => {
  for (const catalog of [undefined, { ...POPULATIONS, partitioned: false }, { ...POPULATIONS, requires_selection: false }, { ...POPULATIONS, populations: [] }]) {
    const report = overview();
    report.snapshot.runtime_populations = catalog;
    expect(queryHistoryEmptyState(report)?.kind).toBe("unavailable");
  }
  const report = overview();
  report.snapshot.workload = partial<Workload>({ population: GROUP_A });
  expect(queryHistoryEmptyState(report)).toBeNull();
  expect(renderToStaticMarkup(<QueryHistoryEmpty report={report} />)).toBe("");
});
