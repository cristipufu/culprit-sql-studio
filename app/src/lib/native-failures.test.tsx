import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { NativeFailureSourceIssue, RetainedNativeFailures, nativeFailureReadIssue } from "../components/RetainedNativeFailures";
import type { NativeFailureSnapshot } from "../gen/NativeFailureSnapshot";
import type { RetainedFailureReport } from "../gen/RetainedFailureReport";
import type { SourceOutcome } from "../gen/SourceOutcome";

function evidence(positions = [0]): NativeFailureSnapshot {
  return {
    source: "native failures",
    observed_from: "2026-11-13T10:00:00Z",
    observed_until: "2026-11-13T10:00:01Z",
    resource: null,
    observations: positions.map((position) => ({
      native: "mysql-error-log",
      observation: {
        position,
        logged_at: "2020-01-01T01:02:03.000001Z",
        thread_id: "18446744073709551615",
        priority: "Error",
        error_code: "MY-010935",
        subsystem: "Server",
        message: "private application SQL with a secret",
      },
    })),
  };
}

function explanation(positions = [0]): RetainedFailureReport {
  return {
    title: "Retained native diagnostics",
    summary: `${positions.length} retained diagnostic records; no incident count established.`,
    source_state: "partial",
    source_reason: "The native source retained a bounded subset.",
    limitations: ["Query ownership remains unresolved.", "The buffer does not establish complete history."],
    entries: positions.map((position) => ({
      position,
      title: "Classification from the backend",
      explanation: "The source does not identify a historical query.",
      investigation: "Compare the same instance and period.",
      verify: "Measure the effect of a tested change.",
    })),
  };
}

function show(native = evidence(), report = explanation()): string {
  return renderToStaticMarkup(<RetainedNativeFailures evidence={native} report={report} />);
}

function mysqlRow(snapshot: NativeFailureSnapshot, index = 0) {
  const row = snapshot.observations[index];
  if (row?.native !== "mysql-error-log") throw new Error("Expected native MySQL fixture metadata");
  return row.observation;
}

test("native diagnostic explanations come from the backend and never expose omitted messages or invent query ownership", () => {
  const html = show();
  expect(html).toContain("Classification from the backend");
  expect(html).toContain("MY-010935");
  expect(html).toContain("Query ownership remains unresolved.");
  expect(html).toContain("does not identify a historical query");
  expect(html).not.toContain("private application SQL");
  expect(html).not.toContain("secret");
  expect(html).not.toContain("sig-dot");
  expect(html).not.toContain("Open in Queries");
});

test("qualified event UTC and exact unsigned connection identity remain distinct from read bounds and current sessions", () => {
  const html = show();
  expect(html).toContain("2020-01-01T01:02:03.000001Z");
  expect(html).toContain("Native event time (UTC)");
  expect(html).toContain("2026-11-13T10:00:00Z");
  expect(html).toContain("18446744073709551615");
  expect(html).toContain("This number can be reused");
  expect(html).not.toContain("current query");
  expect(html).not.toContain("18446744073709552000");
});

test("native labels are escaped text and free-form messages remain undisplayed even in an externally supplied DTO", () => {
  const native = evidence();
  const row = mysqlRow(native);
  row.priority = "<script>";
  row.error_code = "<svg>";
  row.subsystem = "<img>";
  row.message = "<script>private()</script>";
  const html = show(native);
  expect(html).toContain("&lt;script&gt;");
  expect(html).toContain("&lt;svg&gt;");
  expect(html).toContain("&lt;img&gt;");
  expect(html).not.toContain("<script>");
  expect(html).not.toContain("<svg>");
  expect(html).not.toContain("private()");
});

test("native connection zero stays distinct from SQL NULL without inventing a session generation", () => {
  const native = evidence([8, 2]);
  mysqlRow(native).thread_id = "0";
  mysqlRow(native, 1).thread_id = null;
  const html = show(native, explanation([8, 2]));
  expect(html).toContain("<code>0</code>");
  expect(html).toContain("<code>Unknown</code>");
  expect(html.match(/class="operational-card"/g)).toHaveLength(2);
  expect(html).not.toContain("data-session");
});

