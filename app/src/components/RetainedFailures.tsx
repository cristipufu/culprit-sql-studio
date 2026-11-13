import { useState, type ReactNode } from "react";
import type { RetainedFailureExplanation } from "../gen/RetainedFailureExplanation";
import type { RetainedFailureReport } from "../gen/RetainedFailureReport";
import type { SourceOutcome } from "../gen/SourceOutcome";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";

const PAGE_SIZE = 10;

type CaptureBounds = { source: string; observed_from: string; observed_until: string };

/** Shared presentation owns read qualification and paging; each native format owns its evidence renderer. */
export function RetainedFailures({ evidence, report, scope, renderEntry, collapseEntries = false }: {
  evidence: CaptureBounds;
  report: RetainedFailureReport;
  scope: string;
  renderEntry: (entry: RetainedFailureExplanation) => ReactNode;
  collapseEntries?: boolean;
}) {
  const [page, setPage] = useState(0);
  const lastPage = Math.max(0, Math.ceil(report.entries.length / PAGE_SIZE) - 1);
  const currentPage = Math.min(page, lastPage);
  const first = currentPage * PAGE_SIZE;
  const qualification = readQualification(report.source_state);
  const observations = <>
    <div className="operational-grid memory-failure-grid">
      {report.entries.slice(first, first + PAGE_SIZE).map(renderEntry)}
    </div>
    {report.entries.length > PAGE_SIZE && <div className="actions">
      <button type="button" className="btn" disabled={currentPage === 0} onClick={() => setPage(currentPage - 1)}>{t("ui.termination_history.previous")}</button>
      <span className="muted">{t("ui.memory_failures.page", { first: digits(String(first + 1)), last: digits(String(Math.min(first + PAGE_SIZE, report.entries.length))), total: digits(String(report.entries.length)) })}</span>
      <button type="button" className="btn" disabled={currentPage === lastPage} onClick={() => setPage(currentPage + 1)}>{t("ui.termination_history.next")}</button>
    </div>}
  </>;
  return <section className="operational-health retained-failures" aria-label={report.title}>
    <h3>{report.title}</h3>
    <p>{report.summary}</p>
    <p className="muted">{evidence.source} · {scope}</p>
    <p className="muted">{t("ui.memory_failures.read_between", { from: evidence.observed_from, until: evidence.observed_until })}</p>
    {qualification && <p className="muted">{qualification}</p>}
    <details>
      <summary>{t("ui.memory_failures.limitations")}</summary>
      {report.source_reason && <p className="muted">{report.source_reason}</p>}
      {report.limitations.map((limitation, index) => <p className="muted" key={index}>{limitation}</p>)}
    </details>
    {collapseEntries && report.entries.length > 0 ? <details>
      <summary>{t("ui.retained_failures.observations")}</summary>
      {observations}
    </details> : observations}
  </section>;
}

/** A failed acquisition remains visible even when it cannot admit a typed native wrapper. */
export function retainedFailureReadIssue(source: string, sources: SourceOutcome[], requested = false): SourceOutcome | undefined {
  const expected = sourceKey(source);
  return sources.find((outcome) => sourceKey(outcome.source) === expected && (
    outcome.state === "partial" || outcome.state === "unavailable" || (requested && outcome.state === "not-applicable")
  ));
}

function sourceKey(source: string): string {
  return source.replace(/[A-Z ]/g, (character) => character === " " ? "_" : character.toLowerCase());
}

export function RetainedFailureSourceIssue({ source, title }: { source: SourceOutcome; title: string }) {
  return <section className="operational-health retained-failures" aria-label={title}>
    <h3>{title}</h3>
    <p className="muted">{readQualification(source.state)}</p>
    <details>
      <summary>{t("ui.memory_failures.limitations")}</summary>
      <p>{source.reason ?? t("view.memory_failures.limit.source_unknown")}</p>
    </details>
  </section>;
}

function readQualification(state: SourceOutcome["state"] | null): string | null {
  switch (state) {
    case "partial": return t("ui.common.read_partial");
    case "unavailable": return t("ui.dashboard.status.unavailable");
    case "not-applicable": return t("ui.retained_failures.not_applicable");
    default: return null;
  }
}
