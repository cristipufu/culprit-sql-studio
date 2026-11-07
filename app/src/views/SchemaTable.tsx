import { Copy } from "lucide-react";
import { useId, useMemo, useState } from "react";
import { SchemaData } from "./SchemaData";
import { Sql } from "../components/Sql";
import type { Report } from "../gen/Report";
import type { TableStat } from "../gen/TableStat";
import { useHotkeys } from "../lib/hotkeys";
import { ddl, relations, tableFindings } from "../lib/schema-model";
import { copyAndTell } from "../lib/clipboard";
import { remember, rememberedChoice } from "../lib/prefs";
import { Header } from "./schema-table/Header";
import { Columns } from "./schema-table/Columns";
import { Indexes } from "./schema-table/Indexes";
import { Relations } from "./schema-table/Relations";

type Tab = "columns" | "indexes" | "relations" | "data" | "ddl";

const TAB_KEY = "culprit.schema.tab";

function rememberedTab(): Tab {
  return rememberedChoice<Tab>(TAB_KEY, "columns", ["columns", "indexes", "relations", "data", "ddl"]);
}

/** One table: what it is, how big, what's wrong with it, and tabs for its columns, indexes, relations, rows and DDL. */
export function SchemaTable({ report, table, onOpenTable }: { report: Report; table: TableStat; onOpenTable: (key: string) => void }) {
  const { snapshot } = report;
  const engine = snapshot.server.engine;
  const tabId = useId();
  const [tab, setTab] = useState<Tab>(rememberedTab);
  const indexes = useMemo(() => snapshot.indexes.filter((i) => i.schema === table.schema && i.table === table.name), [snapshot.indexes, table]);
  const columns = useMemo(
    () => snapshot.columns.filter((c) => c.schema === table.schema && c.table === table.name).sort((a, b) => a.ordinal - b.ordinal),
    [snapshot.columns, table],
  );
  const links = useMemo(() => relations(report, table), [report, table]);
  const findings = useMemo(() => tableFindings(report, table), [report, table]);
  const choose = (next: Tab) => {
    setTab(next);
    remember(TAB_KEY, next);
  };
  useHotkeys({
    c: () => choose("columns"),
    i: () => choose("indexes"),
    r: () => choose("relations"),
    p: () => choose("data"),
    d: () => choose("ddl"),
  });

  const tabs: { key: Tab; label: string; n?: number; hotkey: string }[] = [
    { key: "columns", label: "Columns", n: columns.length, hotkey: "C" },
    { key: "indexes", label: "Indexes", n: indexes.length, hotkey: "I" },
    {
      key: "relations",
      label: "Relations",
      n: links.outgoing.length + links.incoming.length,
      hotkey: "R",
    },
    { key: "data", label: "Data", hotkey: "P" },
    { key: "ddl", label: "DDL", hotkey: "D" },
  ];

  return (
    <div className="td" onKeyDown={(e) => {
      if (["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"].includes(e.key)) e.stopPropagation();
    }}>
      <Header report={report} table={table} findings={findings} />
      <div
        className="td-tabs"
        role="tablist"
        aria-label="Table details"
        onKeyDown={(e) => {
          if (e.key !== "ArrowRight" && e.key !== "ArrowLeft") return;
          e.preventDefault();
          const at = tabs.findIndex((t) => t.key === tab);
          const next = tabs[(at + (e.key === "ArrowRight" ? 1 : tabs.length - 1)) % tabs.length];
          if (!next) return;
          choose(next.key);
          (e.currentTarget.querySelector(`[data-tab="${next.key}"]`) as HTMLElement | null)?.focus();
        }}
      >
        {tabs.map((t) => (
          <button
            key={t.key}
            id={`${tabId}-${t.key}`}
            data-tab={t.key}
            role="tab"
            aria-selected={tab === t.key}
            aria-controls={`${tabId}-panel`}
            tabIndex={tab === t.key ? 0 : -1}
            className={tab === t.key ? "on" : ""}
            title={`${t.label} (${t.hotkey})`}
            onClick={() => choose(t.key)}
          >
            {t.label}
            {t.n != null && <span className="n">{t.n}</span>}
          </button>
        ))}
      </div>
      <div id={`${tabId}-panel`} className={`td-body ${tab === "data" ? "fill" : tab === "columns" || tab === "indexes" ? "grid-tab" : ""}`} role="tabpanel" aria-labelledby={`${tabId}-${tab}`}>
        {tab === "columns" && <Columns report={report} table={table} indexes={indexes} links={links} onOpenTable={onOpenTable} />}
        {tab === "indexes" && <Indexes report={report} table={table} indexes={indexes} links={links} />}
        {tab === "relations" && <Relations links={links} onOpenTable={onOpenTable} />}
        {tab === "data" && <SchemaData key={JSON.stringify([snapshot.connection, engine, table.schema, table.name])} report={report} table={table} />}
        {tab === "ddl" && <Ddl text={ddl(engine, table, columns, indexes, links.outgoing)} original={table.definition != null} />}
      </div>
    </div>
  );
}

function Ddl({ text, original }: { text: string; original: boolean }) {
  return (
    <div className="ddl">
      <div className="data-bar">
        <span className="muted">{original ? "Table definition captured from the database." : "Rebuilt from catalog metadata."} Check it against the database before running it anywhere.</span>
        <span className="spacer" />
        <button className="btn" onClick={() => copyAndTell(text, "Copied the DDL")}>
          <Copy size={12} /> Copy
        </button>
      </div>
      <div className="codeblock">
        <Sql text={text} pretty={false} />
      </div>
    </div>
  );
}
