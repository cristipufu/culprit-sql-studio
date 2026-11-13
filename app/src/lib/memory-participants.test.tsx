import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { HealthTab } from "../components/Health";
import { RequestMemoryParticipants, SessionMemoryParticipants } from "../components/MemoryParticipants";
import { ObservationSourceIssue, observationReadIssue } from "../components/ObservationList";
import type { ExecutionGrantObservation } from "../gen/ExecutionGrantObservation";
import type { MemoryParticipantExplanation } from "../gen/MemoryParticipantExplanation";
import type { MemoryParticipantsReport } from "../gen/MemoryParticipantsReport";
import type { NativeIdentity } from "../gen/NativeIdentity";
import type { Report } from "../gen/Report";
import type { RequestMemorySnapshot } from "../gen/RequestMemorySnapshot";
import type { SessionMemoryObservation } from "../gen/SessionMemoryObservation";
import type { SessionMemorySnapshot } from "../gen/SessionMemorySnapshot";
import type { SourceOutcome } from "../gen/SourceOutcome";
import { partial } from "../test/fixtures";
import { t } from "./i18n";

function identity(id = "120"): NativeIdentity {
  return { id, qualifiers: { generation: "1770000000", namespace: "native-instance" } };
}

function grant(): ExecutionGrantObservation {
  return partial<ExecutionGrantObservation>({
    source: "current request memory", identity: identity(), observed_at: "2026-11-13T10:00:00.500000Z",
    state: "granted", request_association: "generation-checked", statement_relationship: "observed-current-statement",
    requested_without_offset: "2020-01-01T11:30:00.003", granted_without_offset: "2020-01-01T11:30:00.006",
    requested_kibibytes: "9007199254740993", required_kibibytes: "64", granted_kibibytes: "1024",
    used_kibibytes: "0", maximum_used_kibibytes: "16", ideal_kibibytes: null,
    wait_ms: "18446744073709551615", timeout_seconds: "25", degree_of_parallelism: "1",
    reserved_workers: "4", used_workers: "0", maximum_used_workers: "3",
    resource: { id: "2", qualifiers: { resource_semaphore_id: "0" } }, workload_group: null, native_properties: {},
    request: {
      key: { session: { id: "42", generation: "native-login-1" }, request: { id: "0", generation: "native-request-1" } },
      client: { application: "<img src=x onerror=alert(1)>" },
      statement: { state: "available", text: "SELECT '<script>alert(1)</script>'", sha256: "a".repeat(64), redaction_version: 1 },
    },
  });
}

function session(id = "120"): SessionMemoryObservation {
  return {
    source: "session memory", identity: identity(id), observed_at: "2026-11-13T10:00:00.500000Z",
    session: { id: "75", generation: id }, tracking_enabled: true,
    current_bytes: "18446744073709551615", controlled_current_bytes: "0", high_water_bytes: "18446744073709551615",
    controlled_high_water_bytes: null, native_properties: { INSTRUMENTED: "YES" },
    current_context: {
      request: { id: "5", generation: "999" }, observed_at: "2026-11-13T10:00:00.600000Z",
      database: "app", query_hash: "0123456789abcdef0123456789abcdef",
    },
  };
}

function explanation(native: NativeIdentity, index = 0): MemoryParticipantExplanation {
  return {
    observation_key: `backend-reference-${index}`, observation_identity: structuredClone(native),
    title: `Backend participant ${index}`, explanation: "The native participant is observed; the allocating statement is unresolved.",
    investigation: "Correlate matching native evidence without treating current context as the allocator.",
    verify: "Compare the same native measurement after a tested change.",
  };
}

function report(identities = [identity()]): MemoryParticipantsReport {
  return {
    title: "Observed native memory participants", summary: `${identities.length} retained participants; no causal owner established.`,
    source_state: "partial", source_reason: "Only a bounded native population was read.",
    limitations: ["A current statement is context, not proof of allocation ownership."],
    entries: identities.map(explanation),
  };
}

function requestEvidence(observations = [grant()]): RequestMemorySnapshot {
  return { source: "current request memory", observed_at: "2026-11-13T10:00:00.500000Z", observations };
}

