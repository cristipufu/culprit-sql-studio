import { Play, RefreshCw, Save, ScanSearch, Zap } from "lucide-react";
import type { Engine } from "../../gen/Engine";
import type { ExplainMode } from "../../gen/ExplainMode";
import { can, measuresRuns } from "../../lib/engines";
import { keys } from "../../lib/keys";

type Props = {
  engine: Engine;
  production: boolean;
  /** The saved query this tab holds, if any. */
  savedName: string | null;
  /** The name being typed for a new saved query; null when not naming. */
  naming: string | null;
  onNaming: (name: string | null) => void;
  onRun: () => void;
  onExplain: (mode: ExplainMode) => void;
  onSave: (name?: string) => void;
  /** Leave naming with Escape: back to the editor. */
  onCancelNaming: () => void;
  catalogBusy: boolean;
  /** Tables the completion knows, when it has a catalog. */
  tables: number | null;
  onRefreshCatalog: () => void;
};

/** Run, plan, measure and save, what running does on this connection, and reloading completion's names. */
export function Toolbar({ engine, production, savedName, naming, onNaming, onRun, onExplain, onSave, onCancelNaming, catalogBusy, tables, onRefreshCatalog }: Props) {
  return (
    <div className="toolbar">
      <button className="btn primary" onClick={onRun} title="Run the selection, or the whole tab">
        <Play size={13} /> Run <span className="kbd">{keys("⌘↵")}</span>
      </button>
      <button className="btn" onClick={() => onExplain("estimated")} title="Show the plan without running the statement">
        <ScanSearch size={14} /> Explain <span className="kbd">{keys("⌘E")}</span>
      </button>
      {measuresRuns(engine) && (
        <button className="btn" onClick={() => onExplain("analyze")} title="Run the statement and measure each step">
          <Zap size={13} /> Analyze <span className="kbd">{keys("⌘⇧E")}</span>
        </button>
      )}
      {naming === null ? (
        <button className="btn" onClick={() => onSave()} title={savedName ? `Update “${savedName}”` : "Save as a named query"}>
          <Save size={13} /> {savedName ? "Save" : "Save as…"} <span className="kbd">{keys("⌘S")}</span>
        </button>
      ) : (
        <form
          className="name-query"
          onSubmit={(e) => {
            e.preventDefault();
            if (naming.trim()) onSave(naming.trim());
          }}
        >
          <input autoFocus value={naming} placeholder="Name it, e.g. Open orders by customer" onChange={(e) => onNaming(e.target.value)} onKeyDown={(e) => e.key === "Escape" && (onNaming(null), onCancelNaming())} />
          <button className="btn primary" type="submit" disabled={!naming.trim()}>
            Save
          </button>
        </form>
      )}
      <span className="spacer" />
      <RunsHere engine={engine} production={production} />
      <button className="icon-btn" title={catalogBusy ? "Loading table and column names…" : `Reload table and column names${tables !== null ? ` (${tables} tables)` : ""}`} onClick={onRefreshCatalog} disabled={catalogBusy}>
        <RefreshCw size={13} className={catalogBusy ? "spin" : ""} />
      </button>
    </div>
  );
}

/** What running a statement does on this connection. */
function RunsHere({ engine, production }: { engine: Engine; production: boolean }) {
  if (!can(engine, "writes")) {
    return (
      <span className="env-note" title="Culprit opens the file read-only: statements that read run; anything that would change it is refused">
        Read-only file
      </span>
    );
  }
  if (production) {
    return (
      <span className="env-chip env-production" title="Reads run as they are; anything that changes data asks first, and statements that touch every row need the connection's name typed">
        Production · changes ask first
      </span>
    );
  }
  return (
    <span className="env-note" title="Statements Culprit can prove only read run in a read-only transaction; anything else commits">
      Reads roll back · changes commit
    </span>
  );
}
