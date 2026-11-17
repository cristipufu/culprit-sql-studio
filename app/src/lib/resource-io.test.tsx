import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ResourceCharts } from "../components/Health";
import type { ResourceSample } from "../gen/ResourceSample";
import { t } from "./i18n";

function samples(basis: ResourceSample["data_io_accounting"]): ResourceSample[] {
  return Array.from({ length: 4 }, (_, index) => ({
    at: `2020-01-01T06:00:${String(index * 15).padStart(2, "0")}Z`,
    data_io_accounting: basis, data_io_pct: 99, cpu_pct: null, log_write_pct: null, memory_pct: null, workers_pct: null,
  }));
}

test("native accounting determines the I/O chart wording without server/tier name inference", () => {
  for (const [basis, label, meaning] of [
    ["data-files", "core.resource_io.data_files.label", "core.resource_io.data_files.meaning"],
    ["compute-local-ssd", "core.resource_io.compute_local_ssd.label", "core.resource_io.compute_local_ssd.meaning"],
    ["unknown", "core.resource_io.unknown.label", "core.resource_io.unknown.meaning"],
  ] as const) {
    const html = renderToStaticMarkup(<ResourceCharts samples={samples(basis)} signals={[]} />);
    expect(html).toContain(t(label));
    expect(html).toContain(t(meaning));
    expect(html.match(/class="resource-chart"/g)?.length).toBe(1);
    expect(html).not.toContain("Data reads");
  }
});

test("mixed observed bases render independent series and cannot inherit one comparable good grade", () => {
  const mixed = [...samples("data-files"), ...samples("compute-local-ssd").map((sample) => ({ ...sample, at: sample.at.replace("06:00", "06:01") }))];
  const html = renderToStaticMarkup(<ResourceCharts samples={mixed} signals={[]} />);
  expect(html.match(/class="resource-chart"/g)?.length).toBe(2);
  expect(html).toContain(t("core.resource_io.data_files.label"));
  expect(html).toContain(t("core.resource_io.compute_local_ssd.label"));
  expect(html).not.toContain("sig-dot good");
});

test("pool/unknown accounting remains unknown even alongside known local SSD samples", () => {
  const html = renderToStaticMarkup(<ResourceCharts samples={[...samples("unknown"), ...samples("compute-local-ssd")]} signals={[]} />);
  expect(html.match(/class="resource-chart"/g)?.length).toBe(2);
  expect(html).toContain(t("core.resource_io.unknown.meaning"));
});
