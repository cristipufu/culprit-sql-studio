import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { Error as NativeError } from "../gen/Error";
import type { Report } from "../gen/Report";
import type { ServerInfo } from "../gen/ServerInfo";
import { HealthTab } from "../components/Health";
import { healthReportFixture } from "../test/health-report";
import { ProviderAcquisition } from "../components/ProviderAcquisition";
import { currentAcquisition, CURRENT_TEMPLATE_ID } from "../test/provider-acquisition";
import { partial } from "../test/fixtures";
import { requireProbe, requireScan } from "./provider-acquisition";
import { t } from "./i18n";

test("probe projection keeps original success separate from unconfirmed settlement without editing the error", () => {
  const acquisition = currentAcquisition();
  const server = partial<ServerInfo>({ database: "original-database" });
  let observed: unknown;
  expect(
    requireProbe({ outcome: { kind: "connected", server }, acquisition }, (value) => {
      observed = value;
    }),
  ).toBe(server);
  expect(observed).toBe(acquisition);
  const error: NativeError = {
    code: "query_failed",
    message: "same-original-error",
    hint: null,
    detail: { "collection-stopped": "cancellation-unconfirmed" },
  };
  const before = JSON.stringify(error);
  expect(() =>
    requireProbe(
      { outcome: { kind: "native-work-unconfirmed", original: { kind: "connected", server }, error }, acquisition },
      (value) => {
        observed = value;
      },
    ),
  ).toThrow();
  try {
    requireProbe({
      outcome: { kind: "native-work-unconfirmed", original: { kind: "connected", server }, error },
      acquisition,
    });
  } catch (actual) {
    expect(actual).toBe(error);
  }
  expect(JSON.stringify(error)).toBe(before);
  expect(observed).toBe(acquisition);
});
test("failed scan metadata belongs to its request and cannot become a successful report", () => {
  const acquisition = currentAcquisition();
  const error: NativeError = { code: "permission_denied", message: "original", hint: null };
  let observed: unknown;
  try {
    requireScan({ kind: "failed", error, acquisition }, (value) => {
      observed = value;
    });
    throw new Error("expected failed scan");
  } catch (actual) {
    expect(actual).toBe(error);
  }
  expect(observed).toBe(acquisition);
  const report = partial<Report>({ provider_acquisition: acquisition, source_acquisitions: [] });
  expect(
    requireScan({ kind: "complete", report }, (value) => {
      observed = value;
    }),
  ).toBe(report);
  expect(observed).toBe(acquisition);
});
test("current work stays folded, paged and text-only with exact integers and no invented total", () => {
  const report = currentAcquisition(12);
  if (report.kind !== "available") throw new Error("available fixture");
  report.summary = "<script>route()</script>";
  const html = renderToStaticMarkup(<ProviderAcquisition report={report} />);
  expect(html.match(/<article/g)).toHaveLength(10);
  expect(html).not.toContain("<details open");
  expect(html).toContain("&lt;script&gt;");
  expect(html).not.toContain("<script>");
  expect(html).toContain("9007199254740993");
  expect(html).toContain("18446744073709551615");
  expect(html).toContain(CURRENT_TEMPLATE_ID);
  expect(html).toContain("2026-11-21T00:00:00Z");
  expect(html).not.toContain(t("view.provider_acquisition.total", { count: "0" }));
  expect(html).not.toContain("manifest_sha256");
});
test("withdrawn current accounting stays unavailable without another observation's clocks or native identity", () => {
  const html = renderToStaticMarkup(
    <ProviderAcquisition report={{ kind: "unavailable", reason: t("view.provider_acquisition.sharing_withheld") }} />,
  );
  expect(html).toContain(t("view.provider_acquisition.sharing_withheld"));
  expect(html).not.toContain(CURRENT_TEMPLATE_ID);
  expect(html).not.toContain("2026-11-21");
  expect(renderToStaticMarkup(<ProviderAcquisition report={null} />)).toBe("");
});

test("Health retains a current-only failed acquisition independently of native or historical observations", () => {
  const acquisition = currentAcquisition(0);
  if (acquisition.kind !== "available") throw new Error("available");
  acquisition.evidence.native_disposition = "unconfirmed";
  acquisition.evidence.original_failure = { code: "connection_failed", stop: null };
  acquisition.settlement = t("view.provider_acquisition.native.unconfirmed");
  const report = healthReportFixture({
    provider_acquisition: acquisition,
    snapshot: { server: { engine: "postgres" }, sources: [], resources: [], pressure: null },
  });
  const html = renderToStaticMarkup(
    <HealthTab report={report} pressure={null} resources={[]} engineLabel="PostgreSQL" />,
  );
  expect(html).toContain(t("view.provider_acquisition.title"));
  expect(html).toContain("current-client-acquisition");
  expect(html).not.toContain(t("ui.health.empty"));
});
