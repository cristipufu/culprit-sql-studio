import { useState } from "react";
import type { ActivityCategory } from "../gen/ActivityCategory";
import type { ActivityReport } from "../gen/ActivityReport";
import { count, percent, when } from "../lib/format";
import { t } from "../lib/i18n";
import { setState } from "../lib/store";
import { waitColor } from "../lib/waits";

export function ActivityHistoryView({ activity, selected, onSelect, onClose }: {
  activity: ActivityReport; selected?: string | null; onSelect: (key: string) => void; onClose: () => void;
}) {
  const current = activity.categories.find((c) => c.key === selected) ?? activity.categories[0];
  const p = activity.period;
  return <section aria-label={t("ui.activity_history.label")}>
    <div className="notice activity-notice">
      <b>{t("ui.activity_history.retained", { observations: count(p.observations), weight: count(p.weighted_observations) })}</b>
      <p title={`${p.first_sample ?? ""} — ${p.last_sample ?? ""}`}>{p.first_sample && p.last_sample ? t("ui.activity_history.observed", { from: when(p.first_sample).short, to: when(p.last_sample).short }) : t("ui.activity_history.no_samples")}
        {p.partial && ` ${t("ui.common.read_partial")}`}</p>
      <p className="muted" title={`${p.requested_start} — ${p.requested_end}`}>{t("ui.activity_history.requested", { from: when(p.requested_start).short, to: when(p.requested_end).short, source: p.source })}</p>
      {p.notes.map((note, i) => <p className="muted" key={i}>{note}</p>)}
    </div>
    {current && <div className="waits-grid">
      <div className="waits-list" role="listbox" aria-label={t("ui.activity_history.categories")}>
        {activity.categories.map((c) => <button type="button" key={c.key} role="option" aria-selected={c.key === current.key}
          className={`waits-row ${c.key === current.key ? "on" : ""}`} onClick={() => onSelect(c.key)}
          style={{ "--cat": waitColor(c.key) } as React.CSSProperties}>
          <i className="swatch" /><span className="waits-row-text"><span className="name">{c.label}</span></span>
          <span className="waits-row-num"><b>{percent(c.share)}</b><span>{t("ui.activity_history.weight", { weight: count(c.weighted_observations) })}</span></span>
        </button>)}
      </div>
      <ActivityDetail key={current.key} category={current} onClose={onClose} />
    </div>}
  </section>;
}

const PAGE_SIZE = 25;
function ActivityDetail({ category: c, onClose }: { category: ActivityCategory; onClose: () => void }) {
  const [page, setPage] = useState(0);
  const currentPage = Math.min(page, Math.max(0, Math.ceil(c.events.length / PAGE_SIZE) - 1));
  const events = c.events.slice(currentPage * PAGE_SIZE, (currentPage + 1) * PAGE_SIZE);
  return <div className="waits-detail">
    <div className="detail-head"><h3>{c.label}</h3><span className="pill">{t("ui.activity_history.share_of_weight", { share: percent(c.share) })}</span></div>
    <p className="muted">{t("ui.activity_history.category_summary", { observations: count(c.observations), weight: count(c.weighted_observations) })}</p>
    <table className="grid"><thead><tr><th>{t("ui.common.node")}</th><th>{t("ui.activity_history.component")}</th><th>{t("ui.activity_history.native_event")}</th><th className="r">{t("ui.activity_history.observations")}</th><th className="r">{t("ui.activity_history.weight_heading")}</th></tr></thead>
      <tbody>{events.map((e, i) => <tr key={i}><td className="mono" title={e.node}>{e.node?.slice(0, 8) ?? t("ui.common.unknown")}</td><td>{e.component}</td><td className="mono" style={{ whiteSpace: "normal", overflowWrap: "anywhere" }}>{e.wait_event}{e.query_id && <div className="muted">{t("ui.common.query_id", { id: e.query_id })}</div>}</td><td className="r">{count(e.observations)}</td><td className="r">{count(e.weighted_observations)}</td></tr>)}</tbody>
    </table>
    {c.events.length > PAGE_SIZE && <div className="actions">
      <button type="button" className="btn" disabled={currentPage === 0} onClick={() => setPage(currentPage - 1)}>{t("ui.common.previous")}</button>
      <span className="muted">{t("ui.activity_history.page", { from: String(currentPage * PAGE_SIZE + 1), to: String(Math.min((currentPage + 1) * PAGE_SIZE, c.events.length)), total: String(c.events.length) })}</span>
      <button type="button" className="btn" disabled={(currentPage + 1) * PAGE_SIZE >= c.events.length} onClick={() => setPage(currentPage + 1)}>{t("ui.common.next")}</button>
    </div>}
    <h4 className="section-title">{t("ui.activity_history.queries")}</h4>
    <div className="wq-list">{c.queries.map((q) => <button type="button" className="wq-row" key={q.id} disabled={!q.text}
      title={q.text ? t("ui.common.open_in_queries") : t("ui.activity_history.outside_workload")}
      onClick={() => { setState({ view: "queries", query: q.id }); onClose(); }}>
      <span className="wq-share">{percent(q.share_of_category)}</span><span>{t("ui.common.query_id", { id: q.id })}<br />{q.text ?? t("ui.activity_history.not_retained")}</span>
    </button>)}</div>
    {!c.queries.length && <p className="muted">{t("ui.activity_history.no_statement")}</p>}
  </div>;
}
