import type { Report } from "../gen/Report";
import { queryHistoryEmptyState } from "../lib/query-history";

export function QueryHistoryEmpty({ report, className = "empty" }: { report: Report; className?: string }) {
  const state = queryHistoryEmptyState(report);
  if (!state) return null;
  return (
    <div className={className}>
      <h3>{state.title}</h3>
      <p>{state.detail}</p>
      {state.sourceReason && <p className="muted">{state.sourceReason}</p>}
    </div>
  );
}