test("native observations are collapsed and paged by shared presentation rather than flooding Health", () => {
  const positions = Array.from({ length: 11 }, (_, position) => position);
  const html = show(evidence(positions), explanation(positions));
  expect(html).toContain("<details><summary>Inspect retained observations</summary>");
  expect(html).not.toContain("<details open");
  expect(html.match(/class="operational-card"/g)).toHaveLength(10);
  expect(html).toContain("Observations 1–10 of 11");
  expect(html).toContain("11 retained diagnostic records");
});

test("missing native rows and unqualified resource identities cannot acquire invented evidence", () => {
  const html = show(evidence(), explanation([2]));
  expect(html).toContain("The retained native row for this explanation is unavailable");
  expect(html).not.toContain("MY-010935");
  expect(html).toContain("Resource scope not established");
  const native = evidence();
  native.resource = { engine: "mysql", kind: "pool", namespace: { id: "unknown", qualifiers: {} }, native: { id: "unknown", qualifiers: {} } };
  expect(show(native)).not.toContain("Shared hosting-engine context");
});

test("failed and explicitly unsupported requested source profiles remain visible without treating default opt-out as a failure", () => {
  const unavailable: SourceOutcome = { source: "native failures", state: "unavailable", reason: "Native source denied access." };
  expect(nativeFailureReadIssue(undefined, [unavailable])).toBe(unavailable);
  expect(nativeFailureReadIssue("other-source", [unavailable])).toBeUndefined();
  const notApplicable: SourceOutcome = { ...unavailable, state: "not-applicable", reason: "The native source does not apply to this product." };
  expect(nativeFailureReadIssue(undefined, [notApplicable])).toBeUndefined();
  expect(nativeFailureReadIssue(undefined, [notApplicable], true)).toBe(notApplicable);
  const html = renderToStaticMarkup(<NativeFailureSourceIssue source={notApplicable} />);
  expect(html).toContain('aria-label="Retained native diagnostics"');
  expect(html).toContain("This source is not available for the requested profile.");
  expect(html).toContain("does not apply to this product");
  expect(html).not.toContain("No retained diagnostic rows");
});

test("native source issues preserve exact canonical and custom source identities", () => {
  const native: SourceOutcome = { source: "native failures", state: "partial", reason: "Positive retained rows were incomplete." };
  expect(nativeFailureReadIssue(undefined, [native])).toBe(native);
  expect(native.state).toBe("partial");
  expect(native.source).toBe("native failures");
  const custom: SourceOutcome = { ...native, source: "Custom Ä SOURCE" };
  expect(nativeFailureReadIssue(custom.source, [custom])).toBe(custom);
  expect(nativeFailureReadIssue("custom_Ä_source", [custom])).toBeUndefined();
  expect(nativeFailureReadIssue("custom_ä_source", [custom])).toBeUndefined();
  expect(nativeFailureReadIssue("native-failures", [native])).toBeUndefined();
  expect(nativeFailureReadIssue("native\u00a0failures", [native])).toBeUndefined();
});

test("PostgreSQL metadata preserves unqualified local time and signed identities without MySQL labels or owner inference", () => {
  const native = evidence();
  native.observations = [{
    native: "postgres-log",
    observation: {
      position: 0, timestamp: "2026-11-13 12:00:00.123 CEST", timestamp_utc: null,
      sql_state: "53200", severity: "ERROR", database: "shop", process_id: "71", session_id: "5f0b.47",
      session_start: "2026-11-13 11:00:00 CEST", session_line_number: "9007199254740993",
      virtual_transaction_id: null, transaction_id: null, query_id: "-9223372036854775808",
      leader_process_id: null, backend_type: "client backend",
      statement: { state: "available", text: "SELECT <redacted>", sha256: "0".repeat(64), redaction_version: 1 },
      deadlock: null,
    },
  }];
  const html = show(native);
  expect(html).toContain("2026-11-13 12:00:00.123 CEST");
  expect(html).toContain("UTC offset not established");
  expect(html).toContain("53200");
  expect(html).toContain("9007199254740993");
  expect(html).toContain("-9223372036854775808");
  expect(html).toContain("Reporting process ID");
  expect(html).toContain("Statement reported with this error");
  expect(html).toContain("SELECT &lt;redacted&gt;");
  expect(html).not.toContain("Native connection ID");
  expect(html).not.toContain("Native subsystem");
  expect(html).not.toContain("MY-010935");
});
