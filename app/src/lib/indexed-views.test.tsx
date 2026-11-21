import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { IndexedViews } from "../components/IndexedViews";
import type { NativeReadContext } from "../gen/NativeReadContext";
import { partial } from "../test/fixtures";
import { indexedViewsReport } from "../test/indexed-views";
import { t } from "./i18n";

test("folded view catalog keeps exact native values and separate local time without a health verdict", () => {
  const report = indexedViewsReport();
  const html = renderToStaticMarkup(<IndexedViews reports={[report]} />);
  expect(html).toContain("<details"); expect(html).not.toContain(" open=");
  expect(html).toContain("reporting.Revenue"); expect(html).toContain("9007199254740993");
  expect(html).toContain("2026-11-10T07:00:10.123"); expect(html).toContain("ROW (1)"); expect(html).toContain("PAGE (2)");
  for (const limitation of report.limitations) expect(html).toContain(limitation);
  expect(html).not.toContain('class="sev');
});

test("native usage absence, unsupported collection, partial and withdrawn evidence never become measured healthy zeros", () => {
  for (const state of ["absent", "not-collected", "denied", "partial", "empty", "withdrawn"] as const) {
    const report = indexedViewsReport();
    if (state === "absent") report.entries[0]!.usage = { kind: "observed", data: { row_present: false, user_seeks: null, user_scans: null, user_lookups: null, user_updates: null, last_user_seek_without_offset: null, last_user_scan_without_offset: null, last_user_lookup_without_offset: null, last_user_update_without_offset: null, reset: { kind: "unknown" } } };
    if (state === "not-collected") report.entries[0]!.usage = { kind: "not-collected" };
    if (state === "denied" || state === "withdrawn") { report.source.state = "unavailable"; report.entries = []; report.native_index_rows = null; report.observed_index_rows = 0; report.observed_view_indexes = 0; }
    if (state === "withdrawn") report.source.reason = t("view.indexed_views.sharing_withheld");
    if (state === "partial" || state === "empty") { report.entries = []; report.observed_view_indexes = 0; report.source.state = state; }
    const html = renderToStaticMarkup(<IndexedViews reports={[report]} />);
    if (state === "absent" || state === "not-collected") expect(html).toContain(t("view.indexed_views.usage", { state }));
    if (state === "withdrawn") expect(html).toContain(t("view.indexed_views.sharing_withheld"));
    if (state === "denied" || state === "withdrawn") expect(html).not.toContain("9007199254740993");
    expect(html).toContain(t("view.indexed_views.limit.visibility")); expect(html).not.toContain('class="sev');
  }
});

test("native labels and columns are bounded plain text and never emit control characters or HTML", () => {
  const report = indexedViewsReport();
  report.entries[0]!.parent_name = "<script>\n\u001b[31m" + "界".repeat(2000);
  report.entries[0]!.columns[0]!.name = "<svg>\n\u001b[31m" + "界".repeat(2000);
  const html = renderToStaticMarkup(<IndexedViews reports={[report]} />);
  expect(html).toContain("&lt;script&gt;"); expect(html).toContain("&lt;svg&gt;");
  expect(html).not.toContain("<script>"); expect(html).not.toContain("<svg>"); expect(html).not.toContain("\u001b");
  expect(html).not.toContain("界".repeat(161));
});

test("original read bounds remain independent of a current scan catalog scope", () => {
  const current = indexedViewsReport(); const original = indexedViewsReport();
  original.context = partial<NativeReadContext>({ observed_from: "2020-01-01T00:00:00Z", observed_until: "2020-01-01T00:00:01Z" });
  original.entries[0]!.parent_name = "OriginalView";
  const html = renderToStaticMarkup(<IndexedViews reports={[current, original]} />);
  expect(html).toContain("OriginalView"); expect(html).toContain("2020-01-01T00:00:00Z");
  expect(html).toContain(t("ui.index_configuration.original_scope")); expect(html).toContain(t("ui.index_configuration.scan_scope"));
});
