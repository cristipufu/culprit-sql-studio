import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { QueryWaitFragments } from "../components/QueryWaitFragments";
import { HealthTab } from "../components/Health";
import { queryWaitFragmentReport } from "../test/query-wait-fragments";
import { pageServerReadHealthReport } from "../test/page-server-reads";
import { t } from "./i18n";


test("native wait fragments fold and page separate outcomes with exact metric text and original qualifications", () => {
  const view = queryWaitFragmentReport();
  const html = renderToStaticMarkup(<QueryWaitFragments report={view} />);
  expect(html.match(/<article/g)).toHaveLength(10);
  expect(html).toContain(view.entries[0]!.metrics[0]!.text!);
  expect(html).toContain("Aborted"); expect(html).toContain("Regular");
  expect(html).not.toContain("Native category 10");
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  for (const limitation of view.limitations) expect(html).toContain(limitation);
  expect(html).not.toContain("href=");
});
test("Health retains unavailable wait history without a current engine condition or a healthy empty fallback", () => {
  const view = queryWaitFragmentReport(0); view.source_state = "unavailable"; view.selected_rows = null;
  view.summary = t("view.query_wait_fragments.unavailable"); view.source_reason = t("view.query_wait_fragments.sharing_withheld");
  const saved = pageServerReadHealthReport(null); saved.query_wait_fragments = view;
  const html = renderToStaticMarkup(<HealthTab report={saved} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
  expect(html).toContain(view.title); expect(html).toContain(view.summary); expect(html).toContain(view.source_reason);
  expect(html).not.toContain(t("ui.health.empty"));
});
test("native descriptions and historical qualifiers render as text only with no executable links", () => {
  const view = queryWaitFragmentReport(1);
  view.entries[0]!.category_description = '<script>wait_canary()</script>';
  view.entries[0]!.outcome_description = '<img src=x onerror="wait_canary()">';
  view.entries[0]!.native.native.qualifiers.original_replica = '<a href="https://private.invalid">member</a>';
  const html = renderToStaticMarkup(<QueryWaitFragments report={view} />);
  expect(html).toContain("&lt;script&gt;"); expect(html).toContain("&lt;img");
  expect(html).not.toContain("<script"); expect(html).not.toContain("<img"); expect(html).not.toContain("<a ");
});
