import type { Finding } from "../gen/Finding";
import { t } from "./i18n";

/** What a person can do with a finding: an inactive one is history, whatever its classification. */
export function actionability(f: Finding): Finding["actionability"] {
  return f.inactive ? "history" : f.actionability;
}
export function isActionable(f: Finding): boolean {
  return !f.inactive && ["needs-attention", "optional-improvement"].includes(actionability(f));
}
export function actionLabel(f: Finding): string {
  if (f.inactive) return t("ui.findings.stopped_running");
  switch (actionability(f)) {
    case "needs-attention":
      return t("ui.findings.needs_attention");
    case "optional-improvement":
      return t("ui.findings.optional_improvement");
    case "information":
      return t("ui.findings.information");
    case "history":
      return t("ui.findings.history");
  }
}
