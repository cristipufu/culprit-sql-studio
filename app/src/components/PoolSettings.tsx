import { useState } from "react";
import type { PoolSettingsReport } from "../gen/PoolSettingsReport";
import { contextPreview } from "../lib/context-preview";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";
import { ObservationSourceIssue } from "./ObservationList";

const FIELD_LIMIT = 6;
const LIMITATION_LIMIT = 8;
const text = (value: string) => contextPreview(value).text;

/** Native meanings come from the shared report; elastic-pool membership is separate context. */
export function PoolSettings({ report }: { report: PoolSettingsReport }) {
  const [expanded, setExpanded] = useState(false);
  const bounds = report.context?.kind === "native" ? report.context.context : report.context;
  return <section className="operational-health observation-list" aria-label={text(report.title)}>
    <h3>{text(report.title)}</h3><p>{text(report.summary)}</p>
    {report.source_state !== "collected" && <ObservationSourceIssue title={t("provider.mssql.source.pool_governance_settings")} source={{ source: t("provider.mssql.source.pool_governance_settings"), state: report.source_state, reason: null }} />}
    <details onToggle={(event) => setExpanded(event.currentTarget.open)}>
      <summary>{t("view.pool_settings.details")}</summary>
      {expanded && <>
        {bounds && <p className="muted">{t("ui.memory_failures.read_between", { from: text(bounds.observed_from), until: text(bounds.observed_until) })}</p>}
        <dl>
          <NativeValue label={t("view.pool_settings.database")} value={report.member_database ? text(report.member_database) : null} />
          {report.elastic_pool && <NativeValue label={t("view.pool_settings.pool")} value={text(report.elastic_pool.native.id)} />}
          <NativeValue label={t("view.pool_settings.logical_server")} value={report.logical_server ? text(report.logical_server) : null} />
          <NativeValue label={t("view.pool_settings.native_clock")} value={report.observed_at ? text(report.observed_at) : null} />
        </dl>
        {!report.read_complete && <p>{t("ui.common.read_partial")}</p>}
        <PoolSettingFields report={report} />
        {report.fields.length > FIELD_LIMIT && <p className="muted">{t("ui.pool_settings.fields_limited", { shown: digits(String(FIELD_LIMIT)), available: digits(String(report.fields.length)) })}</p>}
        <p>{text(report.investigation)}</p><p>{text(report.verify)}</p>
        {report.limitations.slice(0, LIMITATION_LIMIT).map((limitation, index) => <p className="muted" key={`${index}:${limitation.slice(0, 32)}`}>{text(limitation)}</p>)}
      </>}
    </details>
  </section>;
}

export function PoolSettingFields({ report }: { report: PoolSettingsReport }) {
  return <div className="operational-grid observation-grid">{report.fields.slice(0, FIELD_LIMIT).map((field, index) => <article className="operational-card" key={`${index}:${field.key.slice(0, 32)}`}>
    <dl><NativeValue label={text(field.label)} value={field.value == null ? null : text(field.value)} /></dl>
    <p><code>{text(field.key)}</code></p><p>{text(field.explanation)}</p>
  </article>)}</div>;
}
