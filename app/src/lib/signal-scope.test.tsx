import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { HealthTab, SignalTile } from "../components/Health";
import type { Pressure } from "../gen/Pressure";
import type { Report } from "../gen/Report";
import type { Signal } from "../gen/Signal";
import { partial } from "../test/fixtures";
import { duration } from "./format";
import { t } from "./i18n";

function signal(scope: Signal["scope"]): Signal {
  return partial<Signal>({ key: scope, label: `native-${scope}`, value: 123, unit: "count", scope, healthy: null, status: "info" });
}

function health(signals: Signal[]): string {
  const pressure = partial<Pressure>({ signals, uptime_hours: 48, files: [], replicas: [], oldest_transaction: null });
  const report = partial<Report>({ findings: [], snapshot: {
    server: { engine: "mssql" }, sources: [], options: {},
    operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null },
  } });
  return renderToStaticMarkup(<HealthTab report={report} pressure={pressure} resources={[]} engineLabel="SQL Server" />);
}

function startupLifetime(): string {
  return t("ui.health.since_start", { duration: duration(48 * 3_600_000), engine: "SQL Server" });
}

test("unknown-period cumulative counters render their recorded scope rather than now or since start", () => {
  const html = renderToStaticMarkup(<SignalTile signal={signal("cumulative")} />);
  expect(html).toContain(t("common.signal_scope.cumulative"));
  expect(html).not.toContain(t("ui.waits.scope.now"));
  expect(html).not.toContain(t("ui.waits.scope.since_startup"));
});

test("cumulative counters remain in historical details without borrowing an unrelated server uptime", () => {
  for (const signals of [[signal("cumulative")], [signal("cumulative"), signal("since-startup")]]) {
    const html = health(signals);
    const historical = html.indexOf('<details class="historical-signals">');
    const counter = html.indexOf("native-cumulative");
    expect(historical).toBeGreaterThan(-1);
    expect(counter).toBeGreaterThan(historical);
    expect(counter).toBeLessThan(html.indexOf("</details>", historical));
    expect(html).toContain(t("common.signal_scope.cumulative"));
    expect(html).not.toContain(startupLifetime());
    expect(html).not.toContain(t("ui.waits.scope.now"));
  }
});

test("explicit startup counters keep their own lifetime while current observations stay outside historical details", () => {
  const html = health([signal("since-startup"), signal("now")]);
  const historical = html.indexOf('<details class="historical-signals">');
  expect(html).toContain("native-now");
  expect(html.indexOf("native-now")).toBeLessThan(historical);
  expect(html.indexOf("native-since-startup")).toBeGreaterThan(historical);
  expect(html).toContain(t("ui.waits.scope.since_startup"));
  expect(html).toContain(startupLifetime());
});
