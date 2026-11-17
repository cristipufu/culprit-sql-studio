import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { PageServerReads } from "../components/PageServerReads";
import type { PageServerReadReport } from "../gen/PageServerReadReport";
import { EXACT_PAGE_SERVER_PAGES, pageServerReadReport } from "../test/page-server-reads";
import { t } from "./i18n";

function informationalReport(): PageServerReadReport {
  const report = pageServerReadReport(1);
  report.read_complete = true;
  report.selected_rows = 1;
  report.observed_rows = 1;
  report.source_state = "partial";
  report.summary = t("view.page_server_reads.summary.archive", { observed: "1", selected: "1" });
  report.runtime_read = {
    capture: { observation_id: "PRIVATE-ORIGINAL-CAPTURE", revision: { outbox_id: "PRIVATE-OUTBOX", owner_epoch: "1", sequence: "1" } },
    manifest_sha256: "a".repeat(64), source_sha256: "b".repeat(64), streams: [{ stream: "query-runtime", rows: 1 }],
  };
  report.reference_coverage = {
    queries: { required: 1, resolved: 0, unresolved: 1 }, plans: { required: 0, resolved: 0, unresolved: 0 },
    unavailable_payloads: 0, integrity_complete: true, complete: false, activity: null,
  };
  const entry = report.entries[0];
  if (!entry) throw new Error("The fixture requires one original runtime fragment");
  entry.identity = {
    kind: "runtime-fragment",
    reference: { kind: "informational", position: { stream: "query-runtime", row_ordinal: 1, payload_sha256: "c".repeat(64) } },
    native: { native: { id: "17", qualifiers: {} }, references: { query: { native: { id: "query-17", qualifiers: {} }, definition_id: null }, plan: null }, cohort: null },
  };
  report.limitations = [t("view.page_server_reads.limit.references")];
  entry.explanation.limitations = [t("view.page_server_reads.limit.identity")];
  return report;
}

test("unresolved fragments disclose their original position and exact counters without inventing SQL identity", () => {
  const html = renderToStaticMarkup(<PageServerReads report={informationalReport()} />);
  expect(html).toContain(EXACT_PAGE_SERVER_PAGES);
  expect(html).not.toContain("9007199254740992");
  expect(html).toContain(`<dt>${t("ui.page_server_reads.position")}</dt><dd>1</dd>`);
  expect(html).toContain(t("view.page_server_reads.limit.identity"));
  expect(html).toContain(t("view.page_server_reads.limit.references"));
  expect(html).not.toContain("PRIVATE-ORIGINAL-CAPTURE");
  expect(html).not.toContain("PRIVATE-OUTBOX");
  expect(html).not.toContain("c".repeat(64));
  expect(html).not.toContain("<details open");
});

test("an incomplete archive body preserves its independent gap alongside unresolved identity", () => {
  const report = informationalReport();
  report.read_complete = false;
  report.selected_rows = 2;
  report.summary = t("view.page_server_reads.summary.archive", { observed: "1", selected: "2" });
  const original = report.runtime_read;
  if (!original) throw new Error("The fixture requires its original runtime read");
  const stream = original.streams[0];
  if (!stream) throw new Error("The fixture requires its original runtime stream");
  stream.rows = 2;
  report.limitations.push(t("view.page_server_reads.limit.read"));
  const html = renderToStaticMarkup(<PageServerReads report={report} />);
  expect(html).toContain(t("view.page_server_reads.limit.read"));
  expect(html).toContain(t("view.page_server_reads.limit.references"));
  expect(html).toContain(t("view.page_server_reads.limit.identity"));
  expect(html).toContain(EXACT_PAGE_SERVER_PAGES);
});
