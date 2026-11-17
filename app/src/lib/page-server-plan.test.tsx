import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import english from "../../../locales/en.json";
import { NodeDetails } from "../components/NodeDetails";
import type { Plan } from "../gen/Plan";
import { pageServerDisplay, pageServerPlan } from "../test/page-server-plan";

function render(plan: Plan): string {
  return renderToStaticMarkup(<NodeDetails plan={plan} node={plan.root} share={null} />);
}
function disclosure(html: string): string {
  return html.match(/<details class="nd-all nd-page-server"[^>]*>[\s\S]*?<\/details>/)?.[0] ?? "";
}

test("selected operator details preserve exact shared page counts and start folded", () => {
  const panel = disclosure(render(pageServerPlan()));
  expect(panel).toContain("9007199254740995");
  expect(panel).not.toContain("9,007,199,254,740,995");
  expect(panel).not.toContain("9007199254740996");
  expect(panel).not.toContain(" open=");
  expect(panel.match(/<dt>/g)).toHaveLength(4);
  expect(panel).toContain("<dd>0</dd>");
  expect(panel).toContain(english["view.plan_page_server_io.limit.recorded"]);
});

test("partial independent fields retain backend unknown text instead of becoming zeroes", () => {
  const panel = disclosure(render(pageServerPlan(pageServerDisplay(true))));
  expect(panel.match(/<dd>Unknown<\/dd>/g)).toHaveLength(2);
  expect(panel.match(/<dd>0<\/dd>/g)).toHaveLength(1);
  expect(panel).toContain("<dd>5</dd>");
  expect(panel).toContain(english["view.plan_page_server_io.limit.partial"]);
});

test("absent or another operator's shared view never borrows raw runtime counters", () => {
  const plan = pageServerPlan(null);
  plan.root.runtime = {
    cpu_ms: null, logical_reads: null, physical_reads: null, spill_pages: null, thread_rows: [],
    page_server_io: { threads_observed: 1, reads: "17", read_aheads: "0", lob_reads: "0", lob_read_aheads: "0", issues: [] },
  };
  expect(disclosure(render(plan))).toBe("");
  const other = pageServerPlan();
  other.view!.nodes[0]!.id = other.root.id + 1;
  expect(disclosure(render(other))).toBe("");
});

test("native display strings stay text and output remains capped at four fields", () => {
  const display = pageServerDisplay();
  display.metrics[0]!.label = '<img src=x onerror="window.pageCounterXss=true">';
  display.metrics[0]!.text = '<script>window.pageCounterXss=true</script>';
  display.metrics.push({ label: "Unexpected fifth display field", value: 0, unit: "text", text: "secret-extra-field" });
  display.limitations.push("Second bounded limitation", "Unexpected third limitation");
  const panel = disclosure(render(pageServerPlan(display)));
  expect(panel).not.toContain("<img");
  expect(panel).not.toContain("<script>");
  expect(panel).toContain("&lt;script&gt;");
  expect(panel.match(/<dt>/g)).toHaveLength(4);
  expect(panel).not.toContain("secret-extra-field");
  expect(panel).not.toContain("Unexpected third limitation");
});
