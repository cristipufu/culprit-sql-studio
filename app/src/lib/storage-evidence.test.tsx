import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { StorageEvidence } from "../components/StorageEvidence";
import { HealthTab } from "../components/Health";
import { storageHealthReport, storageReport } from "../test/storage";
import { t } from "./i18n";

test("Rust storage report preserves exact large bytes and measured zero with folded native rows", () => {
  const html = renderToStaticMarkup(<StorageEvidence report={storageReport()} />);
  expect(html).toContain("9007199254740993");
  expect(html).toContain(">0<");
  expect(html).not.toContain("<details open");
  expect(html.match(/<article/g)).toHaveLength(2);
  expect(html).not.toContain("sig-dot");
});
test("unknown size basis, unavailable values and hostile native names stay separate text-only facts", () => {
  const report = storageReport("unknown");
  report.entries[1]!.title = "<img src=x onerror=alert(1)>";
  const html = renderToStaticMarkup(<StorageEvidence report={report} />);
  expect(html).toContain(t("ui.memory_failures.not_reported"));
  expect(html).toContain(t("view.storage.basis.unknown"));
  expect(html).not.toContain("<img");
  expect(html).toContain("&lt;img");
});
test("Health renders the shared storage report without an engine condition or healthy empty fallback", () => {
  const data = storageHealthReport(storageReport("partial"));
  const html = renderToStaticMarkup(
    <HealthTab report={data} pressure={null} resources={[]} engineLabel="PostgreSQL" />,
  );
  expect(html).toContain(t("view.storage.title"));
  expect(html).toContain(t("ui.common.read_partial"));
  expect(html).not.toContain(t("ui.health.empty"));
});

test("unavailable storage remains an explicit read gap without invented zero fields or a healthy empty state", () => {
  const native = renderToStaticMarkup(<StorageEvidence report={storageReport("unavailable")} />);
  expect(native).toContain(t("ui.dashboard.status.unavailable"));
  expect(native).not.toContain("<article");
  const data = storageHealthReport(null);
  const health = renderToStaticMarkup(
    <HealthTab report={data} pressure={null} resources={[]} engineLabel="SQL Server" />,
  );
  expect(health).toContain(t("view.storage.title"));
  expect(health).toContain(t("ui.dashboard.status.unavailable"));
  expect(health).not.toContain(t("ui.health.empty"));
});
