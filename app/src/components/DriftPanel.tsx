import { ArrowLeft, Check, Copy, SquarePen } from "lucide-react";
import { useEffect, useState } from "react";
import type { Drift } from "../gen/Drift";
import type { DriftItem } from "../gen/DriftItem";
import type { DriftKind } from "../gen/DriftKind";
import type { DriftSide } from "../gen/DriftSide";
import type { Report } from "../gen/Report";
import { api, errorMessage, type ApiError } from "../lib/api";
import { ago } from "../lib/format";
import { t } from "../lib/i18n";
import { tx } from "../lib/message-parts";
import { setState, useStore } from "../lib/store";
import { Sql } from "./Sql";
import { serverName } from "../lib/engines";
import { copyAndTell } from "../lib/clipboard";
import { canOpenFixInEditor } from "../lib/editor-sql";
import { remember, remembered } from "../lib/prefs";

type DriftState = { status: "idle" } | { status: "loading" } | { status: "ready"; drift: Drift } | { status: "error"; error: ApiError };

/** The kinds of difference, in the order the report lists them, each with its heading. */
function groups(): { kind: DriftKind; label: string }[] {
  return [
    { kind: "index", label: t("ui.drift_panel.group.index") },
    { kind: "table", label: t("ui.drift_panel.group.table") },
    { kind: "column", label: t("ui.drift_panel.group.column") },
    { kind: "setting", label: t("ui.drift_panel.group.setting") },
    { kind: "version", label: t("ui.drift_panel.group.version") },
  ];
}


function pickKey(name: string): string {
  return `culprit.drift.${name}`;
}

/** What differs between this connection's latest scan and another connection of the same engine. */
export function DriftPanel({ report, onClose }: { report: Report; onClose: () => void }) {
  const active = useStore((s) => s.active) ?? report.snapshot.connection;
  const connections = useStore((s) => s.connections);
  const others = connections.filter((c) => c.engine === report.snapshot.server.engine && c.name !== active);
  const [other, setOther] = useState<string | null>(() => {
    const last = remembered(pickKey(active));
    return others.find((c) => c.name === last)?.name ?? others[0]?.name ?? null;
  });
  const [state, setDrift] = useState<DriftState>({ status: "idle" });

  useEffect(() => {
    if (!other) return;
    let cancelled = false;
    setDrift({ status: "loading" });
    api()
      .then((client) => client.drift(active, other))
      .then((drift) => !cancelled && setDrift({ status: "ready", drift }))
      .catch((e) => !cancelled && setDrift({ status: "error", error: errorMessage(e) }));
    return () => {
      cancelled = true;
    };
  }, [active, other]);

  const choose = (name: string) => {
    setOther(name);
    remember(pickKey(active), name);
  };

  return (
    <div className="drift">
      <div className="toolbar">
        <button type="button" className="btn ghost" onClick={onClose}>
          <ArrowLeft size={13} /> {t("ui.drift_panel.back")}
        </button>
        <span className="drift-with">
          {tx("ui.drift_panel.compare", {}, { connection: <b>{active}</b> })}
        </span>
        {others.map((c) => (
          <button type="button" key={c.name} className={`chip ${other === c.name ? "on" : ""}`} onClick={() => choose(c.name)}>
            <span className={`env-dot env-${c.environment}`} style={{ width: 6, height: 6, boxShadow: "none" }} /> {c.name}
          </button>
        ))}
      </div>
      <div className="drift-body">
        {others.length === 0 && (
          <div className="empty" style={{ padding: 40 }}>
            <h3>{t("ui.drift_panel.empty")}</h3>
            <p>{t("ui.drift_panel.empty_hint", { engine: serverName(report.snapshot.server.engine, report.snapshot.server.product) })}</p>
          </div>
        )}
        {state.status === "loading" && (
          <div className="empty" style={{ padding: 40 }}>
            <div className="spinner" />
            {t("ui.drift_panel.loading")}
          </div>
        )}
        {state.status === "error" && (
          <div className="notice" style={{ margin: 20 }}>
            <div>
              <b>{state.error.message}</b>
              {state.error.hint && <div className="muted">{state.error.hint}</div>}
            </div>
          </div>
        )}
        {state.status === "ready" && <DriftReport drift={state.drift} active={active} />}
      </div>
    </div>
  );
}

