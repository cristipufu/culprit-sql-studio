import type { CdcReport } from "../gen/CdcReport";
import type { ChangeFeedReport } from "../gen/ChangeFeedReport";
import type { ChangeCaptureEntry } from "../gen/ChangeCaptureEntry";
import type { SourceOutcome } from "../gen/SourceOutcome";
import { t } from "../lib/i18n";
import { NativeValue, nativeTextPreview } from "./NativeObservationFields";
import { ObservationExplanationIssue, ObservationList, ObservationReadQualifications } from "./ObservationList";

type HistoryReport = CdcReport | ChangeFeedReport;
const DISPLAY_FIELDS = 13;
const TEXT_CHARS = 1024;
const NATIVE_FIELD_CHARS = 512;
const CONTEXT_CHARS = 256;
const TIME_CHARS = 128;
export const CDC_HISTORY_SOURCES = ["cdc status", "cdc scan sessions", "cdc processing errors"];
export const CHANGE_FEED_HISTORY_SOURCES = ["change feed availability", "change feed scan sessions", "change feed processing errors"];

/** Presence/qualification only; the canonical Rust report owns every native interpretation. */
export function changeCaptureHistoryState(reports: HistoryReport[] | undefined, recorded: number, names: string[], sources: SourceOutcome[]) {
  const originals = reports ?? [];
  const sourceRecorded = sources.some((source) => names.includes(source.source));
  return {
    reports: originals,
    present: originals.length > 0 || recorded > 0 || sourceRecorded,
    unexplained: originals.length === 0 && (recorded > 0 || sourceRecorded),
    issue: originals.some((report) => !report.read_complete || report.source_state === "partial" || report.source_state === "unavailable"),
  };
}

function text(value: string, limit = TEXT_CHARS): string {
  return nativeTextPreview(value, limit);
}
function fieldValue(value: string | null): string | null {
  return value === null ? null : text(value, NATIVE_FIELD_CHARS);
}
function Entry({ entry }: { entry: ChangeCaptureEntry }) {
  return <article className="operational-card change-capture-entry">
    <h4>{text(entry.title)}</h4>
    <p>{text(entry.explanation)}</p>
    <p className="muted">{text(entry.source, CONTEXT_CHARS)}</p>
    <dl><NativeValue label={t("view.storage.field.observed_at")} value={text(entry.observed_at, TIME_CHARS)} />
      {entry.fields.slice(0, DISPLAY_FIELDS).map((field) => <NativeValue key={field.key} label={text(field.label, CONTEXT_CHARS)} value={fieldValue(field.value)} />)}
    </dl>
    <p>{text(entry.investigation)}</p><p>{text(entry.verify)}</p>
  </article>;
}

/** Independent native originals retain their own scope and clocks, including withdrawn explanations. */
export function ChangeCaptureHistory({ title, reports, unexplained = false }: { title: string; reports: HistoryReport[]; unexplained?: boolean }) {
  if (reports.length === 0) return unexplained ? <ObservationExplanationIssue title={title} reason={t("view.change_capture.history.missing")} /> : null;
  return <ObservationList collapseEntries source={title} scope={t("view.change_capture.history.original")}
    report={{ title, summary: t("view.change_capture.history.summary"), sources: [], limitations: [], entries: reports }}
    renderEntry={(report) => <div className="operational-card" key={reports.indexOf(report)}>
      {report.context?.target.scope === "database" && <p><code>{text(report.context.target.identity.database, CONTEXT_CHARS)}</code></p>}
      <ObservationList collapseEntries source={text(report.title)} scope={t("view.change_capture.history.original")}
        time={report.context ? t("ui.memory_failures.read_between", { from: report.context.observed_from, until: report.context.observed_until }) : null}
        report={{ title: text(report.title), summary: text(report.summary), source_state: report.source_state, source_reason: null,
          limitations: report.limitations.map((limitation) => text(limitation)), entries: report.entries }}
        renderEntry={(entry) => <Entry key={entry.observation_key} entry={entry} />}>
        <ObservationReadQualifications sources={report.sources} />
      </ObservationList>
    </div>} />;
}
