import { useState } from "react";
import type { TerminationHistory } from "../gen/TerminationHistory";
import type { TerminationReason } from "../gen/TerminationReason";
import type { Report } from "../gen/Report";
import { when } from "../lib/format";
import { t } from "../lib/i18n";
import { setState } from "../lib/store";

const REASONS: Record<TerminationReason, () => string> = {
  "temp-file-limit": () => t("ui.termination_history.reason.temp_file_limit"),
  sigkill: () => t("ui.termination_history.reason.sigkill"),
  sigsegv: () => t("ui.termination_history.reason.sigsegv"),
  other: () => t("ui.termination_history.reason.other"),
};
const PAGE_SIZE = 25;

export function TerminationHistoryView({ history, report }: { history: TerminationHistory; report: Report }) {
  const [page, setPage] = useState(0);
  const records = history.events.slice(0, 1000);
  const currentPage = Math.min(page, Math.max(0, Math.ceil(records.length / PAGE_SIZE) - 1));
  const events = records.slice(currentPage * PAGE_SIZE, (currentPage + 1) * PAGE_SIZE);
  const queries = new Set(report.snapshot.workload?.queries.map((q) => q.id));
  return <section className="operational-card" aria-label={t("ui.termination_history.label")}>
    <h4>{t("ui.termination_history.title", { count: String(records.length) })}</h4>
    <p className="muted">{history.source}. {history.visibility}. {history.partial && t("ui.termination_history.partial")}</p>
    <p className="muted" title={`${history.requested_start} — ${history.requested_end}`}>{t("ui.termination_history.requested", { from: when(history.requested_start).short, to: when(history.requested_end).short })}</p>
    {history.notes.map((note, i) => <p className="muted" key={i}>{note}</p>)}
    {history.events.length > records.length && <p className="muted">{t("ui.termination_history.first_records", { shown: String(records.length), total: String(history.events.length) })}</p>}
    {!records.length ? <p>{t("ui.termination_history.none")}</p> : <>
      <div style={{ overflowX: "auto" }}><table className="grid"><thead><tr><th>{t("ui.termination_history.column.terminated")}</th><th>{t("ui.termination_history.column.reason")}</th><th>{t("ui.termination_history.column.node")}</th><th>{t("ui.termination_history.column.query")}</th></tr></thead>
        <tbody>{events.map((event, i) => <tr key={i}>
          <td title={event.ended_at}>{when(event.ended_at).short}</td><td>{REASONS[event.reason]()}</td>
          <td className="mono" title={event.node ?? undefined}>{event.node?.slice(0, 8) ?? t("ui.common.unknown")}</td>
          <td>{event.query_id ? queries.has(event.query_id) ? <button type="button" className="link mono" onClick={() => setState({ view: "queries", query: event.query_id! })}>{event.query_id}</button> : <span className="mono" title={t("ui.termination_history.query_outside")}>{event.query_id}</span> : t("ui.termination_history.unresolved")}</td>
        </tr>)}</tbody></table></div>
      {records.length > PAGE_SIZE && <div className="actions">
        <button type="button" className="btn" disabled={currentPage === 0} onClick={() => setPage(currentPage - 1)}>{t("ui.termination_history.previous")}</button>
        <span className="muted">{t("ui.termination_history.page", { first: String(currentPage * PAGE_SIZE + 1), last: String(Math.min((currentPage + 1) * PAGE_SIZE, records.length)), total: String(records.length) })}</span>
        <button type="button" className="btn" disabled={(currentPage + 1) * PAGE_SIZE >= records.length} onClick={() => setPage(currentPage + 1)}>{t("ui.termination_history.next")}</button>
      </div>}
    </>}
  </section>;
}
