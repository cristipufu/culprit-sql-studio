import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { MemoryFailureSourceIssue, RetainedMemoryFailures, memoryFailureReadIssue } from "../components/RetainedMemoryFailures";
import type { RetainedFailureReport } from "../gen/RetainedFailureReport";
import type { MemoryFailureSnapshot } from "../gen/MemoryFailureSnapshot";
import type { SourceOutcome } from "../gen/SourceOutcome";
import { partial } from "../test/fixtures";

function nativeEvidence(positions = [0]): MemoryFailureSnapshot {
  return partial<MemoryFailureSnapshot>({
    source: "native-memory-failures",
    observed_from: "2026-11-13T08:00:00Z",
    observed_until: "2026-11-13T08:00:01Z",
    scope: "server",
    resource: null,
    observations: positions.map((position) => ({
      position,
      event_time_without_offset: "2099-01-01T11:30:00.003",
      oom_cause: 255,
      oom_cause_desc: "UNRECOGNIZED_NATIVE_CODE",
      oom_factor: 11,
      oom_factor_desc: "UNINTERPRETED_INTERNAL_FACTOR",
      available_physical_memory_mb: -1,
      process_memory_usage_mb: 2_147_483_647,
      non_sos_memory_usage_mb: 64,
      committed_memory_target_mb: 1024,
      committed_memory_mb: 1000,
      allocation_potential_memory_mb: -1,
      initial_job_object_memory_limit_mb: null,
      current_job_object_memory_limit_mb: 0,
      possible_non_sos_leaked_memory_mb: null,
      oom_resource_pools: { raw: "null", qualification: "parsed" },
      top_memory_clerks: { raw: '<script>alert("owner")</script>', qualification: "unparsed" },
      top_resource_pools: { raw: '{"unknown-shape":"<img src=x onerror=alert(1)>"}', qualification: "parsed" },
      possible_leaked_memory_clerks: null,
    })),
  });
}

function explanation(positions = [0]): RetainedFailureReport {
  return {
    title: "Retained memory failures",
    summary: `${positions.length} retained observations; no incident count established.`,
    source_state: "partial",
    source_reason: "The native source returned only bounded retained context.",
    limitations: ["Affected statements and memory owners are unresolved.", "Native read completeness was not established."],
    entries: positions.map((position) => ({
      position,
      title: "Backend-selected unknown native cause",
      explanation: "The native cause remains uninterpreted.",
      investigation: "Establish the native clock and resource before correlating other observations.",
      verify: "Compare bounded source evidence after a measured change.",
    })),
  };
}

function show(evidence = nativeEvidence(), report = explanation()): string {
  return renderToStaticMarkup(<RetainedMemoryFailures evidence={evidence} report={report} />);
}

test("memory source issue aliases retain actual partial results and failed regions remain named", () => {
  const source: SourceOutcome = { source: "MEMORY_FAILURES", state: "partial", reason: "Native positive observations were incomplete." };
  expect(memoryFailureReadIssue(undefined, [source])).toBe(source);
  expect(source.state).toBe("partial");
  expect(memoryFailureReadIssue("memory-failures", [source])).toBeUndefined();
  const html = renderToStaticMarkup(<MemoryFailureSourceIssue source={source} />);
  expect(html).toContain('aria-label="Retained memory failures"');
  expect(html).toContain('class="operational-health retained-failures"');
  expect(html).toContain("Native positive observations were incomplete.");
});

test("retained memory observations render backend explanation without becoming a database opportunity or owner", () => {
  const html = show();
  expect(html).toContain("Backend-selected unknown native cause");
  expect(html).toContain("The native cause remains uninterpreted.");
  expect(html).toContain("UNRECOGNIZED_NATIVE_CODE");
  expect(html).toContain("UNINTERPRETED_INTERNAL_FACTOR");
  expect(html).toContain("Affected statements and memory owners are unresolved.");
  expect(html).toContain("only bounded retained context");
  expect(html).not.toContain("sig-dot");
  expect(html).not.toContain("needs-attention");
  expect(html).not.toContain("Open in Queries");
  expect(html).toContain("<details><summary>Investigation and native evidence</summary>");
});

test("native local time is preserved separately from UTC capture bounds without conversion or current-window claim", () => {
  const html = show();
  expect(html).toContain("2099-01-01T11:30:00.003");
  expect(html).toContain("UTC offset unknown");
  expect(html).toContain("2026-11-13T08:00:00Z");
  expect(html).toContain("2026-11-13T08:00:01Z");
  expect(html).not.toContain("2099-01-01T11:30:00.003Z");
  expect(html).not.toContain("right now");
});

