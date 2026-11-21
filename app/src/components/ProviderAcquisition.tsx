import type { ProviderAcquisitionReport } from "../gen/ProviderAcquisitionReport";
import type { ReadWorkExplanation } from "../gen/ReadWorkExplanation";
import type { ReadWorkSummary } from "../gen/ReadWorkSummary";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";
import { ObservationList } from "./ObservationList";
import { ConnectionWorkDetails, NativeReadDetails, ReadTemplateValues } from "./ReadWorkDetails";

/** Current client acquisition only; historical captures keep their own original source/time. */
export function ProviderAcquisition({ report }: { report: ProviderAcquisitionReport | null | undefined }) {
  if (!report) return null;
  if (report.kind === "unavailable")
    return (
      <details className="operational-card">
        <summary>{t("view.provider_acquisition.title")}</summary>
        <p>{report.reason}</p>
      </details>
    );
  const evidence = report.evidence;
  return (
    <ObservationList
      collapseEntries
      source={report.title}
      scope={t("view.provider_acquisition.scope")}
      report={{
        title: report.title,
        summary: `${report.summary} ${report.settlement} ${report.coverage}`,
        sources: [],
        limitations: report.limitations,
        entries: report.readers,
      }}
      renderEntry={(explained) => {
        const reader = evidence.readers.find((value) => value.reader_id === explained.reader_id);
        if (!reader) return null;
        return (
          <article className="operational-card" key={reader.reader_id}>
            {reader.queries != null && (
              <p>
                {t("view.provider_acquisition.reader", {
                  reader: digits(String(reader.reader_id)),
                  count: digits(String(reader.queries)),
                })}
              </p>
            )}
            <Work report={explained.work} metrics={reader.work} />
            {reader.setup_accounting === "separate" && reader.setup_queries != null && (
              <p>{t("view.provider_acquisition.setup", { count: digits(String(reader.setup_queries)) })}</p>
            )}
            {explained.setup_work && <Work report={explained.setup_work} metrics={reader.setup_work} />}
          </article>
        );
      }}
    >
      <p className="muted">
        {t("view.source_overhead.cli.bounds", { from: evidence.client_started_at, until: evidence.client_finished_at })}
      </p>
      {report.total_admissions != null && (
        <p>{t("view.provider_acquisition.total", { count: digits(report.total_admissions) })}</p>
      )}
    </ObservationList>
  );
}
function Work({ report, metrics }: { report: ReadWorkExplanation; metrics: ReadWorkSummary | null }) {
  return (
    <details>
      <summary>{report.summary}</summary>
      {report.connection_work && <ConnectionWorkDetails report={report.connection_work} />}
      {metrics && <ReadTemplateValues metrics={metrics} order={report.template_order} />}
      {report.native_interval && <NativeReadDetails report={report.native_interval} />}
      {report.limitations.map((text, index) => (
        <p className="muted" key={index}>
          {text}
        </p>
      ))}
    </details>
  );
}
