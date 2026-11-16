import type { QueryProgress } from "../gen/QueryProgress";
import { percent } from "../lib/format";
import { t } from "../lib/i18n";
import { progressFraction } from "../lib/query-progress";

/** The source chooses the scope; a maintenance phase can never fill the statement-progress field. */
export function QueryProgressMeters({ progress }: { progress: QueryProgress }) {
  const whole = progressFraction(progress.percent);
  const phase = progressFraction(progress.phase_percent);
  if (whole == null && phase == null && !progress.phase) return null;
  return <div className="qp-progress">
    {progress.phase && <div className="qp-phase">{progress.phase}</div>}
    {phase != null && <ProgressMeter value={phase} scope="phase" caption={t("ui.query_progress.phase_only")} basis={progress.phase_percent_basis} />}
    {whole != null && <ProgressMeter value={whole} scope="statement"
      caption={progress.nodes.length > 0 ? t("ui.query_progress.basis_rows") : t("ui.query_progress.basis_engine")}
      basis={progress.percent_basis} />}
  </div>;
}

function ProgressMeter({ value, scope, caption, basis }: { value: number; scope: "phase" | "statement"; caption: string; basis?: string | null }) {
  return <div className={`qp-meter qp-meter-${scope}`}>
    <div className="qp-bar-row">
      <span className="qp-bar" role="progressbar" aria-label={caption} aria-valuemin={0} aria-valuemax={100}
        aria-valuenow={Math.round(value * 100)} data-progress-scope={scope}>
        <span style={{ width: `${Math.round(value * 100)}%` }} />
      </span>
      <b>{percent(value)}</b>
    </div>
    <div className="qp-basis" title={basis ?? undefined}>{caption}</div>
  </div>;
}
