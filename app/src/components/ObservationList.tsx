import { useState, type ReactNode } from "react";
import type { SourceOutcome } from "../gen/SourceOutcome";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";

const PAGE_SIZE = 10;

type ReadSource = { source: string; state: SourceOutcome["state"] | null; reason?: string | null };

type ObservationReport<Entry> = {
  title: string;
  summary: string;
  limitations: string[];
  entries: Entry[];
} & ({ source_state: SourceOutcome["state"] | null; source_reason: string | null } | { sources: SourceOutcome[] });

/** Shared presentation owns read qualification and paging; each native format owns its evidence renderer. */
export function ObservationList<Entry>({ source, time, report, scope, renderEntry, collapseEntries = false, entriesLabel, children }: {
  source: string;
  time?: ReactNode;
  report: ObservationReport<Entry>;
  scope: string;
  renderEntry: (entry: Entry) => ReactNode;
  collapseEntries?: boolean;
  entriesLabel?: string;
  children?: ReactNode;
}) {
  const [page, setPage] = useState(0);
  const lastPage = Math.max(0, Math.ceil(report.entries.length / PAGE_SIZE) - 1);
  const currentPage = Math.min(page, lastPage);
  const first = currentPage * PAGE_SIZE;
  const sources = "sources" in report ? report.sources : [{ source, state: report.source_state, reason: report.source_reason }];
  const observations = <>
    <div className="operational-grid observation-grid">
      {report.entries.slice(first, first + PAGE_SIZE).map(renderEntry)}
    </div>
    {report.entries.length > PAGE_SIZE && <div className="actions">
      <button type="button" className="btn" disabled={currentPage === 0} onClick={() => setPage(currentPage - 1)}>{t("ui.termination_history.previous")}</button>
      <span className="muted">{t("ui.memory_failures.page", { first: digits(String(first + 1)), last: digits(String(Math.min(first + PAGE_SIZE, report.entries.length))), total: digits(String(report.entries.length)) })}</span>
      <button type="button" className="btn" disabled={currentPage === lastPage} onClick={() => setPage(currentPage + 1)}>{t("ui.termination_history.next")}</button>
    </div>}
  </>;
  return <section className="operational-health observation-list" aria-label={report.title}>
    <h3>{report.title}</h3>
    <p>{report.summary}</p>
    <p className="muted">{source} · {scope}</p>
    {time != null && <p className="muted">{time}</p>}
    <ObservationReadQualifications sources={sources} />
    {children}
    <details>
      <summary>{t("ui.memory_failures.limitations")}</summary>
      <ObservationReadReasons sources={sources} />
      {report.limitations.map((limitation, index) => <p className="muted" key={index}>{limitation}</p>)}
    </details>
    {collapseEntries && report.entries.length > 0 ? <details>
      <summary>{entriesLabel ?? t("ui.retained_failures.observations")}</summary>
      {observations}
    </details> : observations}
  </section>;
}

/** A failed acquisition remains visible even when it cannot admit a typed native wrapper. */
export function observationReadIssue(source: string, sources: SourceOutcome[], requested = false): SourceOutcome | undefined {
  const outcome = sources.find((outcome) => outcome.source === source);
  if (!outcome) {
    return requested ? { source, state: "unavailable", reason: t("ui.observation_list.source_unknown") } : undefined;
  }
  return (
    outcome.state === "partial" || outcome.state === "unavailable" || (requested && outcome.state === "not-applicable")
  ) ? outcome : undefined;
}

export function ObservationSourceIssue(props: { title: string } & ({ source: SourceOutcome } | { sources: SourceOutcome[] })) {
  const sources = "sources" in props ? props.sources : [props.source];
  return <section className="operational-health observation-list" aria-label={props.title}>
    <h3>{props.title}</h3>
    <ObservationReadQualifications sources={sources} />
    <details>
      <summary>{t("ui.memory_failures.limitations")}</summary>
      <ObservationReadReasons sources={sources} showUnknown />
    </details>
  </section>;
}

/** Explanation admission is separate from the original source's collection outcome. */
export function ObservationExplanationIssue({ title, reason }: { title: string; reason: string }) {
  return <section className="operational-health observation-list" aria-label={title}>
    <h3>{title}</h3><p className="muted">{reason}</p>
  </section>;
}

export function ObservationReadQualifications({ sources }: { sources: ReadSource[] }) {
  return sources.map((source) => {
    const qualification = readQualification(source.state);
    return qualification && <p className="muted" key={source.source}>{sources.length > 1 && <>{source.source} · </>}{qualification}</p>;
  });
}

export function ObservationReadReasons({ sources, showUnknown = false }: { sources: ReadSource[]; showUnknown?: boolean }) {
  return sources.map((source) => {
    const reason = source.reason ?? (showUnknown ? t("ui.observation_list.source_unknown") : null);
    return reason && <p className="muted" key={source.source}>{sources.length > 1 && <>{source.source} · </>}{reason}</p>;
  });
}

function readQualification(state: SourceOutcome["state"] | null): string | null {
  switch (state) {
    case null: return t("ui.observation_list.source_unknown");
    case "partial": return t("ui.common.read_partial");
    case "unavailable": return t("ui.dashboard.status.unavailable");
    case "not-applicable": return t("ui.retained_failures.not_applicable");
    default: return null;
  }
}
