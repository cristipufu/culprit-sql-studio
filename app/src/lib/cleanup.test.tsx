import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import english from "../../../locales/en.json" with { type: "json" };
import { CleanupEvidence } from "../components/CleanupEvidence";
import { HealthTab } from "../components/Health";
import type { CleanupReport } from "../gen/CleanupReport";
import type { CleanupSnapshot } from "../gen/CleanupSnapshot";
import type { Report } from "../gen/Report";
import type { RetentionSnapshot } from "../gen/RetentionSnapshot";
import {
  EXACT_REASON_COUNT, NATIVE_COLUMN_CANARY, PREPARED_AT, PVS_SOURCE,
  persistentVersionObservation, persistentVersionRead, persistentVersionReport,
  preparedRetainer, retainer, retentionEvidence, retentionReport,
} from "../test/cleanup";
import { partial } from "../test/fixtures";

function show(reads: CleanupSnapshot[], retention: RetentionSnapshot | null, report: CleanupReport): string {
  return renderToStaticMarkup(<CleanupEvidence reads={reads} retention={retention} report={report} />);
}

test("PVS retains exact native reason counters and escaped column names inside collapsed evidence", () => {
  const html = show([persistentVersionRead()], null, persistentVersionReport());
  expect(html).toContain(EXACT_REASON_COUNT);
  expect(html).not.toContain("9007199254740992");
  expect(html).toContain("pvs_off_row_page_skipped_oldest_active_xdesid");
  expect(html).toContain("&lt;script&gt;native-column-canary&lt;/script&gt;");
  expect(html).not.toContain("<script>");
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  expect(html).toContain("2026-11-13T10:00:00Z");
  expect(html).toContain("2026-11-13T10:00:01Z");
});

test("native points require matching read and row sources and duplicate keys stay ambiguous", () => {
  const native = persistentVersionObservation();
  const wrongRow = { ...native, source: "unrelated-source" };
  const report = persistentVersionReport(native);
  for (const reads of [
    [{ ...persistentVersionRead(), source: "unrelated-source" }],
    [persistentVersionRead([wrongRow])],
    [persistentVersionRead([native, structuredClone(native)])],
  ]) {
    const html = show(reads, null, report);
    expect(html).toContain(english["ui.memory_failures.missing_row"]);
    expect(html).not.toContain(EXACT_REASON_COUNT);
    expect(html).not.toContain(NATIVE_COLUMN_CANARY);
  }
  // The same key in a different source must not make this exact source's record ambiguous.
  const html = show([persistentVersionRead(), { ...persistentVersionRead([wrongRow]), source: wrongRow.source }], null, report);
  expect(html).toContain(EXACT_REASON_COUNT);
  expect(html).not.toContain(english["ui.memory_failures.missing_row"]);
});

test("retainer input order does not replace full generations and the initial page remains bounded", () => {
  const holders = Array.from({ length: 12 }, (_, index) => retainer(index));
  const html = show([], retentionEvidence([...holders].reverse()), retentionReport(holders));
  expect(html.match(/<article/g)).toHaveLength(10);
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
  expect(html).toContain("Native retainer 0");
  expect(html).not.toContain("Native retainer 10");
  expect(html).not.toContain("Native retainer 11");
  expect(html.indexOf("Native retainer 0")).toBeLessThan(html.indexOf("Native retainer 1"));
  expect(html).toContain(holders[0]!.session!.generation);
  expect(html).toContain(holders[9]!.session!.generation);
  expect(html).not.toContain(english["ui.memory_failures.missing_row"]);
});

test("a recycled PID wrong source or duplicate native identity cannot supply another retainer's context", () => {
  const original = retainer(0);
  const recycled = retainer(1);
  const wrongSource = { ...original, source: "unrelated-retainer-source" };
  for (const holders of [[recycled], [wrongSource], [original, structuredClone(original)]]) {
    const html = show([], retentionEvidence(holders), retentionReport([original]));
    expect(html).toContain(english["ui.memory_failures.missing_row"]);
    expect(html).not.toContain("native-xid-0");
    expect(html).not.toContain(original.session!.generation);
  }
});

test("prepared time stays separate from an unknown transaction start and session generation", () => {
  const holder = preparedRetainer();
  const html = show([], retentionEvidence([holder]), retentionReport([holder]));
  expect(html).toContain(`<dt>${english["ui.cleanup.transaction_prepared"]}</dt><dd>${PREPARED_AT}</dd>`);
  expect(html).toContain(`<dt>${english["ui.cleanup.transaction_started"]}</dt><dd>${english["ui.memory_failures.not_reported"]}</dd>`);
  expect(html).toContain(`<dt>${english["ui.cleanup.native_session"]}</dt><dd>${english["ui.memory_failures.not_reported"]}</dd>`);
  expect(html).not.toContain("<code>generation</code>");
  expect(html).not.toContain("<code>transaction_start</code>");
});

test("an unresolved native session number remains visible without acquiring a generation or query link", () => {
  const holder = { ...retainer(0), holder: "replication-slot" as const,
    identity: { id: "unverified-slot", qualifiers: { native_id: "slot-a" } },
    session: null, query: null, transaction: null, unresolved_session_id: "90241" };
  const html = show([], retentionEvidence([holder]), retentionReport([holder]));
  expect(html).toContain(`<dt>${english["ui.cleanup.unresolved_session"]}</dt><dd>90241</dd>`);
  expect(html).toContain(`<dt>${english["ui.cleanup.native_session"]}</dt><dd>${english["ui.memory_failures.not_reported"]}</dd>`);
  expect(html).not.toContain("<code>generation</code>");
  expect(html).not.toContain("<a ");
});

test("failed reads and raw evidence without an admitted explanation remain visible instead of healthy empty", () => {
  for (const failed of [true, false]) {
    const report = partial<Report>({ snapshot: {
      server: { engine: "mssql" },
      sources: [{ source: PVS_SOURCE, state: failed ? "unavailable" : "collected", reason: failed ? "Native cleanup permission denied." : null }],
      operations: { cleanup: failed ? [] : [persistentVersionRead()], spaces: [], counters: [], sequences: [], multixacts: [], sqlite: null, cluster: null },
    } });
    const html = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="SQL Server" />);
    expect(html).toContain(english["view.cleanup.title"]);
    expect(html).not.toContain(english["ui.health.empty"]);
    expect(html).toContain(failed ? "Native cleanup permission denied." : english["view.cleanup.invalid"]);
    expect(html).not.toContain(EXACT_REASON_COUNT);
  }
});
