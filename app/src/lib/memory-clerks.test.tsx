import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { HealthTab } from "../components/Health";
import { MemoryClerks } from "../components/MemoryClerks";
import type { NativeReadContext } from "../gen/NativeReadContext";
import { partial } from "../test/fixtures";
import { EXACT_CLERK_ALLOCATION, memoryClerkHealthReport, memoryClerkReport } from "../test/memory-clerks";
import { t } from "./i18n";

test("clerk dimensions remain separate exact values with folded records and text-only native labels", () => {
  const report = memoryClerkReport();
  report.entries[0]!.clerk_type = "<img src=x onerror=alert(1)>";
  report.entries[0]!.name = "<script>alert(1)</script>";
  const html = renderToStaticMarkup(<MemoryClerks report={report} />);
  expect(html).toContain(EXACT_CLERK_ALLOCATION);
  expect(html).toContain(">0<");
  expect(html).toContain(t("ui.memory_failures.not_reported"));
  expect(html.match(/<article/g)).toHaveLength(10);
  expect(html).not.toContain("<details open");
  expect(html).not.toContain("<img");
  expect(html).not.toContain("<script");
  expect(html).toContain("&lt;img");
  expect(html).toContain("&lt;script");
  expect(html).not.toContain("sig-dot");
  for (const limitation of report.limitations) expect(html).toContain(limitation);
});

test("native read bounds stay distinct from an unavailable original clock", () => {
  const report = memoryClerkReport(1);
  report.context = { kind: "native", context: partial<NativeReadContext>({
    observed_from: "2020-01-01T08:00:00Z", observed_until: "2020-01-01T08:00:01Z",
  }) };
  const native = renderToStaticMarkup(<MemoryClerks report={report} />);
  expect(native).toContain("2020-01-01T08:00:00Z");
  expect(native).toContain("2020-01-01T08:00:01Z");
  expect(native).not.toContain("2020-01-01T12:00:00Z");
  report.context = null;
  expect(renderToStaticMarkup(<MemoryClerks report={report} />)).not.toContain("2020-01-01T08:00");
});

test("Health renders a clerk-only explanation without an engine-name condition or healthy empty fallback", () => {
  const data = memoryClerkHealthReport();
  data.snapshot.sources = [];
  const html = renderToStaticMarkup(<HealthTab report={data} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
  expect(html).toContain(t("collected.memory_clerks.title"));
  expect(html).toContain(EXACT_CLERK_ALLOCATION);
  expect(html).not.toContain(t("ui.health.empty"));
});

test("Health retains a clerk source failure or withheld explanation without inventing allocation values", () => {
  const data = memoryClerkHealthReport(null);
  const render = () => renderToStaticMarkup(<HealthTab report={data} pressure={null} resources={[]} engineLabel="SQL Server" />);
  const unavailable = render();
  expect(unavailable).toContain(t("collected.memory_clerks.title"));
  expect(unavailable).toContain(t("ui.dashboard.status.unavailable"));
  expect(unavailable).not.toContain(EXACT_CLERK_ALLOCATION);
  expect(unavailable).not.toContain(t("ui.health.empty"));
  data.snapshot.sources = [{ source: "memory clerks", state: "not-applicable", reason: null }];
  const notApplicable = render();
  expect(notApplicable).toContain(t("ui.retained_failures.not_applicable"));
  expect(notApplicable).not.toContain(t("collected.memory_clerks.invalid"));
  expect(notApplicable).not.toContain(EXACT_CLERK_ALLOCATION);
  data.snapshot.sources = [{ source: "memory clerks", state: "collected", reason: null }];
  const unexplained = render();
  expect(unexplained).toContain(t("collected.memory_clerks.invalid"));
  expect(unexplained).not.toContain("<article");
  expect(unexplained).not.toContain(t("ui.health.empty"));
});
