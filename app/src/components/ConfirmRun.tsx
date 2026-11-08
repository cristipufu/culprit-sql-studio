import { TriangleAlert } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import type { ConnectionInfo } from "../gen/ConnectionInfo";
import { t } from "../lib/i18n";
import { tx } from "../lib/message-parts";
import type { PendingRun } from "../lib/run-batch";


/**
 * Asked before a statement that changes data runs on production. Cancel has focus; a statement that touches every row
 * or removes an object also needs the connection's name typed, so a reflexive Enter can't run it.
 */
export function ConfirmRun({ pending, connection, onCancel, onConfirm }: { pending: PendingRun; connection: ConnectionInfo; onCancel: () => void; onConfirm: () => void }) {
  const { check, sql, lint = [] } = pending;
  const risky = Boolean(check.warning || check.problem || lint.length > 0);
  const [typed, setTyped] = useState("");
  const cancel = useRef<HTMLButtonElement>(null);
  const input = useRef<HTMLInputElement>(null);
  const ready = !risky || typed.trim() === connection.name;

  useEffect(() => {
    (risky ? input.current : cancel.current)?.focus();
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onCancel();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [risky, onCancel]);

  const lines = sql.split("\n");
  const preview = lines.length > 14 ? [...lines.slice(0, 14), t("ui.confirm_run.more_lines", { count: lines.length - 14 })].join("\n") : sql;
  const pill = <span className="env-pill production">{connection.name}</span>;

  return (
    <div className="modal-backdrop" onMouseDown={(e) => e.target === e.currentTarget && onCancel()}>
      <div className="modal confirm-run" role="alertdialog" aria-modal="true" aria-labelledby="confirm-run-title">
        <div className="confirm-head">
          <TriangleAlert size={18} />
          <div>
            <h2 id="confirm-run-title">{t("ui.confirm_run.title")}</h2>
            <div className="muted">
              {check.verb ? tx("ui.confirm_run.runs_on", { verb: check.verb }, { connection: pill }) : tx("ui.confirm_run.statement_runs_on", {}, { connection: pill })}
            </div>
          </div>
        </div>
        {(check.warning || check.problem) && <div className="confirm-warning">{check.problem ?? check.warning}</div>}
        {lint.map((f) => (
          <div key={f.id} className="confirm-warning">
            <div>
              <b>{f.title}</b>
            </div>
            {f.fix?.summary && <div className="muted" style={{ marginTop: 2 }}>{f.fix.summary}</div>}
          </div>
        ))}
        <pre className="confirm-sql">{preview}</pre>
        {risky && (
          <label className="confirm-type">
            {tx("ui.confirm_run.type_name", {}, { connection: <b>{connection.name}</b> })}
            <input ref={input} value={typed} onChange={(e) => setTyped(e.target.value)} onKeyDown={(e) => e.key === "Enter" && ready && onConfirm()} spellCheck={false} autoComplete="off" />
          </label>
        )}
        <div className="confirm-actions">
          <button type="button" ref={cancel} className="btn" onClick={onCancel}>
            {t("ui.common.cancel")}
          </button>
          <button type="button" className="btn danger" disabled={!ready} onClick={onConfirm}>
            {t("ui.confirm_run.run_on", { connection: connection.name })}
          </button>
        </div>
      </div>
    </div>
  );
}
