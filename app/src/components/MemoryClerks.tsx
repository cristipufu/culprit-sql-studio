import type { MemoryClerkReport } from "../gen/MemoryClerkReport";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

/** Rust interprets the original allocation fields; the UI renders their exact values without adding them. */
export function MemoryClerks({ report }: { report: MemoryClerkReport }) {
  const bounds = report.context?.kind === "native" ? report.context.context : report.context;
  return <ObservationList collapseEntries source={t("provider.mssql.source.memory_clerks")}
    report={report} scope={t("collected.memory_clerks.scope")}
    time={bounds && t("ui.memory_failures.read_between", {
      from: bounds.observed_from, until: bounds.observed_until,
    })}
    renderEntry={(entry) => <article className="operational-card" key={entry.observation_key}>
      <h4><code>{entry.clerk_type}</code></h4>
      <p>{entry.explanation}</p>
      <dl>
        <NativeValue label={t("collected.memory_clerks.native.name")} value={entry.name} />
        <NativeValue label={t("collected.memory_clerks.native.node")} value={entry.memory_node_id} />
        {entry.fields.map((field) => <NativeValue key={field.key} label={field.label} value={field.value} />)}
      </dl>
    </article>} />;
}
