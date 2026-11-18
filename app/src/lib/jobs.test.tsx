import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Jobs } from "../components/Jobs";
import { HealthTab } from "../components/Health";
import type { Report } from "../gen/Report";
import { partial } from "../test/fixtures";
import { JOBS_SOURCE, jobPoint, jobsEvidence, jobsReport, agentJobEvidence, agentJobsReport } from "../test/jobs";
import { t } from "./i18n";

test("job points keep exact native counts in collapsed paged text without rendering read-owner metadata", () => {
  const rows = Array.from({ length: 12 }, (_, index) => jobPoint(index));
  const html = renderToStaticMarkup(<Jobs evidence={jobsEvidence([...rows].reverse())} report={jobsReport(rows)} />);
  expect(html.match(/<article/g)).toHaveLength(10);
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  expect(html).toContain("9007199254740993");
  expect(html).toContain("2026-11-15T12:00:00.123456Z");
  expect(html).toContain("native_procedure&lt;script&gt;job-canary&lt;/script&gt;");
  expect(html).not.toContain("<script>");
  expect(html).not.toContain("private-read-host-canary");
  expect(html).not.toContain("private-read-database-canary");
  expect(html).not.toContain("sig-dot");
});

test("a missing, duplicate, foreign-source or different-job row cannot supply displayed job details", () => {
  const original = jobPoint();
  const otherSource = structuredClone(original); otherSource.source = "another-native-source";
  const otherJob = structuredClone(original);
  if (otherJob.fact.kind !== "timescale-job-state") throw new Error("Job state fixture required");
  otherJob.fact.data.job_id = "2000";
  for (const rows of [[], [original, original], [otherSource], [otherJob]]) {
    const html = renderToStaticMarkup(<Jobs evidence={jobsEvidence(rows)} report={jobsReport([original])} />);
    expect(html).toContain(t("ui.memory_failures.missing_row"));
    expect(html).not.toContain("9007199254740993");
    expect(html).not.toContain("native_procedure");
  }
});

test("failed scan acquisition stays visible under Health without a typed raw job wrapper", () => {
  const report = partial<Report>({ snapshot: { server: { engine: "postgres" }, sources: [{ source: JOBS_SOURCE, state: "unavailable", reason: "Native job permission denied." }],
    operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } } });
  const html = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
  expect(html).toContain(t("view.jobs.title"));
  expect(html).toContain("Native job permission denied.");
  expect(html).not.toContain(t("ui.health.empty"));
});

test("a view from different native read bounds cannot authorize job detail presentation", () => {
  const evidence = jobsEvidence();
  const report = jobsReport();
  report.context = { ...report.context, observed_until: "2026-11-15T12:00:02.123456Z" };
  const html = renderToStaticMarkup(<Jobs evidence={evidence} report={report} />);
  expect(html).toContain(t("ui.memory_failures.missing_row"));
  expect(html).not.toContain("9007199254740993");
});

test("Jobs assesses the native-failure opt-in while an explicitly recorded log-source gap remains visible", () => {
  const report = partial<Report>({ snapshot: { server: { engine: "postgres" }, options: { collect_native_failures: true },
    sources: [{ source: JOBS_SOURCE, state: "unavailable", reason: "Native job permission denied." }],
    operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null } } });
  const render = () => renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
  expect(render()).not.toContain(`aria-label="${t("view.native_failures.title")}"`);
  report.snapshot.sources!.push({ source: "native failures", state: "unavailable", reason: "Original native-log permission failure." });
  expect(render()).toContain("Original native-log permission failure.");
  expect(render()).toContain(`aria-label="${t("view.native_failures.title")}"`);
  report.snapshot.sources![1] = { source: "native failures", state: "not-applicable", reason: "An explicit native-log coverage qualification." };
  expect(render()).toContain("An explicit native-log coverage qualification.");
});

test("Agent history reuses folded exact text, local-clock limits and source binding without a Timescale window", () => {
  const evidence=agentJobEvidence();const report=agentJobsReport(evidence);
  const html=renderToStaticMarkup(<Jobs evidence={evidence} report={report}/>);
  expect(html).toContain("2147483647");expect(html).toContain("250101");expect(html).toContain("stored &lt;script&gt;agent-canary&lt;/script&gt;");expect(html).not.toContain("<script>");
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);expect(html).not.toContain("private-read-database-canary");expect(html).not.toContain("private-read-host-canary");
  expect(html).toContain("Job local times have no recorded offset.");
  report.entries[0]!.kind="state";
  const mismatch=renderToStaticMarkup(<Jobs evidence={evidence} report={report}/>);expect(mismatch).toContain(t("ui.memory_failures.missing_row"));expect(mismatch).not.toContain("2147483647");
});
test("Agent source-only gap cannot fall through to healthy Health emptiness", () => {
  const report=partial<Report>({snapshot:{server:{engine:"mssql"},sources:[{source:"mssql-agent-jobs",state:"unavailable",reason:"Native msdb visibility denied."}],operations:{spaces:[],counters:[],sequences:[],multixacts:[],sqlite:null,cluster:null}}});
  const html=renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="SQL Server"/>);
  expect(html).toContain(t("view.mssql_agent_jobs.title"));expect(html).toContain("Native msdb visibility denied.");expect(html).not.toContain(t("ui.health.empty"));
});

test("Agent not-applicable and acquired-without-view markers retain explicit Health coverage", () => {
  for (const state of ["not-applicable","collected"] as const) {
    const report=partial<Report>({snapshot:{server:{engine:"mssql"},sources:[{source:"mssql-agent-jobs",state,reason:null}],operations:{spaces:[],counters:[],sequences:[],multixacts:[],sqlite:null,cluster:null}}});
    const html=renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="SQL Server"/>);
    expect(html).toContain(t("view.mssql_agent_jobs.title"));expect(html).not.toContain(t("ui.health.empty"));
    if (state==="collected") expect(html).toContain(t("view.jobs.invalid"));
  }
});
