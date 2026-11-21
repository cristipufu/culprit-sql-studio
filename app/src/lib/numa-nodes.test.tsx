import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { numaNodeReport as original } from "../test/numa-nodes";
import { InstanceResources } from "../components/InstanceResources";
import { HealthTab } from "../components/Health";
import { pageServerReadHealthReport } from "../test/page-server-reads";
import { t } from "./i18n";

test("CPU and memory source populations keep exact signed bitmaps and native KiB without aggregate pressure", () => {
  const report = original("numa_exact");
  const nodes = report.numa_nodes!;
  const html = renderToStaticMarkup(<InstanceResources report={report} />);
  expect(nodes.populations).toHaveLength(2);
  expect(html).toContain("-9223372036854775808");
  expect(html).toContain("9007199254740993");
  expect(html).toContain(nodes.populations[0]!.title);
  expect(html).toContain(nodes.populations[1]!.title);
  expect(html).toContain(nodes.context!.kind === "native" ? nodes.context!.context.observed_from : nodes.context!.observed_from);
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  expect(report.groups).toHaveLength(0);
  expect(report.observed_at).toBeNull();
});
test("native NULL, unavailable columns and measured zero retain separate canonical states", () => {
  const nulls = original("numa_null");
  const unavailable = original("numa_unavailable");
  const zero = original("numa_zero");
  expect(renderToStaticMarkup(<InstanceResources report={nulls} />)).toContain(t("view.schedulers.value.null"));
  expect(renderToStaticMarkup(<InstanceResources report={unavailable} />)).toContain(t("view.schedulers.value.unavailable"));
  expect(zero.numa_nodes!.populations[1]!.entries[0]!.fields.find((field) => field.native_name === "pages_kb")!.value).toBe("0");
});
test("partial and denied original coverage remains visible with bounded paging and text-only native properties", () => {
  const report = original("numa_limited");
  const population = report.numa_nodes!.populations[0]!;
  population.entries[0]!.fields[0]!.value = '<img src=x onerror="numa_canary()">';
  const html = renderToStaticMarkup(<InstanceResources report={report} />);
  expect(html).toContain("&lt;img");
  expect(html).not.toContain("<img");
  expect(html).toContain(t("ui.termination_history.next"));
  expect(population.entries_limited).toBe(true);
  for (const name of ["numa_partial", "numa_cpu_denied", "numa_both_denied"] as const) {
    const saved = pageServerReadHealthReport(null);
    saved.snapshot.sources = [];
    saved.numa_nodes = [original(name)];
    const html = renderToStaticMarkup(<HealthTab report={saved} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
    expect(html).not.toContain(t("ui.health.empty"));
    for (const population of saved.numa_nodes[0]!.numa_nodes!.populations) {
      expect(html).toContain(population.title);
    }
  }
});
