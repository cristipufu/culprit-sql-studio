import { Play, RefreshCw, Save, ScanSearch, Zap } from "lucide-react";
import type { Engine } from "../../gen/Engine";
import type { ExplainMode } from "../../gen/ExplainMode";
import { can, measuresRuns } from "../../lib/engines";
import { t } from "../../lib/i18n";
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
      <button type="button" className="btn primary" onClick={onRun} title={t("ui.editor.run_title")}>
        <Play size={13} /> {t("ui.editor.toolbar.run")} <span className="kbd">{keys("⌘↵")}</span>
      </button>
      <button type="button" className="btn" onClick={() => onExplain("estimated")} title={t("ui.editor.toolbar.explain_title")}>
        <ScanSearch size={14} /> {t("ui.editor.toolbar.explain")} <span className="kbd">{keys("⌘E")}</span>
      </button>
      {measuresRuns(engine) && (
        <button type="button" className="btn" onClick={() => onExplain("analyze")} title={t("ui.editor.toolbar.analyze_title")}>
          <Zap size={13} /> {t("ui.editor.toolbar.analyze")} <span className="kbd">{keys("⌘⇧E")}</span>
        </button>
      )}
      {naming === null ? (
        <button type="button" className="btn" onClick={() => onSave()} title={savedName ? t("ui.editor.toolbar.update_title", { name: savedName }) : t("ui.editor.save_as_title")}>
          <Save size={13} /> {savedName ? t("ui.common.save") : t("ui.editor.toolbar.save_as")} <span className="kbd">{keys("⌘S")}</span>
        </button>
      ) : (
        <form
          className="name-query"
          onSubmit={(e) => {
            e.preventDefault();
            if (naming.trim()) onSave(naming.trim());
          }}
        >
          <input autoFocus value={naming} placeholder={t("ui.editor.toolbar.name_placeholder")} onChange={(e) => onNaming(e.target.value)} onKeyDown={(e) => e.key === "Escape" && (onNaming(null), onCancelNaming())} />
          <button className="btn primary" type="submit" disabled={!naming.trim()}>
            {t("ui.common.save")}
          </button>
        </form>
      )}
      <span className="spacer" />
      <RunsHere engine={engine} production={production} />
      <button type="button" className="icon-btn" title={catalogBusy
        ? t("ui.editor.toolbar.catalog_loading")
        : tables !== null
          ? t("ui.editor.toolbar.catalog_reload_count", { n: tables, count: String(tables) })
          : t("ui.editor.toolbar.catalog_reload")} onClick={onRefreshCatalog} disabled={catalogBusy}>
        <RefreshCw size={13} className={catalogBusy ? "spin" : ""} />
      </button>
    </div>
  );
}

/** What running a statement does on this connection. */
function RunsHere({ engine, production }: { engine: Engine; production: boolean }) {
  if (!can(engine, "writes")) {
    return (
      <span className="env-note" title={t("ui.editor.toolbar.read_only_title")}>
        {t("ui.editor.toolbar.read_only")}
      </span>
    );
  }
  if (production) {
    return (
      <span className="env-chip env-production" title={t("ui.editor.toolbar.production_title")}>
        {t("ui.editor.toolbar.production")}
      </span>
    );
  }
  return (
    <span className="env-note" title={t("ui.editor.toolbar.commits_title")}>
      {t("ui.editor.toolbar.commits")}
    </span>
  );
}
