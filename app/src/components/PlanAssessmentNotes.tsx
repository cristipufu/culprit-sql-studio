import { ChevronDown, Info } from "lucide-react";
import { t } from "../lib/i18n";

export function PlanAssessmentNotes({ note }: { note: string | null }) {
  if (!note) return null;
  return (
    <details className="ins-context">
      <summary>
        <Info size={13} />
        {t("ui.plan_insights.assessment_notes")}
        <ChevronDown size={13} className="ins-context-chevron" />
      </summary>
      <p className="ins-note">{note}</p>
    </details>
  );
}
