import type { ReadWorkSummary } from "../gen/ReadWorkSummary";
import type { SourceConnectionExplanation } from "../gen/SourceConnectionExplanation";
import type { SourceNativeIntervalExplanation } from "../gen/SourceNativeIntervalExplanation";
import { digits, duration } from "../lib/format";
import { t } from "../lib/i18n";
/** Shared numerical leaves only. Each caller retains its own current or historical scope. */
export function ConnectionWorkDetails({ report }: { report: SourceConnectionExplanation }) {
  return (
    <details>
      <summary>{report.summary}</summary>
      {report.observations.map((text, index) => (
        <p key={index}>{text}</p>
      ))}
      {report.limitations.map((text, index) => (
        <p className="muted" key={index}>
          {text}
        </p>
      ))}
    </details>
  );
}
export function NativeReadDetails({ report }: { report: SourceNativeIntervalExplanation }) {
  return (
    <details>
      <summary>{report.summary}</summary>
      {report.measurements.map((text, index) => (
        <p key={index}>{text}</p>
      ))}
      {report.limitations.map((text, index) => (
        <p className="muted" key={index}>
          {text}
        </p>
      ))}
    </details>
  );
}
export function ReadTemplateValues({ metrics, order }: { metrics: ReadWorkSummary; order: number[] }) {
  return (
    <>
      {order.map((index) => {
        const metric = metrics.templates[index];
        return (
          metric && (
            <div key={metric.template_id} style={{ overflowWrap: "anywhere" }}>
              <p>
                {t("view.source_overhead.cli.template", {
                  id: metric.template_id,
                  elapsed: duration(metric.client_elapsed_ms),
                  completed: digits(String(metric.completed_tasks)),
                  failures: digits(String(metric.failures)),
                })}
              </p>
              <p>
                {t("view.source_overhead.cli.values", {
                  rows: digits(metric.rows_decoded),
                  bytes: digits(metric.charged_value_bytes),
                  timeouts: digits(String(metric.timeouts)),
                  cancellations: digits(String(metric.cancellations)),
                  unconfirmed: digits(String(metric.unconfirmed_cancellations)),
                })}
              </p>
            </div>
          )
        );
      })}
    </>
  );
}
