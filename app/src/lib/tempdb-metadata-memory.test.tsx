import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { HealthTab } from "../components/Health";
import { TempdbMetadataMemory } from "../components/TempdbMetadataMemory";
import { EXACT_TEMPDB_RESERVATION, tempdbMetadataHealthReport, tempdbMetadataReport } from "../test/tempdb-metadata-memory";
import { t } from "./i18n";

test("metadata memory shows separate exact reservation, zero and unknown usage in bounded folded records", () => {
  const report = tempdbMetadataReport("partial");
  report.entries[0]!.description = "<img src=x onerror=alert(1)>";
  report.entries[0]!.native_type_name = "<script>alert(1)</script>";
  report.limitations.push(t("view.tempdb_metadata_memory.source.denied"));
  const html = renderToStaticMarkup(<TempdbMetadataMemory report={report} />);
  expect(html.match(/<article/g)).toHaveLength(10);
  expect(html).not.toContain("<details open");
  expect(html).toContain(EXACT_TEMPDB_RESERVATION);
  expect(html).toContain(">0<");
  expect(html).toContain(t("ui.memory_failures.not_reported"));
  expect(html).toContain("2020-01-01T12:00:00.2Z");
  expect(html).toContain("2020-01-01T12:00:00.5Z");
  expect(html).toContain("2020-01-01T14:00:00.003");
  expect(html).not.toContain("consumer-10");
  expect(html).not.toContain("<img"); expect(html).not.toContain("<script");
  expect(html).toContain("&lt;img"); expect(html).toContain("&lt;script");
  expect(html).not.toContain("sig-dot");
  for (const limitation of report.limitations) expect(html).toContain(limitation);
  for (const privateValue of ["private-metadata-row", "private-metadata-machine", "private-metadata-instance", "private-process-epoch"])
    expect(html).not.toContain(privateValue);
});

test("disabled and unknown metadata feature states retain their canonical words without invented consumer bytes", () => {
  for (const mode of ["disabled", "unknown"] as const) {
    const report = tempdbMetadataReport(mode);
    const html = renderToStaticMarkup(<TempdbMetadataMemory report={report} />);
    expect(html).toContain(report.summary);
    expect(html).not.toContain("<article");
    expect(html).not.toContain(EXACT_TEMPDB_RESERVATION);
    if (mode === "unknown") expect(html).toContain(t("ui.memory_failures.not_reported"));
    else expect(html).toContain(">0<");
  }
});

test("Health uses the canonical historical report without an engine gate or healthy empty fallback", () => {
  const data = tempdbMetadataHealthReport(); data.snapshot.sources = [];
  const html = renderToStaticMarkup(<HealthTab report={data} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
  expect(html).toContain(t("view.tempdb_metadata_memory.title"));
  expect(html).toContain(EXACT_TEMPDB_RESERVATION);
  expect(html).not.toContain(t("ui.health.empty"));
});

test("Health keeps unavailable and withheld metadata explanations visible without borrowing a live value", () => {
  const data = tempdbMetadataHealthReport(null);
  const render = () => renderToStaticMarkup(<HealthTab report={data} pressure={null} resources={[]} engineLabel="SQL Server" />);
  expect(render()).toContain(t("view.tempdb_metadata_memory.source.denied"));
  expect(render()).not.toContain(EXACT_TEMPDB_RESERVATION);
  expect(render()).not.toContain(t("ui.health.empty"));
  data.snapshot.sources = [{ source: "tempdb metadata memory", state: "collected", reason: null }];
  expect(render()).toContain(t("view.tempdb_metadata_memory.invalid"));
  expect(render()).not.toContain("<article");
  data.snapshot.sources = [{ source: "tempdb metadata memory", state: "not-applicable", reason: null }];
  expect(render()).toContain(t("ui.retained_failures.not_applicable"));
  expect(render()).not.toContain(t("ui.health.empty"));
});
