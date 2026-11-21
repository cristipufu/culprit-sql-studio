import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { IndexConfigurations } from "../components/IndexConfigurations";
import type { NativeReadContext } from "../gen/NativeReadContext";
import { partial } from "../test/fixtures";
import { indexConfigurationReport, selectedIndexConfiguration } from "../test/index-configuration";
import { IndexConfiguration } from "../views/schema-table/IndexConfiguration";
import { t } from "./i18n";


test("folded configuration keeps exact large IDs and each partition encoding without a health verdict", () => {
  const view = indexConfigurationReport(); const html = renderToStaticMarkup(<IndexConfigurations reports={[view]} />);
  expect(html).toContain("<details"); expect(html).not.toContain(" open=");
  expect(html).toContain("9007199254740993"); expect(html).toContain("9007199254740994");
  expect(html).toContain("ROW (1)"); expect(html).toContain("PAGE (2)"); expect(html).toContain(view.qualification);
  expect(html).toContain(t("view.index_configuration.partial")); expect(html).not.toContain('class="sev');
});
test("schema disclosure refuses stale generation, ambiguous entries and original-only matches", () => {
  const { report, index } = selectedIndexConfiguration();
  const shown = renderToStaticMarkup(<IndexConfiguration report={report} index={index} />);
  expect(shown).toContain("80");
  expect(shown).toContain(report.index_configuration[0]!.qualification);
  for (const limitation of report.index_configuration[0]!.limitations) expect(shown).toContain(limitation);
  const entry = report.index_configuration[0]!.entries[0]!;
  entry.parent_object = { ...entry.parent_object, created_without_offset: "2000-01-01T00:00:00.0000000" };
  expect(renderToStaticMarkup(<IndexConfiguration report={report} index={index} />)).toBe("");
  entry.parent_object = index.mssql_configuration!.parent_object; report.index_configuration[0]!.entries.push(entry);
  expect(renderToStaticMarkup(<IndexConfiguration report={report} index={index} />)).toBe("");
  report.index_configuration[0]!.entries.pop();
  report.index_configuration[0]!.context = partial<NativeReadContext>({ observed_from: "2000-01-01T00:00:00Z", observed_until: "2000-01-01T00:00:01Z" });
  expect(renderToStaticMarkup(<IndexConfiguration report={report} index={index} />)).toBe("");
  report.index_configuration[0]!.context = null; report.snapshot.sources![0]!.state = "unavailable";
  expect(renderToStaticMarkup(<IndexConfiguration report={report} index={index} />)).toBe("");
});
test("native NULL, unavailable and future descriptions remain bounded plain text", () => {
  const view = indexConfigurationReport(); const entry = view.entries[0]!;
  entry.fields[0]!.text = t("view.index_configuration.native_null");
  entry.partitions[0]!.fields[0]!.text = t("view.index_configuration.availability", { state: "unsupported" });
  entry.partitions[1]!.fields[0]!.text = "<script>\n\u001b[31m" + "界".repeat(2000);
  const html = renderToStaticMarkup(<IndexConfigurations reports={[view]} />);
  expect(html).toContain(t("view.index_configuration.native_null")); expect(html).toContain(t("view.index_configuration.availability", { state: "unsupported" }));
  expect(html).toContain("&lt;script&gt;"); expect(html).not.toContain("<script>"); expect(html).not.toContain("\u001b"); expect(html).not.toContain("界".repeat(161));
});
test("withdrawn original remains visible without native entries or a replacement current scope", () => {
  const view = indexConfigurationReport(); view.entries = []; view.dataset = null; view.source_state = "unavailable";
  view.summary = t("view.index_configuration.sharing_withheld"); view.native_rows = null;
  const html = renderToStaticMarkup(<IndexConfigurations reports={[view]} />);
  expect(html).toContain(view.summary); expect(html).toContain(t("ui.dashboard.status.unavailable")); expect(html).not.toContain("9007199254740993");
});
