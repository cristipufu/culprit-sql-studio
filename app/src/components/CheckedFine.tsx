import { Check, ChevronRight, Info } from "lucide-react";
import { useState } from "react";
import type { Report } from "../gen/Report";
import { engineLimitations } from "../lib/engines";
import { t } from "../lib/i18n";

/** The pipeline's evidence assessment. No finding alone never means fine. */
export function CheckedFine({ report }: { report: Report }) {
  const [open, setOpen] = useState(report.findings.length <= 3);
  const assessments = report.assessments;
  const assessed = assessments.filter((c) => c.status === "assessed");
  const limited = assessments.filter((c) => c.status === "insufficient-evidence" || c.status === "unavailable" || (c.status === "findings" && c.reason));
  const inapplicable = assessments.filter((c) => c.status === "not-applicable");
  return (
    <div className="checked-fine">
      <button type="button" className="checked-head" onClick={() => setOpen((o) => !o)} aria-expanded={open}>
        <ChevronRight size={13} className={open ? "turned" : ""} />
        <span>{t("ui.checked_fine.title")} <span className="n">{t("ui.checked_fine.counts", { assessed: String(assessed.length), limited: String(limited.length) })}</span></span>
      </button>
      {open && <>
        {assessed.length > 0 && <ul>{assessed.map((c) => <li key={c.check}>
          <Check size={13} className="ok" /><span><span className="t">{c.title}</span><span className="d">{t("ui.checked_fine.no_finding")}</span></span>
        </li>)}</ul>}
        {limited.length > 0 && <details><summary>{t("ui.checked_fine.limited", { count: String(limited.length) })}</summary><p className="muted">{t("ui.checked_fine.limited_note")}</p><ul>{limited.map((c) => <li key={c.check}>
          <Info size={13} /><span><span className="t">{c.title} · {c.status === "unavailable" ? t("ui.checked_fine.unavailable") : t("ui.checked_fine.insufficient")}</span><span className="d">{c.reason}</span></span>
        </li>)}</ul></details>}
        {inapplicable.length > 0 && <details><summary>{t("ui.checked_fine.not_applicable", { count: String(inapplicable.length) })}</summary><ul>{inapplicable.map((c) => <li key={c.check}><span><span className="t">{c.title}</span><span className="d">{c.reason}</span></span></li>)}</ul></details>}
        <details><summary>{t("ui.checked_fine.cannot_establish")}</summary><ul>{engineLimitations(report.snapshot.server.engine).map((text) => <li key={text}><Info size={13} /><span>{text}</span></li>)}</ul></details>
      </>}
    </div>
  );
}