function sessionEvidence(observations = [session()]): SessionMemorySnapshot {
  return {
    source: "session memory", observed_from: "2026-11-13T10:00:00.100000Z", observed_until: "2026-11-13T10:00:00.900000Z",
    resource: { engine: "mysql", kind: "instance", native: identity("native-instance"), namespace: identity("mysql-server") },
    observations,
  };
}

test("participant metadata joins exact native references despite reordered rows and qualifiers, without a frontend proof hash", () => {
  const row = grant();
  const other = grant();
  other.identity = identity("another-row");
  other.requested_kibibytes = "2999";
  const view = report();
  view.entries[0]!.observation_identity.qualifiers = { namespace: "native-instance", generation: "1770000000" };
  const html = renderToStaticMarkup(<RequestMemoryParticipants evidence={requestEvidence([other, row])} report={view} />);
  expect(html).toContain("9007199254740993 KiB");
  expect(html).not.toContain("2999 KiB");
  expect(html).toContain("Backend participant 0");
  expect(html).not.toContain("backend-reference-0");
});

test("different native generations, duplicate identities and wrong sources cannot borrow another row's counters", () => {
  const view = report();
  const changed = grant();
  changed.identity.qualifiers.generation = "another-generation";
  const duplicate = grant();
  duplicate.requested_kibibytes = "7";
  const wrongSource = grant();
  wrongSource.source = "another native source";
  for (const observations of [[changed], [grant(), duplicate], [wrongSource]]) {
    const html = renderToStaticMarkup(<RequestMemoryParticipants evidence={requestEvidence(observations)} report={view} />);
    expect(html).toContain(t("ui.memory_failures.missing_row"));
    expect(html).not.toContain("9007199254740993 KiB");
    expect(html).not.toContain("7 KiB");
  }
});

test("native grant units, large counts, zero and unknown remain exact without assigning UTC to local native clocks", () => {
  const html = renderToStaticMarkup(<RequestMemoryParticipants evidence={requestEvidence()} report={report()} />);
  expect(html).toContain("9007199254740993 KiB");
  expect(html).toContain("18446744073709551615 ms");
  expect(html).toContain("<dd>0 KiB</dd>");
  expect(html).toContain("<dd>Not reported</dd>");
  expect(html).toContain("Native request time (UTC offset unknown)");
  expect(html).toContain("2020-01-01T11:30:00.003");
  expect(html).not.toContain("2020-01-01T11:30:00.003Z");
  expect(html).toContain("2026-11-13T10:00:00.500000Z");
  expect(html).toContain("Reserved workers</dt><dd>4</dd>");
  expect(html).toContain("Workers currently used</dt><dd>0</dd>");
  expect(html).not.toContain("sig-dot");
  expect(html).not.toContain("Open in Queries");
});

test("statement context needs its typed native association and database-sourced text remains escaped", () => {
  const row = grant();
  const html = renderToStaticMarkup(<RequestMemoryParticipants evidence={requestEvidence([row])} report={report()} />);
  expect(html).toContain("Observed current statement context");
  expect(html).toContain("&lt;script&gt;");
  expect(html).toContain("&lt;img");
  expect(html).not.toContain("<script>");
  expect(html).not.toContain("<img ");
  row.statement_relationship = "unresolved";
  const unresolved = renderToStaticMarkup(<RequestMemoryParticipants evidence={requestEvidence([row])} report={report()} />);
  expect(unresolved).not.toContain("Observed current statement context");
  expect(unresolved).not.toContain("&lt;script&gt;");
});

test("session allocations retain exact bytes and independently observed context clocks without assigning an allocator", () => {
  const html = renderToStaticMarkup(<SessionMemoryParticipants evidence={sessionEvidence()} report={report()} />);
  expect(html).toContain("18446744073709551615 B");
  expect(html).toContain("<dd>0 B</dd>");
  expect(html).toContain("<dd>Not reported</dd>");
  expect(html).toContain("2026-11-13T10:00:00.500000Z");
  expect(html).toContain("2026-11-13T10:00:00.600000Z");
  expect(html).toContain("0123456789abcdef0123456789abcdef");
  expect(html).toContain("the allocating statement is unresolved");
  expect(html).not.toContain("sig-dot");
  expect(html).not.toContain("needs-attention");
});

