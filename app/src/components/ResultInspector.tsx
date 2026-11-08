import { ArrowDown, ArrowUp, Braces, Copy, WrapText, X } from "lucide-react";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { ResultSet } from "../gen/ResultSet";
import { copyAndTell } from "../lib/clipboard";
import { useFocusTrap } from "../lib/focus";
import { t } from "../lib/i18n";
import { cellText, columnType, formattedJson, rowTsv, VALUE_PREVIEW_CHARS, valuePreview, type ResultCell } from "../lib/result-values";

export function ResultInspector({ result, cell, types, onSelect, onClose }: {
  result: ResultSet; cell: ResultCell; types?: Record<string, string>;
  onSelect: (cell: ResultCell) => void; onClose: () => void;
}) {
  const [tab, setTab] = useState<"value" | "row">("value");
  const [pretty, setPretty] = useState(true);
  const [wrap, setWrap] = useState(true);
  const root = useRef<HTMLDivElement>(null);
  const close = useRef<HTMLButtonElement>(null);
  const title = useId();
  const value = result.rows[cell.row]?.[cell.col];
  const raw = useMemo(() => cellText(value), [value]);
  const formatted = useMemo(() => formattedJson(raw), [raw]);
  const displayed = pretty && formatted !== null ? formatted : raw;
  const column = result.columns[cell.col] ?? t("ui.result_inspector.value");
  const kind = value == null ? "NULL" : columnType(types, column) ?? (formatted !== null || typeof value === "object" ? "JSON" : typeof value === "string" ? t("ui.result_inspector.kind_text") : typeof value === "number" ? t("ui.result_inspector.kind_number") : t("ui.result_inspector.kind_boolean"));
  useFocusTrap(root, true, onClose);
  useEffect(() => { window.getSelection()?.removeAllRanges(); close.current?.focus(); }, []);
  const copy = () => tab === "row"
    ? copyAndTell(result.columns.join("\t") + "\n" + rowTsv(result, cell.row), t("ui.result_grid.row_copied"))
    : copyAndTell(displayed, formatted !== null && pretty ? t("ui.result_inspector.json_copied") : t("ui.result_grid.value_copied"));

  return createPortal(
    <div className="rg-inspector-backdrop" onMouseDown={(event) => { if (event.target === event.currentTarget) onClose(); }}>
      <div ref={root} className="rg-inspector" tabIndex={-1} role="dialog" aria-modal="true" aria-labelledby={title} onKeyDown={(event) => {
        event.stopPropagation();
        if (event.key === "Escape") { event.preventDefault(); onClose(); }
        if ((event.metaKey || event.ctrlKey) && event.key === "Enter") event.preventDefault();
        if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "c" && !window.getSelection()?.toString()) {
          event.preventDefault(); void copy();
        }
      }}>
        <header className="rg-inspector-head">
          <div className="rg-inspector-heading">
            <span className="eyebrow">{t("ui.result_inspector.title")}</span>
            <h2 id={title} title={column}>{tab === "value" ? column : t("ui.result_inspector.row_heading", { row: cell.row + 1 })}</h2>
            <div className="rg-inspector-meta"><span className="rg-value-kind">{tab === "value" ? kind : t("ui.result_inspector.columns", { count: result.columns.length })}</span><span>{t("ui.result_inspector.row_of", { row: cell.row + 1, total: result.rows.length })}</span></div>
          </div>
          <button type="button" ref={close} className="icon-btn" aria-label={t("ui.result_inspector.close")} title={t("ui.common.close_esc")} onClick={onClose}><X size={17} /></button>
        </header>
        <div className="rg-inspector-toolbar">
          <div className="rg-switch" role="group" aria-label={t("ui.result_inspector.inspect")}>
            <button type="button" className={tab === "value" ? "on" : ""} aria-pressed={tab === "value"} onClick={() => setTab("value")}>{t("ui.result_inspector.tab_value")}</button>
            <button type="button" className={tab === "row" ? "on" : ""} aria-pressed={tab === "row"} onClick={() => setTab("row")}>{t("ui.result_inspector.tab_row")}</button>
          </div>
          <span className="spacer" />
          <button type="button" className="btn ghost" onClick={() => void copy()}><Copy size={13} /> {tab === "row" ? t("ui.result_inspector.copy_row") : t("ui.result_inspector.copy_value")}</button>
        </div>
        {tab === "value" ? <>
          <div className="rg-inspector-column">
            <label htmlFor={title + "-column"}>{t("ui.result_inspector.column")}</label>
            <select id={title + "-column"} value={cell.col} onChange={(event) => onSelect({ row: cell.row, col: Number(event.target.value) })}>
              {result.columns.map((name, col) => <option key={col} value={col}>{col + 1}. {name}</option>)}
            </select>
          </div>
          <div className="rg-value-toolbar">
            {formatted !== null ? <div className="rg-switch" role="group" aria-label={t("ui.result_inspector.format")}>
              <button type="button" className={pretty ? "on" : ""} aria-pressed={pretty} onClick={() => setPretty(true)}><Braces size={12} /> {t("ui.result_inspector.formatted")}</button>
              <button type="button" className={!pretty ? "on" : ""} aria-pressed={!pretty} onClick={() => setPretty(false)}>{t("ui.result_inspector.raw")}</button>
            </div> : <span className="muted">{value == null ? t("ui.result_inspector.sql_null") : t("ui.result_inspector.as_returned")}</span>}
            <button type="button" className={`btn ghost rg-wrap ${wrap ? "on" : ""}`} aria-label={t("ui.result_inspector.wrap_label")} aria-pressed={wrap} onClick={() => setWrap(!wrap)}><WrapText size={14} /> {t("ui.result_inspector.wrap")}</button>
          </div>
          <ValuePreview key={`${cell.row}:${cell.col}:${pretty}`} text={displayed} wrap={wrap} isNull={value == null} />
        </> : <div className="rg-row-fields">
          {result.columns.map((name, col) => <button type="button" key={col} className={`rg-row-field ${col === cell.col ? "on" : ""}`} onClick={() => { onSelect({ row: cell.row, col }); setTab("value"); }}>
            <span className="rg-field-name"><span>{name}</span><small>{columnType(types, name) ?? t("ui.result_inspector.column_n", { n: String(col + 1) })}</small></span>
            <span className={`rg-field-preview ${result.rows[cell.row]?.[col] == null ? "null" : ""}`}>{valuePreview(cellText(result.rows[cell.row]?.[col]), 240)}</span>
          </button>)}
        </div>}
        <footer className="rg-inspector-foot">
          <span className="muted">{tab === "value" ? t("ui.result_inspector.characters", { count: raw.length }) : t("ui.result_inspector.choose_field")}</span>
          <div className="rg-row-navigation">
            <button type="button" className="icon-btn" aria-label={t("ui.result_inspector.previous_row_label")} title={t("ui.result_inspector.previous_row")} disabled={cell.row === 0} onClick={() => onSelect({ ...cell, row: cell.row - 1 })}><ArrowUp size={15} /></button>
            <button type="button" className="icon-btn" aria-label={t("ui.result_inspector.next_row_label")} title={t("ui.result_inspector.next_row")} disabled={cell.row === result.rows.length - 1} onClick={() => onSelect({ ...cell, row: cell.row + 1 })}><ArrowDown size={15} /></button>
          </div>
        </footer>
      </div>
    </div>, document.body,
  );
}

function ValuePreview({ text, wrap, isNull }: { text: string; wrap: boolean; isNull: boolean }) {
  const [limit, setLimit] = useState(VALUE_PREVIEW_CHARS);
  const preview = valuePreview(text, limit);
  return <div className="rg-value-preview">
    {/* biome-ignore lint/a11y/noNoninteractiveTabindex: a scrolling region takes focus so the keyboard can scroll it */}
    <div className="rg-value-scroll" role="region" tabIndex={0} aria-label={t("ui.result_inspector.cell_value")}><pre className={`${wrap ? "wrap" : ""} ${isNull ? "null" : ""}`}>{preview}</pre></div>
    {preview.length < text.length && <div className="rg-preview-more"><span>{t("ui.result_inspector.showing", { shown: preview.length, total: text.length })}</span><button type="button" className="btn ghost" onClick={() => setLimit(Math.min(text.length, limit * 2))}>{t("ui.result_inspector.show_more")}</button></div>}
  </div>;
}