/** Product and version without repeating the version ("PostgreSQL 18.6 (Debian …)"). */
function serverLabel(side: DriftSide): string {
  const [first = "", ...rest] = side.version.split(" ");
  if (!side.product.endsWith(first)) return `${side.product} ${side.version}`;
  return rest.length ? `${side.product} ${rest.join(" ")}` : side.product;
}

function SideCard({ side, letter }: { side: DriftSide; letter: string }) {
  return (
    <div className="drift-side">
      <div className="name">
        <span className="drift-letter">{letter}</span>
        <span className={`env-dot env-${side.environment}`} style={{ width: 6, height: 6, boxShadow: "none" }} />
        <b>{side.connection}</b>
      </div>
      <div className="muted">
        {t("ui.drift_panel.side", { server: serverLabel(side), ago: ago(side.taken_at) })}
      </div>
    </div>
  );
}

function DriftReport({ drift, active }: { drift: Drift; active: string }) {
  return (
    <div className="drift-report">
      <div className="drift-sides">
        <SideCard side={drift.a} letter="A" />
        <SideCard side={drift.b} letter="B" />
      </div>
      <p className={`drift-summary ${drift.items.length === 0 ? "same" : ""}`}>
        {drift.items.length === 0 && <Check size={14} />} {drift.summary}
      </p>
      {groups().map(({ kind, label }) => {
        const items = drift.items.filter((i) => i.kind === kind);
        if (items.length === 0) return null;
        return (
          <section key={kind}>
            <h3>
              {label} <span className="muted">{items.length}</span>
            </h3>
            {items.map((item) => (
              <DriftRow key={`${item.kind}:${item.subject}:${item.presence}`} item={item} drift={drift} active={active} />
            ))}
          </section>
        );
      })}
    </div>
  );
}

/** Short badge text (the side cards say which connection is A and B) and the full wording for the tooltip. */
function presenceLabel(item: DriftItem, drift: Drift): [string, string] {
  switch (item.presence) {
    case "only-a":
      return [t("ui.drift_panel.only_a"), t("ui.drift_panel.only_in", { connection: drift.a.connection })];
    case "only-b":
      return [t("ui.drift_panel.only_b"), t("ui.drift_panel.only_in", { connection: drift.b.connection })];
    case "different":
      return [t("ui.drift_panel.different"), t("ui.drift_panel.different_hint")];
    case "renamed":
      return [t("ui.drift_panel.renamed"), t("ui.drift_panel.renamed_hint")];
  }
}

function DriftRow({ item, drift, active }: { item: DriftItem; drift: Drift; active: string }) {
  const compare = item.presence === "different" || item.presence === "renamed";
  const [badge, tooltip] = presenceLabel(item, drift);
  const definition = item.presence === "only-a" ? item.a : item.presence === "only-b" ? item.b : null;
  const copy = async () => {
    if (!item.sql) return;
    await copyAndTell(item.sql, t("ui.drift_panel.sql_copied"));
  };
  return (
    <div className={`drift-item ${item.presence}`}>
      <span className={`drift-badge ${item.presence}`} title={tooltip}>
        {badge}
      </span>
      <div className="drift-main">
        <div className="drift-text">{item.title}</div>
        {definition && <code className="drift-def">{definition}</code>}
        {compare && (item.a || item.b) && (
          <div className="drift-values">
            <span className="drift-letter">A</span>
            <code>{item.a ?? "—"}</code>
            <span className="drift-letter">B</span>
            <code>{item.b ?? "—"}</code>
          </div>
        )}
        {item.sql && (
          <div className="drift-sql">
            <div className="drift-sql-head">
              <span className="muted">
                {tx("ui.drift_panel.align_on", {}, { connection: <b>{item.sql_for}</b> })}
              </span>
              <span className="actions">
                <button type="button" className="btn ghost" onClick={copy}>
                  <Copy size={12} /> {t("ui.common.copy")}
                </button>
                {item.sql_for === active && canOpenFixInEditor(item.sql, drift.a.engine) && (
                  <button type="button" className="btn ghost" onClick={() => setState({ view: "editor", editorSql: item.sql })}>
                    <SquarePen size={12} /> {t("ui.common.open_in_editor")}
                  </button>
                )}
              </span>
            </div>
            <div className="codeblock">
              <Sql text={item.sql} pretty={false} />
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