test("disabled session tracking and unknown source completion cannot become healthy zero measurements", () => {
  const row = session();
  row.tracking_enabled = false;
  row.current_bytes = null;
  row.controlled_current_bytes = null;
  row.high_water_bytes = null;
  row.controlled_high_water_bytes = null;
  row.current_context = null;
  const view = report();
  view.source_state = null;
  view.source_reason = null;
  const html = renderToStaticMarkup(<SessionMemoryParticipants evidence={sessionEvidence([row])} report={view} />);
  expect(html.slice(0, html.indexOf("<details>"))).toContain(t("ui.observation_list.source_unknown"));
  expect(html).toContain("Native tracking currently enabled</dt><dd>No</dd>");
  expect(html).toContain(t("view.session_memory.context.unavailable"));
  expect(html).not.toContain("<dd>0 B</dd>");
  expect(html).not.toContain("healthy");
});

test("participant records start collapsed and render only the first bounded page", () => {
  const rows = Array.from({ length: 12 }, (_, index) => session(String(index)));
  const view = report(rows.map((row) => row.identity));
  const html = renderToStaticMarkup(<SessionMemoryParticipants evidence={sessionEvidence(rows)} report={view} />);
  expect(html).toContain("<details><summary>Inspect retained participants</summary>");
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  expect(html.match(/class="operational-card"/g)).toHaveLength(10);
  expect(html).toContain("Observations 1–10 of 12");
  expect(html).toContain("Backend participant 9");
  expect(html).not.toContain("Backend participant 10");
});

test("requested missing and unsupported sources remain explicit while unrequested profiles add no issue", () => {
  expect(observationReadIssue("session memory", [], false)).toBeUndefined();
  expect(observationReadIssue("session memory", [], true)).toEqual({
    source: "session memory", state: "unavailable", reason: t("ui.observation_list.source_unknown"),
  });
  const unavailable: SourceOutcome = { source: "session memory", state: "not-applicable", reason: "The adapter has no safe native source." };
  expect(observationReadIssue("session memory", [unavailable], true)).toBe(unavailable);
  expect(observationReadIssue("session memory", [unavailable], false)).toBeUndefined();
  const positive: SourceOutcome = { source: "session memory", state: "partial", reason: "Some native sessions were retained." };
  expect(observationReadIssue("session memory", [positive])).toBe(positive);
});

test("a different canonical source cannot hide an exact custom failure and unknown reasons stay format-neutral", () => {
  const collected: SourceOutcome = { source: "session memory", state: "collected", reason: null };
  const failed: SourceOutcome = { source: "SESSION_MEMORY", state: "unavailable", reason: null };
  expect(observationReadIssue(failed.source, [collected, failed], true)).toBe(failed);
  expect(observationReadIssue(collected.source, [collected, failed], true)).toBeUndefined();
  const html = renderToStaticMarkup(<ObservationSourceIssue source={failed} title="Requested native source" />);
  expect(html).toContain('aria-label="Requested native source"');
  expect(html).toContain(t("ui.observation_list.source_unknown"));
  expect(html).not.toContain(t("view.memory_failures.limit.source_unknown"));
});

test("Health shows requested participant profiles with unknown source coverage instead of omitting their panels", () => {
  const value = partial<Report>({
    snapshot: {
      server: { engine: "mysql" }, options: { collect_request_memory: true, collect_session_memory: true }, sources: [],
      operations: { spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null },
    },
  });
  const html = renderToStaticMarkup(<HealthTab pressure={null} resources={[]} report={value} engineLabel="MySQL" />);
  expect(html).toContain(t("view.request_memory.title"));
  expect(html).toContain(t("view.session_memory.title"));
  expect(html).toContain(t("ui.observation_list.source_unknown"));
  expect(html).toContain('aria-label="' + t("view.session_memory.title") + '"');
  expect(html).not.toContain(t("ui.health.empty"));
});
