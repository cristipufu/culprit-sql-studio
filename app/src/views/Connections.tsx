import { Bot, Cloud, Database, Import, LockKeyhole, Plus, Search, ShieldCheck } from "lucide-react";
import { useState, type ReactNode } from "react";
import { Logo } from "../components/Logo";
import type { Cloud as CloudKind } from "../gen/Cloud";
import { serverName } from "../lib/engines";
import { ENVIRONMENT_LABEL, groupByEnvironment } from "../lib/environments";
import { t } from "../lib/i18n";
import { useStore } from "../lib/store";
import { ConnectionCard } from "./connections/ConnectionCard";
import { DiscoverPanel, ImportPanel } from "./connections/DiscoverPanel";
import { NewConnectionForm } from "./connections/NewConnectionForm";
import { slot, withElements } from "../lib/message-parts";

export function Connections() {
  const connections = useStore((s) => s.connections);
  const [panel, setPanel] = useState<Panel>("none");
  const active = panel;
  const firstRun = connections.length === 0;
  const [filter, setFilter] = useState("");
  const q = filter.trim().toLowerCase();
  const shown = q ? connections.filter((c) => `${c.name} ${c.display} ${serverName(c.engine, c.last_report?.product)} ${c.environment}`.toLowerCase().includes(q)) : connections;

  return (
    <div className="connections">
      <div className="connections-inner">
        <div className="brand" data-tauri-drag-region>
          <Logo size={36} />
          <div>
            <h1>Culprit</h1>
            <p>{t("ui.connections.tagline")}</p>
          </div>
        </div>
        {firstRun && <FirstRun chosen={active} onChoose={(next) => setPanel(next === active ? "none" : next)} />}
        {/* Adding comes first: with many connections, the actions and the form they open stay in view, above the list. */}
        {!firstRun && (
          <div className="conn-actions">
            <button type="button" className={`btn ${active === "new" || connections.length === 0 ? "primary" : ""}`} onClick={() => setPanel(active === "new" ? "none" : "new")}>
              <Plus size={14} /> {t("ui.connections.new")}
            </button>
            <button type="button" className={`btn ${active === "azure" ? "primary" : ""}`} onClick={() => setPanel(active === "azure" ? "none" : "azure")} title={t("ui.connections.from_azure_title")}>
              <Cloud size={14} /> {t("ui.connections.from_azure")}
            </button>
            <button type="button" className={`btn ${active === "aws" ? "primary" : ""}`} onClick={() => setPanel(active === "aws" ? "none" : "aws")} title={t("ui.connections.from_aws_title")}>
              <Cloud size={14} /> {t("ui.connections.from_aws")}
            </button>
            <button type="button" className={`btn ${active === "import" ? "primary" : ""}`} onClick={() => setPanel(active === "import" ? "none" : "import")} title={t("ui.connections.import_title")}>
              <Import size={14} /> {t("ui.connections.import")}
            </button>
          </div>
        )}
        {active !== "none" && (
          <div className="conn-panel">
            {active === "new" && <NewConnectionForm onDone={() => setPanel("none")} canCancel />}
            {(active === "azure" || active === "aws") && <DiscoverPanel key={active} cloud={active} onDone={() => setPanel("none")} />}
            {active === "import" && <ImportPanel onDone={() => setPanel("none")} />}
          </div>
        )}
        {connections.length > 6 && (
          <label className="search conn-filter">
            <Search size={13} />
            <input value={filter} placeholder={t("ui.connections.filter_placeholder", { count: connections.length })} onChange={(e) => setFilter(e.target.value)} aria-label={t("ui.connections.filter")} />
          </label>
        )}
        {q && shown.length === 0 && <p className="muted">{t("ui.connections.no_match", { filter })}</p>}
        {groupByEnvironment(shown).map((group) => (
          <section key={group.environment} style={{ marginBottom: 18 }}>
            <div className="eyebrow" style={{ marginBottom: 8 }}>
              {ENVIRONMENT_LABEL[group.environment]}
            </div>
            <div className="conn-grid">
              {group.items.map((c) => <ConnectionCard key={c.name} connection={c} />)}
            </div>
          </section>
        ))}
      </div>
    </div>
  );
}

type Panel = "none" | "new" | "import" | CloudKind;

function firstRunTiles(): { panel: Exclude<Panel, "none">; icon: ReactNode; title: string; text: string }[] {
  return [
    { panel: "new", icon: <Database size={18} />, title: t("ui.connections.first_run.new_title"), text: t("ui.connections.first_run.new_text") },
    { panel: "azure", icon: <Cloud size={18} />, title: t("ui.connections.first_run.azure_title"), text: t("ui.connections.first_run.azure_text") },
    { panel: "aws", icon: <Cloud size={18} />, title: t("ui.connections.first_run.aws_title"), text: t("ui.connections.first_run.aws_text") },
    { panel: "import", icon: <Import size={18} />, title: t("ui.connections.first_run.import_title"), text: t("ui.connections.first_run.import_text") },
  ];
}

/** No connections yet: four ways in, and what Culprit promises before anyone types a password. */
function FirstRun({ chosen, onChoose }: { chosen: Panel; onChoose: (panel: Exclude<Panel, "none">) => void }) {
  return (
    <div className="first-run">
      <div className="first-tiles">
        {firstRunTiles().map((tile) => (
          <button type="button" key={tile.panel} className={`first-tile ${chosen === tile.panel ? "on" : ""}`} aria-pressed={chosen === tile.panel} onClick={() => onChoose(tile.panel)}>
            <span className="first-icon">{tile.icon}</span>
            <b>{tile.title}</b>
            <span>{tile.text}</span>
          </button>
        ))}
      </div>
      <ul className="first-promises">
        <li>
          <ShieldCheck size={13} /> {t("ui.connections.promise.read_only")}
        </li>
        <li>
          <LockKeyhole size={13} /> {t("ui.connections.promise.keychain")}
        </li>
        <li>
          <Bot size={13} /> {withElements(t("ui.connections.promise.agents", { command: slot(0) }), [<code key="mcp">culprit mcp</code>])}
        </li>
      </ul>
    </div>
  );
}
