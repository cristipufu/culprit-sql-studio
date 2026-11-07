import type { Finding } from "../gen/Finding";

/** What a person can do with a finding: an inactive one is history, whatever its classification. */
export function actionability(f: Finding): Finding["actionability"] {
  return f.inactive ? "history" : f.actionability;
}
export function isActionable(f: Finding): boolean {
  return !f.inactive && ["needs-attention", "optional-improvement"].includes(actionability(f));
}
export function actionLabel(f: Finding): string {
  if (f.inactive) return "Stopped running";
  return { "needs-attention": "Needs attention", "optional-improvement": "Optional improvement", information: "Information", history: "History" }[actionability(f)];
}
