import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { HealthTab } from "../components/Health";
import { SourceAcquisitions } from "../components/SourceAcquisitions";
import { ACQUISITION_TEMPLATE_ID, historicalAcquisition } from "../test/source-acquisitions";
import { healthReportFixture } from "../test/health-report";
import { t } from "./i18n";

test("historical acquisition metadata is folded and paged with exact client counts rather than current database identity", () => {
  const reports = Array.from({ length: 12 }, (_, index) => historicalAcquisition(index));
  const first = reports[0]; if (first?.kind !== "available") throw new Error("available fixture");
  first.report.summary = "<script>native()</script>";
  const html = renderToStaticMarkup(<SourceAcquisitions reports={reports} />);
  expect(html.match(/<article/g)).toHaveLength(10); expect(html).not.toContain("<details open");
  expect(html).toContain("9007199254740993"); expect(html).toContain("18446744073709551615");
  expect(html).toContain("2020-01-01T00:00:00Z"); expect(html).toContain(t("view.source_overhead.limit.historical"));
  expect(html).toContain("&lt;script&gt;"); expect(html).not.toContain("<script>");
  expect(html).not.toContain("original-capture-10"); expect(html).not.toContain("private-manifest-digest");
});
test("a failed historical attempt stays visible in Health independently from current scan data", () => {
  const report = healthReportFixture({ source_acquisitions: [historicalAcquisition()], snapshot: {
    server: { engine: "postgres" }, sources: [], resources: [], pressure: null,
  } });
  const html = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
  expect(html).toContain(t("view.source_overhead.title")); expect(html).toContain(t("view.source_overhead.limit.incomplete"));
  expect(html).toContain("original-capture-0"); expect(html).not.toContain(t("ui.health.empty"));
});
test("withdrawn metadata and absent history do not borrow another capture or render zero acquisition work", () => {
  const html = renderToStaticMarkup(<SourceAcquisitions reports={[{ kind: "unavailable", reason: t("view.source_overhead.sharing_withheld") }]} />);
  expect(html).toContain(t("view.source_overhead.sharing_withheld")); expect(html).not.toContain("9007199254740993");
  expect(html).not.toContain("2020-01-01"); expect(html).not.toContain(ACQUISITION_TEMPLATE_ID);
  expect(renderToStaticMarkup(<SourceAcquisitions reports={[]} />)).toBe("");
});