test("signed native MB remain exact and zero stays distinct from an unreported optional value", () => {
  const html = show();
  expect(html).toContain("-1 MB");
  expect(html).toContain("2147483647 MB");
  expect(html).toContain('title="current_job_object_memory_limit_mb"');
  expect(html).toContain("<dd>0 MB</dd>");
  expect(html).toContain('title="initial_job_object_memory_limit_mb"');
  expect(html).toContain("<dd>Not reported</dd>");
  expect(html).not.toContain("2.1B MB");
  expect(html).not.toContain("GB");
});

test("opaque native documents are escaped text and distinguish SQL NULL from JSON null and unparsed input", () => {
  const html = show();
  expect(html).toContain("SQL NULL: no document supplied.");
  expect(html).toContain('>null</pre>');
  expect(html).toContain("could not be parsed within the supported JSON bounds");
  expect(html).toContain("&lt;script&gt;");
  expect(html).toContain("&lt;img");
  expect(html).not.toContain("<script>");
  expect(html).not.toContain("<img ");
  expect(html).not.toContain("data-owner");
});

test("equal native rows retain distinct capture-local positions and render without incident deduplication", () => {
  const positions = [8, 2];
  const html = show(nativeEvidence(positions), explanation(positions));
  expect(html.match(/class="operational-card"/g)).toHaveLength(2);
  expect(html.match(/Backend-selected unknown native cause/g)).toHaveLength(2);
  expect(html).toContain("2 retained observations; no incident count established.");
});

test("a missing referenced row cannot display an apparently matched explanation or invented measurements", () => {
  const html = show(nativeEvidence(), explanation([12]));
  expect(html).toContain("retained native row for this explanation is unavailable");
  expect(html).not.toContain("Backend-selected unknown native cause");
  expect(html).not.toContain("2147483647 MB");
});

test("an unqualified native scope never defaults to a shared hosting-engine identity", () => {
  const evidence = nativeEvidence();
  evidence.scope = "unknown";
  const html = show(evidence);
  expect(html).toContain("Resource scope not established");
  expect(html).not.toContain("Shared hosting-engine context");
});

test("a large retained buffer renders only one page while preserving the native observation count", () => {
  const positions = Array.from({ length: 11 }, (_, position) => position);
  const html = show(nativeEvidence(positions), explanation(positions));
  expect(html.match(/class="operational-card"/g)).toHaveLength(10);
  expect(html).toContain("11 retained observations");
  expect(html).toContain("Observations 1–10 of 11");
});

test("a failed or partial read remains visible when no native wrapper or explanation was admitted", () => {
  for (const state of ["unavailable", "partial"] as const) {
    const source: SourceOutcome = { source: "memory failures", state, reason: "Permission or bounded completion was not established." };
    const issue = memoryFailureReadIssue(undefined, [source]);
    expect(issue).toBe(source);
    const html = renderToStaticMarkup(<MemoryFailureSourceIssue source={issue!} />);
    expect(html).toContain("Retained memory failures");
    expect(html).toContain(state === "partial" ? "This read was partial." : "Not available here");
    expect(html).toContain("Permission or bounded completion was not established.");
    expect(html).toContain("<details><summary>What this evidence establishes</summary>");
    expect(html).not.toContain("No retained rows");
    expect(html).not.toContain("0 failures");
  }
  const custom: SourceOutcome = { source: "custom-native-memory", state: "unavailable", reason: null };
  expect(memoryFailureReadIssue(custom.source, [custom])).toBe(custom);
});

test("default unrequested reads and unrelated source failures do not add memory warnings", () => {
  for (const state of ["not-applicable", "empty", "collected"] as const) {
    expect(memoryFailureReadIssue(undefined, [{ source: "memory failures", state, reason: null }])).toBeUndefined();
  }
  expect(memoryFailureReadIssue(undefined, [{ source: "some-other-source", state: "unavailable", reason: null }])).toBeUndefined();
});

test("an explicit unsupported memory diagnostic request remains visible without creating an empty read", () => {
  const source: SourceOutcome = { source: "memory failures", state: "not-applicable", reason: "This provider has no native reader for these records." };
  expect(memoryFailureReadIssue(undefined, [source], true)).toBe(source);
  const html = renderToStaticMarkup(<MemoryFailureSourceIssue source={source} />);
  expect(html).toContain(source.reason!);
  expect(html).not.toContain("0 failures");
});
