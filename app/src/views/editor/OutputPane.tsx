import { Bookmark } from "lucide-react";
import type { ReactNode } from "react";
import { LookToggle, PlanSurface, type PlanLook } from "../../components/PlanSurface";
import { ResultGrid } from "../../components/ResultGrid";
import type { Plan } from "../../gen/Plan";
import type { ResultSet } from "../../gen/ResultSet";
import type { SavedQuery } from "../../gen/SavedQuery";
import type { ApiError } from "../../lib/api";
import { duration, oneLine } from "../../lib/format";
import { keys } from "../../lib/keys";

export type StatementResult = { result: ResultSet; sql: string };

/** What a tab's last action left below the editor. */
export type Output =
  | { kind: "none" }
  | { kind: "busy"; label: string }
  | { kind: "plan"; plan: Plan }
  | { kind: "rows"; results: StatementResult[] }
  | { kind: "error"; error: ApiError };

/** "1,000 rows (first 1,000; more exist)": how many rows came back, and whether the server had more. */
function rowCount(r: ResultSet): string {
  const n = r.rows.length.toLocaleString();
  return `${n} row${r.rows.length === 1 ? "" : "s"}${r.truncated ? ` (first ${n}; more exist)` : ""}`;
}

function statements(n: number): string {
  return `${n} statement${n === 1 ? "" : "s"}`;
}

/** Before anything ran: the shortcuts, and this connection's saved queries one click away. */
export function EmptyOutput({ saved, onOpen }: { saved: SavedQuery[]; onOpen: (q: SavedQuery) => void }) {
  const shortcuts: [string, string][] = [
    [keys("⌘↵"), "Run the selection, or the whole tab"],
    [keys("⌘E"), "Plan without running"],
    [keys("⌘⇧E"), "Run and measure each step"],
    [keys("⌘S"), "Save as a named query"],
  ];
  return (
    <div className="editor-empty">
      <ul className="editor-keys">
        {shortcuts.map(([key, label]) => (
          <li key={key}>
            <span className="kbd">{key}</span> {label}
          </li>
        ))}
      </ul>
      {saved.length > 0 && (
        <div className="editor-recent">
          <div className="eyebrow">Saved for this connection</div>
          {saved.map((q) => (
            <button key={q.name} className="editor-recent-item" onClick={() => onOpen(q)}>
              <Bookmark size={12} />
              <span>{q.name}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export function OutputPane({ output, look, onLook, empty }: { output: Output; look: PlanLook; onLook: (look: PlanLook) => void; empty: ReactNode }) {
  switch (output.kind) {
    case "none":
      return <>{empty}</>;
    case "busy":
      return (
        <div className="empty">
          <div className="spinner" />
          {output.label}
        </div>
      );
    case "error":
      // "Not available here" (the browser demo, an engine that can't) is information, not a failure.
      return (
        <div className={`notice ${output.error.code === "unsupported" ? "info" : "error"}`} style={{ margin: 12 }}>
          <div>
            <b>{output.error.message}</b>
            {output.error.hint && <div style={{ marginTop: 4 }}>{output.error.hint}</div>}
          </div>
        </div>
      );
    case "plan":
      return (
        <div style={{ padding: 12 }}>
          <div className="output-head">
            <LookToggle look={look} onChange={onLook} />
          </div>
          <PlanSurface plan={output.plan} look={look} />
        </div>
      );
    case "rows":
      return <Results results={output.results} />;
  }
}

/** One statement's rows or message, or a batch: its commands summed in one line and each result set in its own grid. */
function Results({ results }: { results: StatementResult[] }) {
  if (results.length === 1) {
    const r = results[0]!.result;
    if (r.columns.length === 0) {
      return (
        <div className="result-message">
          <b>{r.messages.join(" ") || "Done."}</b> <span className="muted">· {duration(r.elapsed_ms)}</span>
        </div>
      );
    }
    return <ResultGrid result={r} summary={`${rowCount(r)} · ${duration(r.elapsed_ms)}`} />;
  }

  const rowResults = results.filter((item) => item.result.columns.length > 0);
  const commandResults = results.filter((item) => item.result.columns.length === 0);
  const commandElapsed = commandResults.reduce((sum, item) => sum + item.result.elapsed_ms, 0);
  const commandMessages = commandResults.flatMap((item) => item.result.messages).filter(Boolean);

  if (rowResults.length === 0) {
    return (
      <div className="result-message">
        <b>{commandMessages.join(" · ") || `${statements(commandResults.length)} completed.`}</b>{" "}
        <span className="muted">· {duration(commandElapsed)}</span>
      </div>
    );
  }

  if (rowResults.length === 1 && commandResults.length > 0) {
    const r = rowResults[0]!.result;
    const commands = `${statements(commandResults.length)} completed (${duration(commandElapsed)})`;
    return <ResultGrid result={r} summary={`${commands} · ${rowCount(r)} · ${duration(r.elapsed_ms)}`} />;
  }

  return (
    <div className="multi-results" style={{ display: "flex", flexDirection: "column", gap: 12, height: "100%", overflowY: "auto", paddingBottom: 12 }}>
      {commandResults.length > 0 && (
        <div className="result-message" style={{ paddingBottom: 0 }}>
          <b>{commandMessages.join(" · ") || `${statements(commandResults.length)} completed`}</b>{" "}
          <span className="muted">· {duration(commandElapsed)}</span>
        </div>
      )}
      {rowResults.map((item, idx) => {
        const r = item.result;
        const rawSnippet = oneLine(item.sql.replace(/--.*$/gm, "").trim());
        const snippet = rawSnippet.length > 72 ? `${rawSnippet.slice(0, 71)}…` : rawSnippet;
        const summary = `Result ${idx + 1} · ${rowCount(r)} · ${duration(r.elapsed_ms)}${snippet ? ` · ${snippet}` : ""}`;
        return (
          <div key={idx} className="multi-result-pane" style={{ minHeight: 220, flex: "1 0 auto", display: "flex", flexDirection: "column", borderTop: idx > 0 || commandResults.length > 0 ? "1px solid var(--border)" : undefined }}>
            <ResultGrid result={r} summary={summary} />
          </div>
        );
      })}
    </div>
  );
}
