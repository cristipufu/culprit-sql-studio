import type { PageServerReadExplanation } from "../gen/PageServerReadExplanation";
import { t } from "../lib/i18n";
import { NativeValue } from "./NativeObservationFields";

/** Rust qualifies the request and owns exact formatting and scope. No browser counter math or diagnosis. */
export function RequestPageServerReads({ report }: { report: PageServerReadExplanation }) {
  return <details className="qp-current-work">
    <summary>{report.title}</summary>
    <p>{report.summary}</p>
    <dl>{report.metrics.map((metric, index) => <NativeValue key={metric.key ?? index} label={metric.label} value={metric.text} />)}</dl>
    <details><summary>{t("ui.memory_failures.limitations")}</summary>
      {report.limitations.map((limitation, index) => <p className="muted" key={index}>{limitation}</p>)}
    </details>
    <details><summary>{t("ui.memory_failures.investigation")}</summary>
      <p>{report.investigate}</p><p>{report.verify}</p>
    </details>
  </details>;
}
