import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { HealthTab } from "../components/Health";
import { ManagedInstanceResources, ManagedInstanceResourceIntervals } from "../components/ManagedInstanceResources";
import { managedInstanceResourceReport, managedInstanceHistoryHealthReport } from "../test/managed-instance-resources";
import { t } from "./i18n";

test("original instance history is folded and never becomes the current database pressure point", () => {
  const history = managedInstanceResourceReport();
  const html = renderToStaticMarkup(<HealthTab report={managedInstanceHistoryHealthReport(history)} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
  expect(html).toContain(history.title); expect(html).toContain(history.summary); expect(html).not.toContain("9007199254740993"); expect(html).not.toContain("<details open");
});
test("canonical interval fields preserve wide totals zero and unavailable layout without an HTML or Unicode path", () => {
  const history = managedInstanceResourceReport(); history.intervals[0]!.fields.push({ key: "resource_name", label: t("view.mi_resources.field.resource_name"), availability: "available", native_value: "<img>🌿", value: "<img>🌿".repeat(1000), missing_reason: null });
  const html = renderToStaticMarkup(<ManagedInstanceResourceIntervals report={history} />);
  expect(html).toContain("9007199254740993"); expect(html).toContain(">0<"); expect(html).toContain(t("view.mi_resources.field_unsupported")); expect(html).toContain("&lt;img&gt;"); expect(html).not.toContain("<img>"); expect(html).not.toContain("�"); expect(html).not.toContain("<img>🌿".repeat(100));
});
test("native partial coverage and unavailable originals do not render stale interval quantities or a healthy zero", () => {
  const history = managedInstanceResourceReport(); history.source_state = "partial"; history.read_complete = false;
  expect(renderToStaticMarkup(<ManagedInstanceResources report={history} />)).toContain(t("ui.common.read_partial"));
  history.source_state = "unavailable"; history.native_source = null; history.native_rows = null; history.recorded_rows = null; history.summary = t("view.mi_resources.sharing_withheld");
  const html = renderToStaticMarkup(<ManagedInstanceResourceIntervals report={history} />); expect(html).not.toContain("9007199254740993"); expect(html).not.toContain(">0<"); expect(html).toContain(t("ui.dashboard.status.unavailable"));
});
