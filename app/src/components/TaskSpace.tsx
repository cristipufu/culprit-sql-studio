import type { TaskRequestCheck } from "../gen/TaskRequestCheck";
import type { TaskSpaceIdentity } from "../gen/TaskSpaceIdentity";
import type { TaskSpaceObservation } from "../gen/TaskSpaceObservation";
import type { TaskSpaceReport } from "../gen/TaskSpaceReport";
import type { TaskSpaceSnapshot } from "../gen/TaskSpaceSnapshot";
import { digits } from "../lib/format";
import { key, t } from "../lib/i18n";
import { observationRows } from "../lib/observation-rows";
import { NativeReference, NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

function reference(task: TaskSpaceIdentity): string {
  return JSON.stringify([task.session_id, task.request_id, task.execution_context_id]);
}

/** The native tuple locates a capture-local row; it does not prove a persistent task or an allocating statement. */
export function TaskSpace({ evidence, report }: { evidence: TaskSpaceSnapshot; report: TaskSpaceReport }) {
  const rows = observationRows(evidence.observations, evidence.source, (row) => reference(row.task));
  return <ObservationList collapseEntries source={evidence.source} report={report}
    scope={t("ui.task_space.scope")} entriesLabel={t("ui.task_space.inspect")}
    time={t("ui.memory_failures.read_between", { from: evidence.observed_from, until: evidence.observed_until })}
    renderEntry={(entry) => {
      const row = rows.get(reference(entry.task));
      return row ? <TaskCard key={entry.observation_key} row={row} entry={entry} />
        : <div className="operational-card" key={entry.observation_key}><p>{t("ui.memory_failures.missing_row")}</p></div>;
    }} />;
}

function TaskCard({ row, entry }: { row: TaskSpaceObservation; entry: TaskSpaceReport["entries"][number] }) {
  const checked = row.before_check?.state === "same-statement" && row.after_check?.state === "same-statement";
  const request = row.request;
  return <article className="operational-card">
    <h4>{entry.title}</h4><p>{entry.explanation}</p>
    <details>
      <summary>{t("ui.memory_failures.investigation")}</summary>
      <p>{entry.investigation}</p><p>{entry.verify}</p>
      <dl>
        <NativeValue label={t("ui.task_space.observed_at")} value={<code>{row.observed_at}</code>} />
        <NativeValue label={t("ui.memory_participants.session")} value={<code>{row.task.session_id}</code>} />
        <NativeValue label={t("ui.memory_participants.request")} value={<code>{row.task.request_id}</code>} />
        <NativeValue label={t("ui.task_space.execution_context")} value={<code>{row.task.execution_context_id}</code>} />
        <NativeValue label={t("ui.task_space.native_database")} value={<code>{row.native_database_id}</code>} />
        <NativeValue label={t("ui.task_space.page_bytes")} value={digits(String(row.bytes_per_page))} />
        {COUNTERS.map(([field, label]) => <NativeValue key={field} label={t(label)} value={row[field] == null ? null : digits(row[field])} />)}
        <NativeValue label={t("ui.memory_participants.generation")} value={request && <code>{request.key.session.generation}</code>} />
        <NativeValue label={t("ui.memory_participants.request_generation")} value={request?.key.request && <code>{request.key.request.generation}</code>} />
        <NativeValue label={t("ui.native_failures.database_context")} value={request?.database} />
        <NativeValue label={t("ui.memory_participants.application")} value={request?.client?.application} />
      </dl>
      <details>
        <summary>{t("ui.task_space.checks")}</summary>
        <RequestCheck title={t("ui.task_space.before")} check={row.before_check} />
        <RequestCheck title={t("ui.task_space.after")} check={row.after_check} />
      </details>
      {checked && <dl>
        {row.statement_identity && <NativeValue label={t("ui.page_waits.statement_identity")} value={<NativeReference identity={row.statement_identity} />} />}
        {row.statement_identity && row.plan_identity && <NativeValue label={t("ui.page_waits.plan_identity")} value={<NativeReference identity={row.plan_identity} />} />}
      </dl>}
      {checked && request?.statement.state === "available" && <details>
        <summary>{t("ui.task_space.current_statement")}</summary>
        <pre className="observation-statement">{request.statement.text}</pre>
      </details>}
    </details>
  </article>;
}

const COUNTERS = [
  ["user_allocated_pages", key("ui.task_space.user_allocated")],
  ["user_deallocated_pages", key("ui.task_space.user_deallocated")],
  ["internal_allocated_pages", key("ui.task_space.internal_allocated")],
  ["internal_deallocated_pages", key("ui.task_space.internal_deallocated")],
] as const;

const CHECKS = {
  "same-statement": key("ui.page_waits.recheck.request.same"),
  missing: key("ui.page_waits.recheck.request.missing"),
  changed: key("ui.page_waits.recheck.request.changed"),
  unverified: key("ui.page_waits.recheck.request.unverified"),
} as const;

function RequestCheck({ title, check }: { title: string; check: TaskRequestCheck | null }) {
  return <div><h5>{title}</h5>{check ? <dl>
    <NativeValue label={t("ui.page_waits.recheck.at")} value={<code>{check.observed_at}</code>} />
    <NativeValue label={t("ui.page_waits.recheck.request")} value={t(CHECKS[check.state])} />
  </dl> : <p className="muted">{t("ui.page_waits.recheck.missing")}</p>}</div>;
}
