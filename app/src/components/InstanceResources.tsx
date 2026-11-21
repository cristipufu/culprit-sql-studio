import type { InstanceResourceGroup } from "../gen/InstanceResourceGroup";
import type { InstanceResourceReport } from "../gen/InstanceResourceReport";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationList } from "./ObservationList";

/** Shared Rust facts own units, native populations and qualifications; this disclosure calculates no pressure. */
export function InstanceResources({ report }: { report: InstanceResourceReport }) {
  const bounds = report.context?.kind === "native" ? report.context.context : report.context;
  const cpu = report.process_cpu;
  return (
    <>
      <ObservationList
        collapseEntries
        source={t("view.instance_resources.source")}
        scope={t("ui.beyond_time.scope.server")}
        time={
          <>
            {report.observed_at != null && (
              <>{t("view.instance_resources.observed_at", { at: report.observed_at })}<br /></>
            )}
            {bounds && (
              <span className="muted">
                {t("ui.memory_failures.read_between", { from: bounds.observed_from, until: bounds.observed_until })}
              </span>
            )}
          </>
        }
        report={{ ...report, entries: report.groups, source_reason: null }}
        renderEntry={renderResourceGroup}
      />
      {cpu && (
        <ObservationList
          collapseEntries
          source={cpu.title}
          scope={t("ui.beyond_time.scope.server")}
          time={cpu.observed_at && t("view.instance_resources.observed_at", { at: cpu.observed_at })}
          report={{
            ...cpu,
            entries: cpu.fields.length ? [{ key: "process-cpu", title: cpu.title, fields: cpu.fields }] : [],
            source_reason: null,
          }}
          renderEntry={renderResourceGroup}
        />
      )}
    </>
  );
}

function renderResourceGroup(group: InstanceResourceGroup) {
  return (
    <article className="operational-card" key={group.key}>
      <h4>{group.title}</h4>
      <dl>
        {group.fields.map((field) => (
          <NativeValue key={field.key} label={field.label} value={field.value} />
        ))}
      </dl>
    </article>
  );
}
