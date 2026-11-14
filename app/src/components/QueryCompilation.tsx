import type { CompilationReport } from "../gen/CompilationReport";
import type { CompilationSnapshot } from "../gen/CompilationSnapshot";
import { findingMetric } from "../lib/format";
import { t } from "../lib/i18n";
import { nativeIdentityKey } from "../lib/native-identity";
import { observationRows } from "../lib/observation-rows";
import { NativeReference, NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

/** The explanation must name this exact native observation; a reused query number cannot supply another row. */
export function QueryCompilation({ evidence, report }: { evidence: CompilationSnapshot; report: CompilationReport }) {
  const rows = observationRows(evidence.observations, evidence.source, (row) => row.key);
  return <ObservationList collapseEntries source={evidence.source} report={report}
    scope={t("ui.compilation.scope")} entriesLabel={t("ui.compilation.inspect")}
    time={t("ui.memory_failures.read_between", { from: evidence.observed_from, until: evidence.observed_until })}
    renderEntry={(entry) => {
      const row = rows.get(entry.observation_key);
      const native = row?.fact.kind === "mssql-query-compilation" ? row.fact.data : null;
      const identity = { id: entry.identity.query_id, qualifiers: entry.identity.qualifiers };
      const matched = native && nativeIdentityKey(identity) === nativeIdentityKey({ id: native.identity.query_id, qualifiers: native.identity.qualifiers });
      return matched ? <article className="operational-card" key={entry.observation_key}>
        <h4>{entry.title}</h4><p>{entry.explanation}</p>
        <dl>{entry.metrics.map((metric, index) => <NativeValue key={metric.key ?? index} label={metric.label} value={findingMetric(metric)} />)}</dl>
        <details>
          <summary>{t("ui.memory_failures.investigation")}</summary>
          <p>{entry.investigation}</p><p>{entry.verify}</p>
          <NativeReference identity={identity} />
        </details>
      </article> : <div className="operational-card" key={entry.observation_key}><p>{t("ui.memory_failures.missing_row")}</p></div>;
    }} />;
}
