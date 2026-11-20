import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { Deadlock } from "../gen/Deadlock";
import type { DeadlockReport } from "../gen/DeadlockReport";
import type { MeasurementScope } from "../gen/MeasurementScope";
import type { Report } from "../gen/Report";
import type { Tile } from "../gen/Tile";
import { partial } from "../test/fixtures";
import { DeadlocksTile } from "../views/dashboard/Tiles";
import { databaseDeadlockGraphCount, DeadlockScopeNotice, DeadlockUnavailableHistory } from "./deadlock-scope";
import { t } from "./i18n";

function graph(): Deadlock {
  return partial<Deadlock>({ id: "original-event", at: "2026-10-11T11:00:00Z", headline: "Original recorded cycle" });
}

function history(scope: MeasurementScope | null, graphs: Deadlock[] = []): DeadlockReport {
  return {
    scope, deadlocks: graphs, source: "pg_stat_database", since_hours: 24,
    counted: null, counted_since: null, note: null, recent_only: false,
  };
}

function overview(evidence: DeadlockReport): Report {
  return partial<Report>({ snapshot: { deadlocks: evidence, taken_at: "2026-10-11T12:00:00Z" } });
}

const unqualified: Array<MeasurementScope | null> = [null, "server", "pool", "node", "cluster", "session", "file", "unknown"];

function tile(): Tile {
  return {
    key: "deadlocks", title: "Deadlocks", status: "good", takeaway: "Stale database count must not survive",
    detail: null, metrics: [], items: [], fix_sql: null, next: null,
  };
}

test("database badges require typed database scope rather than source labels or a nonempty graph population", () => {
  for (const scope of unqualified) {
    for (const graphs of [[], [graph()]]) {
      const evidence = history(scope, graphs);
      expect(databaseDeadlockGraphCount(evidence)).toBeNull();
      expect(evidence.deadlocks).toEqual(graphs);
    }
  }
  expect(databaseDeadlockGraphCount(null)).toBeNull();
  expect(databaseDeadlockGraphCount(undefined)).toBeNull();
  expect(databaseDeadlockGraphCount(history("database"))).toBe(0);
  expect(databaseDeadlockGraphCount(history("database", [graph()]))).toBe(1);
});

test("raw shared and unresolved graphs carry a scope qualification without hiding their history", () => {
  for (const scope of unqualified) {
    const evidence = history(scope, [graph()]);
    const html = renderToStaticMarkup(<DeadlockScopeNotice report={evidence} />);
    expect(html).toContain(t("evidence.deadlocks.database_scope"));
    expect(html).toContain('role="status"');
    if (scope === "server") expect(html).toContain(t("ui.replication.scope.server"));
    expect(evidence.deadlocks[0]!.id).toBe("original-event");
  }
  expect(renderToStaticMarkup(<DeadlockScopeNotice report={history("database")} />)).toBe("");
});

test("empty shared or unresolved history never becomes a green database zero or lends a foreign counter", () => {
  for (const scope of unqualified) {
    for (const counted of [0, 72]) {
      const evidence = history(scope);
      evidence.counted = counted;
      evidence.note = "Source limit <img src=x onerror=alert(1)>";
      const html = renderToStaticMarkup(<DeadlockUnavailableHistory report={evidence} />);
      expect(html).toContain(t("evidence.deadlocks.database_scope"));
      expect(html).toContain("Source limit &lt;img");
      expect(html).not.toContain("<img");
      expect(html).not.toContain("dl-ok");
      expect(html).not.toContain("dl-counter");
      expect(html).not.toContain(t("ui.deadlocks.none_in_day"));
      expect(html).not.toContain(t("ui.deadlocks.none_in_week"));
      expect(html).not.toContain(t("ui.deadlocks.none_since_reset"));
    }
  }
});

test("dashboard refuses stale per-database words and raw timelines for unqualified history but keeps history navigation", () => {
  for (const scope of unqualified) {
    const evidence = history(scope, [graph()]);
    evidence.counted = 72;
    const html = renderToStaticMarkup(<DeadlocksTile tile={tile()} report={overview(evidence)} />);
    expect(html).toContain(t("evidence.deadlocks.database_scope"));
    expect(html).toContain("t-deadlocks unavailable");
    expect(html).toContain(t("ui.dashboard.action.deadlocks"));
    expect(html).not.toContain("Stale database count");
    expect(html).not.toContain("dash-ticks");
    expect(html).not.toContain("dash-big");
    expect(evidence.deadlocks[0]!.id).toBe("original-event");
  }
});

test("qualified database graph history and native counted zero retain their separate dashboard projections", () => {
  const evidence = history("database", [graph()]);
  const html = renderToStaticMarkup(<DeadlocksTile tile={tile()} report={overview(evidence)} />);
  expect(html).toContain("dash-ticks");
  expect(html).toContain("Original recorded cycle");
  expect(html).not.toContain(t("evidence.deadlocks.database_scope"));
  evidence.deadlocks = [];
  evidence.counted = 0;
  const zero = renderToStaticMarkup(<DeadlocksTile tile={tile()} report={overview(evidence)} />);
  expect(zero).toContain("dash-big");
  expect(zero).toContain("<b>0</b>");
});

test("missing deadlock evidence cannot retain a stale healthy dashboard assessment", () => {
  const report = partial<Report>({ snapshot: { deadlocks: null, taken_at: "2026-10-11T12:00:00Z" } });
  const html = renderToStaticMarkup(<DeadlocksTile tile={tile()} report={report} />);
  expect(html).toContain(t("evidence.deadlocks.database_scope"));
  expect(html).toContain("t-deadlocks unavailable");
  expect(html).toContain(t("ui.dashboard.action.deadlocks"));
  expect(html).not.toContain("Stale database count");
  expect(html).not.toContain("dash-ticks");
  expect(html).not.toContain("dash-big");
});
