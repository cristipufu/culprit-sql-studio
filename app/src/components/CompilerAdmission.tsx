import type { ActivityKey } from "../gen/ActivityKey";
import type { CompilerAdmissionReport } from "../gen/CompilerAdmissionReport";
import type { CompilerAdmissionSnapshot } from "../gen/CompilerAdmissionSnapshot";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";
import { nativeIdentityKey } from "../lib/native-identity";
import { observationRows } from "../lib/observation-rows";
import { NativeReference, NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

type Entry = { kind: "waiter"; value: CompilerAdmissionReport["waiters"][number] }
  | { kind: "gateway"; value: CompilerAdmissionReport["gateways"][number] };

/** Original raw observations own the native actor and counters. The view never links a request to a gateway. */
export function CompilerAdmission({ evidence, report }: { evidence: CompilerAdmissionSnapshot; report: CompilerAdmissionReport }) {
  const rows = observationRows(evidence.observations, evidence.source, (row) => row.key);
  const entries: Entry[] = [
    ...report.waiters.map((value): Entry => ({ kind: "waiter", value })),
    ...report.gateways.map((value): Entry => ({ kind: "gateway", value })),
  ];
  return <ObservationList collapseEntries source={evidence.source} report={{ ...report, entries }}
    scope={t("ui.compiler_admission.scope")} entriesLabel={t("ui.compiler_admission.inspect")}
    time={evidence.context ? t("ui.memory_failures.read_between", { from: evidence.context.observed_from, until: evidence.context.observed_until })
      : t("ui.compiler_admission.window_unknown")}
    renderEntry={(entry) => {
      const value = entry.value;
      const row = rows.get(value.observation_key);
      if (entry.kind === "waiter" && row?.fact.kind === "compiler-waiter"
        && activityKey(entry.value.activity) === activityKey(row.fact.data.request.key)
        && row.fact.data.request.scope === "request-database" && row.fact.data.request.wait?.class === "compilation-admission") {
        const waiter = row.fact.data;
        const request = waiter.request;
        return <article className="operational-card" key={value.observation_key}>
          <h4>{value.title}</h4><p>{value.explanation}</p>
          <details><summary>{t("ui.memory_failures.investigation")}</summary>
            <p>{entry.value.investigation}</p><p>{entry.value.verify}</p>
            <dl>
              <NativeValue label={t("ui.memory_participants.observed_at_label")} value={<code>{waiter.observed_at}</code>} />
              <NativeValue label={t("ui.memory_participants.session")} value={<code>{request.key.session.id}</code>} />
              <NativeValue label={t("ui.memory_participants.generation")} value={<code>{request.key.session.generation}</code>} />
              <NativeValue label={t("ui.memory_participants.request")} value={request.key.request?.id} />
              <NativeValue label={t("ui.memory_participants.request_generation")} value={request.key.request?.generation} />
              <NativeValue label={t("ui.native_failures.database_context")} value={request.database} />
              <NativeValue label={t("ui.memory_participants.application")} value={request.client?.application} />
            </dl>
          </details>
        </article>;
      }
      if (entry.kind === "gateway" && row?.fact.kind === "counter-set"
        && nativeIdentityKey(entry.value.identity) === nativeIdentityKey(row.fact.data.identity)) {
        const gateway = row.fact.data;
        return <article className="operational-card" key={value.observation_key}>
          <h4>{value.title}</h4><p>{value.explanation}</p>
          <details><summary>{t("ui.memory_participants.native_properties")}</summary>
            <NativeReference identity={gateway.identity} />
            <dl>{gateway.counters.map((counter) => <NativeValue key={counter.key} label={counter.native_name}
              value={counter.unit.kind === "bytes" ? t("common.bytes.b", { n: digits(String(counter.value.value)) }) : digits(String(counter.value.value))} />)}</dl>
          </details>
        </article>;
      }
      return <div className="operational-card" key={value.observation_key}><p>{t("ui.memory_failures.missing_row")}</p></div>;
    }} />;
}

function activityKey(activity: ActivityKey): string {
  return JSON.stringify([activity.session.id, activity.session.generation, activity.request?.id ?? null,
    activity.request?.generation ?? null, activity.transaction]);
}
