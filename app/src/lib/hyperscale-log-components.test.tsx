import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { HyperscaleLogComponents } from "../components/HyperscaleLogComponents";
import { HealthTab } from "../components/Health";
import { hyperscaleLogComponentsReport, hyperscaleLogHealthReport } from "../test/hyperscale-log-components";
import { t } from "./i18n";

test("native component values and offset-free clocks remain exact, folded, paged and escaped", () => {
  const report = hyperscaleLogComponentsReport();
  report.entries[0]!.role_desc = "<script>private</script>";
  const html = renderToStaticMarkup(<HyperscaleLogComponents report={report} />);
  expect(html).toContain("9007199254740993"); expect(html).toContain(">0<");
  expect(html).toContain("2026-11-18T11:59:59.003"); expect(html).not.toContain("2026-11-18T11:59:59.003Z");
  expect(html.match(/<article/g)).toHaveLength(10); expect(html).not.toContain("<details open");
  expect(html).toContain("&lt;script&gt;"); expect(html).not.toContain("<script>");
});
test("component-only and failed-source-only evidence cannot fall through to healthy emptiness", () => {
  const report = hyperscaleLogHealthReport();
  const render = () => renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="SQL Server" />);
  expect(render()).toContain("9007199254740993"); expect(render()).not.toContain(t("ui.health.empty"));
  delete report.hyperscale_log_components;
  report.snapshot.sources = [{ source: "hyperscale-log-components", state: "unavailable", reason: null }];
  expect(render()).toContain(t("view.hyperscale_log_components.title")); expect(render()).not.toContain("9007199254740993");
  report.snapshot.sources[0]!.state = "collected";
  expect(render()).toContain(t("view.hyperscale_log_components.invalid")); expect(render()).not.toContain(t("ui.health.empty"));
});
