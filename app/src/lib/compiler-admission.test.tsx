import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { CompilerAdmission } from "../components/CompilerAdmission";
import { HealthTab } from "../components/Health";
import type { NativeReadContext } from "../gen/NativeReadContext";
import { compilerAdmissionEvidence, compilerAdmissionReport, compilerGateway, compilerWaiter, COMPILER_ADMISSION_SOURCE } from "../test/compiler-admission";
import { partial } from "../test/fixtures";
import { t } from "./i18n";
import { healthReportFixture } from "../test/health-report";

test("compiler admission keeps separate affected requests and exact gateway values inside collapsed paged evidence", () => {
  const rows = [compilerWaiter(), ...Array.from({ length: 12 }, (_, index) => compilerGateway(index))];
  const html = renderToStaticMarkup(<CompilerAdmission evidence={compilerAdmissionEvidence([...rows].reverse())} report={compilerAdmissionReport(rows)} />);
  expect(html.match(/<article/g)).toHaveLength(10);
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  expect(html).toContain("9007199254740993");
  expect(html).toContain("original-request");
  expect(html).toContain(t("ui.compiler_admission.window_unknown"));
  expect(html).toContain("native-client&lt;script&gt;canary&lt;/script&gt;");
  expect(html).not.toContain("compiler_private_sql_body_canary");
  expect(html).not.toContain("<script>");
  expect(html).not.toContain("sig-dot");
});

test("same numeric actors with replaced generations, other sources and duplicate row keys cannot supply compiler context", () => {
  const original = compilerWaiter();
  const replaced = compilerWaiter();
  if (replaced.fact.kind !== "compiler-waiter") throw new Error("waiter fixture");
  replaced.fact.data.request.key.request!.generation = "replacement-request";
  const otherSource = compilerWaiter();
  otherSource.source = "other-native-source";
  for (const rows of [[replaced], [otherSource], [original, original]]) {
    const html = renderToStaticMarkup(<CompilerAdmission evidence={compilerAdmissionEvidence(rows)} report={compilerAdmissionReport([original])} />);
    expect(html).toContain(t("ui.memory_failures.missing_row"));
    expect(html).not.toContain("native-client");
    expect(html).not.toContain("Original request is waiting to compile");
  }
  const gateway = compilerGateway();
  if (gateway.fact.kind !== "counter-set") throw new Error("gateway fixture");
  const view = structuredClone(compilerAdmissionReport([gateway]));
  gateway.fact.data.identity.qualifiers.resource_pool_id = "99";
  const html = renderToStaticMarkup(<CompilerAdmission evidence={compilerAdmissionEvidence([gateway])} report={view} />);
  expect(html).toContain(t("ui.memory_failures.missing_row"));
  expect(html).not.toContain("9007199254740993");
});

test("native read bounds can be shown without leaking the source owner and failed Health acquisition remains visible", () => {
  const evidence = compilerAdmissionEvidence();
  evidence.context = partial<NativeReadContext>({ observed_from: "2026-11-16T10:00:00Z", observed_until: "2026-11-16T10:00:01Z", target: { scope: "database", identity: { engine: "mssql", database: "private-read-route-canary" } } });
  const html = renderToStaticMarkup(<CompilerAdmission evidence={evidence} report={compilerAdmissionReport()} />);
  expect(html).toContain("2026-11-16T10:00:01Z");
  expect(html).not.toContain("private-read-route-canary");
  const report = healthReportFixture({ snapshot: { server: { engine: "mssql" }, sources: [{ source: COMPILER_ADMISSION_SOURCE, state: "unavailable", reason: "Native compiler permission denied." }],
    operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } } });
  const failed = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="SQL Server" />);
  expect(failed).toContain(t("view.compiler_admission.title"));
  expect(failed).toContain("Native compiler permission denied.");
  expect(failed).not.toContain(t("ui.health.empty"));
});
