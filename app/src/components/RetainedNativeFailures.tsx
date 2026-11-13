import type { MysqlErrorLogObservation } from "../gen/MysqlErrorLogObservation";
import type { NativeFailureRecord } from "../gen/NativeFailureRecord";
import type { NativeFailureSnapshot } from "../gen/NativeFailureSnapshot";
import type { PostgresLogFailureObservation } from "../gen/PostgresLogFailureObservation";
import type { RetainedFailureExplanation } from "../gen/RetainedFailureExplanation";
import type { RetainedFailureReport } from "../gen/RetainedFailureReport";
import type { SourceOutcome } from "../gen/SourceOutcome";
import { t } from "../lib/i18n";
import { RetainedFailures, RetainedFailureSourceIssue, retainedFailureReadIssue } from "./RetainedFailures";

export function RetainedNativeFailures({ evidence, report }: { evidence: NativeFailureSnapshot; report: RetainedFailureReport }) {
  const observations = new Map(evidence.observations.map((row) => [row.observation.position, row]));
  return <RetainedFailures collapseEntries evidence={evidence} report={report} scope={t(evidence.resource?.kind === "instance" ? "ui.memory_failures.scope.server" : "ui.memory_failures.scope.unqualified")} renderEntry={(entry) => {
    const observation = observations.get(entry.position);
    return observation
      ? <NativeFailureRow key={entry.position} row={observation} explanation={entry} />
      : <div className="operational-card" key={entry.position}><p>{t("ui.memory_failures.missing_row")}</p></div>;
  }} />;
}

export function nativeFailureReadIssue(source: string | undefined, sources: SourceOutcome[], requested = false): SourceOutcome | undefined {
  return retainedFailureReadIssue(source ?? "native failures", sources, requested);
}

export function NativeFailureSourceIssue({ source }: { source: SourceOutcome }) {
  return <RetainedFailureSourceIssue source={source} title={t("view.native_failures.title")} />;
}

function NativeFailureRow({ row, explanation }: { row: NativeFailureRecord; explanation: RetainedFailureExplanation }) {
  return <article className="operational-card">
    <h4>{explanation.title}</h4>
    <p>{explanation.explanation}</p>
    <NativeEventTime row={row} />
    <details>
      <summary>{t("ui.memory_failures.investigation")}</summary>
      <p>{explanation.investigation}</p>
      <p>{explanation.verify}</p>
      {row.native === "mysql-error-log" && <MysqlMetadata row={row.observation} />}
      {row.native === "postgres-log" && <PostgresMetadata row={row.observation} />}
      <p className="muted">{t("ui.native_failures.message_omitted")}</p>
    </details>
  </article>;
}

function NativeEventTime({ row }: { row: NativeFailureRecord }) {
  switch (row.native) {
    case "mysql-error-log": return <p className="muted">{t("ui.native_failures.event_time")} <code>{row.observation.logged_at}</code></p>;
    case "postgres-log": return <p className="muted">{row.observation.timestamp_utc ? t("ui.native_failures.event_time") : t("ui.native_failures.local_event_time")} <code>{row.observation.timestamp_utc ?? row.observation.timestamp}</code></p>;
  }
}

function MysqlMetadata({ row }: { row: MysqlErrorLogObservation }) {
  return <>
    <dl>
      <div><dt>{t("ui.native_failures.priority")}</dt><dd><code>{row.priority}</code></dd></div>
      <div><dt>{t("ui.native_failures.error_code")}</dt><dd><code>{row.error_code ?? t("ui.common.unknown")}</code></dd></div>
      <div><dt>{t("ui.native_failures.subsystem")}</dt><dd><code>{row.subsystem ?? t("ui.common.unknown")}</code></dd></div>
      <div><dt>{t("ui.native_failures.thread")}</dt><dd><code>{row.thread_id ?? t("ui.common.unknown")}</code></dd></div>
    </dl>
    <p className="muted">{t("ui.native_failures.thread_note")}</p>
  </>;
}

function PostgresMetadata({ row }: { row: PostgresLogFailureObservation }) {
  return <>
    <dl>
      <div><dt>{t("ui.native_failures.sql_state")}</dt><dd><code>{row.sql_state}</code></dd></div>
      <div><dt>{t("ui.native_failures.severity")}</dt><dd><code>{row.severity ?? t("ui.common.unknown")}</code></dd></div>
      <div><dt>{t("ui.native_failures.database_context")}</dt><dd><code>{row.database ?? t("ui.common.unknown")}</code></dd></div>
      <div><dt>{t("ui.native_failures.process_id")}</dt><dd><code>{row.process_id ?? t("ui.common.unknown")}</code></dd></div>
      <div><dt>{t("ui.native_failures.session_id")}</dt><dd><code>{row.session_id ?? t("ui.common.unknown")}</code></dd></div>
      <div><dt>{t("ui.native_failures.session_start")}</dt><dd><code>{row.session_start ?? t("ui.common.unknown")}</code></dd></div>
      <div><dt>{t("ui.native_failures.session_line")}</dt><dd><code>{row.session_line_number ?? t("ui.common.unknown")}</code></dd></div>
      <div><dt>{t("ui.native_failures.query_id")}</dt><dd><code>{row.query_id ?? t("ui.common.unknown")}</code></dd></div>
      <div><dt>{t("ui.native_failures.backend_type")}</dt><dd><code>{row.backend_type ?? t("ui.common.unknown")}</code></dd></div>
    </dl>
    <p className="muted">{t("ui.native_failures.postgres_context_note")}</p>
    {row.statement.state === "available" && <details>
      <summary>{t("ui.native_failures.reported_statement")}</summary>
      <pre className="mono" style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere", maxHeight: "18rem", overflow: "auto" }}>{row.statement.text}</pre>
    </details>}
  </>;
}
