import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { RuntimeFragments } from "../components/RuntimeFragments";
import { HealthTab } from "../components/Health";
import { runtimeFragmentReport } from "../test/runtime-fragments";
import { pageServerReadHealthReport } from "../test/page-server-reads";
import { queryWaitFragmentReport } from "../test/query-wait-fragments";
import { t } from "./i18n";

test("runtime fragment pages preserve exact original counters unknown mean and unpooled native endpoints", () => {
  const view = runtimeFragmentReport();
  view.entries[0]!.page_server_explanation={availability:"available",title:t("view.page_server_reads.title"),summary:t("view.page_server_reads.observed"),metrics:[{key:"view.page_server_reads.metric.last",label:t("view.page_server_reads.metric.last"),value:0,unit:"text",text:"9007199254740993"}],limitations:[t("view.page_server_reads.limit.storage")],investigate:t("view.page_server_reads.investigate"),verify:t("view.page_server_reads.verify")};
  const html = renderToStaticMarkup(<RuntimeFragments report={view} />);
  expect(html).toContain(t("view.page_server_reads.title"));expect(html).toContain(t("view.page_server_reads.limit.storage"));
  expect(html.match(/<article/g)).toHaveLength(10); expect(html).toContain("9007199254740993 µs"); expect(html).toContain(t("view.runtime_fragments.unknown")); expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/); for (const limit of view.limitations) expect(html).toContain(limit);
});
test("Health renders runtime and wait historical disclosures together and retains unavailable runtime context", () => {
  const saved = pageServerReadHealthReport(null); const view = runtimeFragmentReport(0); view.source_state = "unavailable"; view.selected_rows = null; view.summary = t("view.runtime_fragments.unavailable"); saved.runtime_fragments = view;
  saved.query_wait_fragments = queryWaitFragmentReport(0);
  const html = renderToStaticMarkup(<HealthTab report={saved} pressure={null} resources={[]} engineLabel="PostgreSQL" />); expect(html).toContain(view.title); expect(html).toContain(t("view.query_wait_fragments.title")); expect(html).toContain(view.summary); expect(html).not.toContain(t("ui.health.empty"));
});
test("runtime native labels and original keys remain text with no current query links or executable markup", () => {
  const view = runtimeFragmentReport(1); view.entries[0]!.metrics[0]!.title = '<script>runtime_canary()</script>'; view.entries[0]!.native.native.qualifiers.original_outcome = '<a href="https://private.invalid">value</a>';
  const html = renderToStaticMarkup(<RuntimeFragments report={view} />); expect(html).toContain("&lt;script&gt;"); expect(html).not.toContain("<script"); expect(html).not.toContain("<a ");
});
