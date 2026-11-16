import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { CurrentRequestWork } from "../components/CurrentRequestWork";
import { QueryProgressMeters } from "../components/QueryProgressMeters";
import { currentWorkReport, progressObservation, progressPlan, sessionProgressContext } from "../test/query-progress";
import { t } from "./i18n";
import { ProgressPlanCache, progressFraction } from "./query-progress";

test("only one native request may reuse its plan and a same-SQL replacement gets one fresh plan attempt", () => {
  const cache = new ProgressPlanCache();
  const plan = progressPlan();
  const initial = cache.beginRead();
  expect(initial.withPlan).toBe(true);
  expect(cache.accept(progressObservation({ plan }), initial).plan).toBe(plan);
  const stable = cache.beginRead();
  expect(stable.withPlan).toBe(false);
  expect(cache.accept(progressObservation(), stable).plan).toBe(plan);

  const replacement = progressObservation({ request_key: "native-request-b" });
  expect(replacement.statement).toBe(progressObservation().statement);
  expect(cache.accept(replacement, cache.beginRead()).plan).toBeNull();
  const fresh = cache.beginRead();
  expect(fresh).toEqual({ withPlan: true, requestKey: "native-request-b" });
  cache.accept(replacement, fresh);
  expect(cache.beginRead().withPlan).toBe(false);
});

test("a request that changes during a plan read gets its own finite request rather than the preceding attempt", () => {
  const cache = new ProgressPlanCache();
  cache.accept(progressObservation({ plan: progressPlan() }), cache.beginRead());
  cache.accept(progressObservation({ request_key: "native-request-b" }), cache.beginRead());
  const readingB = cache.beginRead();
  expect(readingB.withPlan).toBe(true);
  const requestC = progressObservation({ request_key: "native-request-c" });
  cache.accept(requestC, readingB);
  const readingC = cache.beginRead();
  expect(readingC).toEqual({ withPlan: true, requestKey: "native-request-c" });
  cache.accept(requestC, readingC);
  expect(cache.beginRead().withPlan).toBe(false);
});

test("unavailable, independent and idle points clear plan ownership even when they carry a stray plan", () => {
  for (const point of [
    progressObservation({ running: null, request_key: null }),
    progressObservation({ session_context: sessionProgressContext(), request_key: null }),
    progressObservation({ running: false }),
  ]) {
    const cache = new ProgressPlanCache();
    const plan = progressPlan();
    cache.accept(progressObservation({ plan }), cache.beginRead());
    expect(cache.accept({ ...point, plan }, cache.beginRead()).plan).toBeNull();
    for (let index = 0; index < 3; index++) {
      const read = cache.beginRead();
      expect(read.withPlan).toBe(false);
      cache.accept(point, read);
    }
    expect(cache.accept(progressObservation(), cache.beginRead()).plan).toBeNull();
  }
});

test("an unavailable initial point does not repeatedly request plans and a later native request gets one attempt", () => {
  const cache = new ProgressPlanCache();
  const initial = cache.beginRead();
  cache.accept(progressObservation({ running: null, request_key: null }), initial);
  expect(initial.withPlan).toBe(true);
  for (let index = 0; index < 3; index++) {
    const read = cache.beginRead();
    expect(read.withPlan).toBe(false);
    cache.accept(progressObservation({ running: null, request_key: null }), read);
  }
  cache.accept(progressObservation(), cache.beginRead());
  const native = cache.beginRead();
  expect(native.withPlan).toBe(true);
  cache.accept(progressObservation(), native);
  expect(cache.beginRead().withPlan).toBe(false);
});

test("a fresh plan without an ownership key belongs only to its original point and cannot be borrowed later", () => {
  const cache = new ProgressPlanCache();
  const plan = progressPlan();
  expect(cache.accept(progressObservation({ plan, request_key: null }), cache.beginRead()).plan).toBe(plan);
  expect(cache.accept(progressObservation({ request_key: null }), cache.beginRead()).plan).toBeNull();
  expect(cache.beginRead().withPlan).toBe(false);
});

test("phase progress has its own caption and never becomes whole-statement completion", () => {
  const phase = renderToStaticMarkup(<QueryProgressMeters progress={progressObservation({ phase: "Sorting <native>", phase_percent: 0.65 })} />);
  expect(phase).toContain('data-progress-scope="phase"');
  expect(phase).toContain('aria-valuenow="65"');
  expect(phase).toContain(t("ui.query_progress.phase_only"));
  expect(phase).toContain("Sorting &lt;native&gt;");
  expect(phase).not.toContain('data-progress-scope="statement"');
  expect(phase).not.toContain(t("ui.query_progress.basis_engine"));

  const statement = renderToStaticMarkup(<QueryProgressMeters progress={progressObservation({ percent: 0 })} />);
  expect(statement).toContain('data-progress-scope="statement"');
  expect(statement).toContain('aria-valuenow="0"');
  for (const invalid of [null, Number.NaN, Infinity, -0.1, 1.01]) expect(progressFraction(invalid)).toBeNull();
  expect(progressFraction(1)).toBe(1);
});

test("current request work renders the shared explanation and exact native metric text inside folded evidence", () => {
  const report = currentWorkReport();
  const html = renderToStaticMarkup(<CurrentRequestWork report={report} />);
  expect(html).toContain("9007199254740993");
  expect(html).toContain(report.summary);
  expect(html).toContain(report.limitations[0]!);
  expect(html).toContain(report.source_reason!);
  expect(html).toContain("native-session-generation");
  expect(html).toContain("2026-11-16T10:00:01Z");
  expect(html).toContain("&lt;script&gt;canary&lt;/script&gt;");
  expect(html).not.toContain("<script>");
  expect(html).not.toContain("private-route-canary");
  expect(html).not.toContain("private-statement-authority");
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  expect(html).toContain(t("ui.statement_grants.unavailable", { reason: "not-collected" }));

  report.entries[0]!.statement = { state: "available", text: "SELECT '<script>native-sql-canary</script>'", sha256: "a".repeat(64), redaction_version: 1 };
  const statement = renderToStaticMarkup(<CurrentRequestWork report={report} />);
  expect(statement).toContain("native-sql-canary");
  expect(statement).toContain("&lt;script&gt;");
  expect(statement).not.toContain("<script>");
});
