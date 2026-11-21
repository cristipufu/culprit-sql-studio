import { useEffect, useRef, useState } from "react";
import type { IncidentHistoryReport } from "../gen/IncidentHistoryReport";
import type { IncidentHistoryEntry } from "../gen/IncidentHistoryEntry";
import { api, errorMessage } from "../lib/api";
import { contextPreview } from "../lib/context-preview";
import { digits } from "../lib/format";
import { t, useLanguage } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

const text = (value: string) => contextPreview(value).text;
type OpenArtifact = (signal: AbortSignal) => Promise<IncidentHistoryReport | null>;
const openArtifact: OpenArtifact = async (signal) => (await api()).openIncidentHistory(signal);

/** An explicit selected artifact has its own owner and never enters current Health grades or session actions. */
export function IncidentHistoryImport({ open = openArtifact }: { open?: OpenArtifact }) {
  const language = useLanguage();
  return <IncidentHistorySelection key={language} open={open} />;
}

function IncidentHistorySelection({ open }: { open: OpenArtifact }) {
  const pending = useRef<AbortController | null>(null);
  const [report, setReport] = useState<IncidentHistoryReport | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => () => pending.current?.abort(), []);
  async function select() {
    pending.current?.abort();
    const owner = new AbortController();
    pending.current = owner;
    setBusy(true);
    setError(null);
    setReport(null);
    try {
      const selected = await open(owner.signal);
      if (!owner.signal.aborted && pending.current === owner) setReport(selected);
    } catch (failure) {
      if (!owner.signal.aborted && pending.current === owner) setError(errorMessage(failure).message);
    } finally {
      if (pending.current === owner) {
        pending.current = null;
        setBusy(false);
      }
    }
  }
  return (
    <section className="operational-health" aria-label={t("view.incident_history.import.section")}>
      <h3>{t("view.incident_history.import.section")}</h3>
      <p className="muted">{t("view.incident_history.import.intro")}</p>
      <button type="button" className="btn" onClick={() => void select()} aria-busy={busy}>
        {busy ? t("view.incident_history.import.replace") : t("view.incident_history.import.open")}
      </button>
      {error && <p role="alert">{text(error)}</p>}
      {report && <IncidentHistory report={report} />}
    </section>
  );
}

export function IncidentHistory({ report }: { report: IncidentHistoryReport }) {
  return (
    <ObservationList
      report={report}
      source={t("view.incident_history.title")}
      scope={t("view.incident_history.selected_scope")}
      collapseEntries
      entriesLabel={t("view.incident_history.entries")}
      renderEntry={(entry) => (
        <IncidentRevision key={`${entry.controller_version}:${entry.episode_id}:${entry.revision}`} entry={entry} />
      )}
    />
  );
}
export function IncidentRevision({ entry }: { entry: IncidentHistoryEntry }) {
  return (
    <article className="operational-card">
      <h4>{text(entry.title)}</h4>
      <p>{text(entry.phase)}</p>
      <p className="muted">{text(entry.scope_label)}</p>
      <dl>
        <NativeValue label={t("view.incident_history.field.first")} value={text(entry.first_seen_at)} />
        <NativeValue label={t("view.incident_history.field.latest")} value={text(entry.last_seen_at)} />
        <NativeValue label={t("view.incident_history.field.recorded")} value={text(entry.recorded_at)} />
      </dl>
      {entry.reasons.map((reason, index) => (
        <p key={index}>{text(reason)}</p>
      ))}
      <details>
        <summary>{t("view.incident_history.evidence.details")}</summary>
        <dl>
          <NativeValue label={t("view.incident_history.field.episode")} value={text(entry.episode_id)} />
          <NativeValue
            label={t("view.incident_history.field.controller")}
            value={digits(String(entry.controller_version))}
          />
          <NativeValue label={t("view.incident_history.field.target")} value={text(entry.target_id)} />
          <NativeValue label={t("view.incident_history.field.scope")} value={text(entry.scope_id)} />
          <NativeValue label={t("view.incident_history.field.source")} value={text(entry.source)} />
          <NativeValue
            label={t("view.incident_history.field.recipe")}
            value={entry.followup_recipe == null ? null : text(entry.followup_recipe)}
          />
        </dl>
        {entry.evidence.map((evidence, index) => (
          <div key={index}>
            <p>
              {text(evidence.label)} · {text(evidence.availability)}
            </p>
            <code>{text(evidence.capture.capture.observation_id)}</code>
          </div>
        ))}
      </details>
      <p>{text(entry.next_step)}</p>
    </article>
  );
}
