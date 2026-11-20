import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { FullTextPopulations } from "../components/FullTextPopulations";
import { HealthTab } from "../components/Health";
import { fullTextPopulationReport } from "../test/full-text-populations";
import { tableMemoryHealthReport } from "../test/table-memory";
import { t } from "./i18n";

test("saved full-text runtime context remains folded, original and informational with native local time", () => {
  const report = fullTextPopulationReport();
  const html = renderToStaticMarkup(<FullTextPopulations reports={[report]} />);
  expect(html).toContain(t("view.full_text_populations.status", { state: "stopped" }));
  expect(html).toContain("2019-12-31T18:00:00.007");
  expect(html).not.toContain("2019-12-31T18:00:00.007Z");
  expect(html).not.toContain("<details open");
  expect(html).not.toContain('class="sev');
  for (const privateValue of ["private-machine", "private-member", "private-original-host", "private-process-and-database"]) expect(html).not.toContain(privateValue);
});
test("unavailable original preserves its reason without a target or manufactured zero population", () => {
  const report = fullTextPopulationReport(); report.original = null; report.native_rows = null;
  report.entries = []; report.source.state = "unavailable"; report.summary = t("view.full_text_populations.sharing_withheld");
  report.source.reason = report.summary;
  const html = renderToStaticMarkup(<FullTextPopulations reports={[report]} />);
  expect(html).toContain(report.summary); expect(html).not.toContain("2019-12-31T18:00:00.007");
  expect(html).not.toContain(t("view.full_text_populations.target", { database: "7", table: "100", catalog: "2", index: "1" }));
});
test("native descriptions are bounded plain Unicode text and Health consumes the canonical optional reports", () => {
  const report = fullTextPopulationReport(); report.entries[0]!.metrics[0]!.text = `<script>\n\u001b[31m${"界".repeat(1000)}`;
  const snapshot = tableMemoryHealthReport([]); snapshot.full_text_populations = [report];
  const html = renderToStaticMarkup(<HealthTab report={snapshot} pressure={null} resources={[]} engineLabel="SQL Server" />);
  expect(html).toContain("&lt;script&gt;"); expect(html).not.toContain("<script>"); expect(html).not.toContain("\u001b");
  expect(html).not.toContain("界".repeat(257));
  expect(html).toContain(t("view.full_text_populations.saved_summary"));
  snapshot.full_text_populations = undefined;
  expect(renderToStaticMarkup(<HealthTab report={snapshot} pressure={null} resources={[]} engineLabel="SQL Server" />)).not.toContain(t("view.full_text_populations.title"));
});
