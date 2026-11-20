import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { SuspectPageHistory } from "../components/SuspectPageHistory";
import { HealthTab } from "../components/Health";
import { suspectPageHistoryReport, suspectPageHealthReport } from "../test/suspect-pages";
import { t } from "./i18n";

test("folded retained history keeps repaired-only zero separate from unavailable and has no health grade", () => {
  for (const unavailable of [false, true]) {
    const report = suspectPageHistoryReport(unavailable); const html = renderToStaticMarkup(<SuspectPageHistory report={report} />);
    expect(html).toContain(report.summary); expect(html).not.toContain("<details open"); expect(html).not.toContain("sig-dot");
    for (const limitation of report.limitations) expect(html).toContain(limitation);
    if (unavailable) { expect(html).toContain(t("view.suspect_pages.permission")); expect(html).not.toContain(">0<"); }
    else { expect(html).toContain(">0<"); expect(html).toContain(">3<"); }
  }
});
test("history labels and native local times render as text and Health retains an informational-only report", () => {
  const report = suspectPageHistoryReport(); report.database = "<img src=x onerror=alert(1)>🧪"; report.last_reported_time_without_offset = "2019-12-30T15:00:00.123";
  const html = renderToStaticMarkup(<SuspectPageHistory report={report} />); expect(html).toContain("&lt;img"); expect(html).not.toContain("<img"); expect(html).toContain("2019-12-30T15:00:00.123"); expect(html).toContain("🧪");
  const health = renderToStaticMarkup(<HealthTab report={suspectPageHealthReport()} pressure={null} resources={[]} engineLabel="SQL Server" />); expect(health).toContain(report.title); expect(health).not.toContain(t("ui.health.empty"));
});
