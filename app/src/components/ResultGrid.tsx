import { ArrowDown, ArrowUp, ArrowUpDown, ChevronLeft, ChevronRight, Copy, PanelRightOpen } from "lucide-react";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import type { ResultSet } from "../gen/ResultSet";
import type { ColumnSort } from "../gen/ColumnSort";
import { copyAndTell } from "../lib/clipboard";
import { t, useLanguage } from "../lib/i18n";
import { cellText, columnType, resultPageSize, rowTsv, valuePreview, type ResultCell } from "../lib/result-values";
import { ResultInspector } from "./ResultInspector";
import { ResultExportMenu } from "./ResultExportMenu";
import "./result-grid.css";

const NUMERIC = /^(tiny|small|medium|big)?int|^int|^integer|^numeric|^decimal|^real|^float|^double|^money|^smallmoney|^bit\b|^serial|^bigserial/i;
type Selection = { result: ResultSet; cell: ResultCell | null; page: number; inspecting: boolean };

/** A bounded result page. Selection and the inspector belong to this result, never to a later query's rows. */
export function ResultGrid({ result, types, summary, order, onSort, sortDisabled = false }: {
  result: ResultSet; types?: Record<string, string>; summary?: string;
  order?: ColumnSort | null; onSort?: (column: string) => void; sortDisabled?: boolean;
}) {
  const [selection, setSelection] = useState<Selection>({ result, cell: null, page: 0, inspecting: false });
  let current = selection;
  if (current.result !== result) {
    // Reset at the transition, including when a cached result returns after another query tab.
    current = { result, cell: null, page: 0, inspecting: false };
    setSelection(current);
  }
  const selected = current.cell;
  const grid = useRef<HTMLTableElement>(null);
  const scroll = useRef<HTMLDivElement>(null);
  const id = useId();
  const pageSize = resultPageSize(result.columns.length);
  const start = current.page * pageSize;
  const end = Math.min(start + pageSize, result.rows.length);
  const numeric = useMemo(() => result.columns.map((column, col) => NUMERIC.test(columnType(types, column) ?? "") ||
    (result.rows.some((row) => row[col] != null) && result.rows.every((row) => row[col] == null || typeof row[col] === "number"))), [result, types]);
  // The titles of cells cut short are worded in the app's language.
  const language = useLanguage();
  const rows = useMemo(() => result.rows.slice(start, end).map((row) => result.columns.map((_, col) => {
    const value = row[col];
    const text = cellText(value);
    const title = text.length > 1024 ? t("ui.result_grid.cell_cut", { preview: valuePreview(text, 1024) }) : valuePreview(text, 1024);
    return { text: valuePreview(text, 320), title: text.length > 40 ? title : undefined, isNull: value == null };
  })), [result, start, end, language]);
  const select = (cell: ResultCell, inspecting = current.inspecting) => setSelection({ result, cell, page: Math.floor(cell.row / pageSize), inspecting });
  const inspect = () => { if (result.rows.length && result.columns.length) select(selected ?? { row: start, col: 0 }, true); };
  const header = result.columns.join("\t");
  const copyValue = () => selected && copyAndTell(cellText(result.rows[selected.row]?.[selected.col]), t("ui.result_grid.value_copied"));
  const changePage = (page: number) => setSelection({ result, page, inspecting: false, cell: selected ? { row: page * pageSize, col: selected.col } : null });
  const selectedRow = selected?.row;
  const selectedCol = selected?.col;
  useEffect(() => {
    const cell = selectedRow != null && selectedCol != null ? document.getElementById(`${id}-${selectedRow}-${selectedCol}`) : null;
    const area = scroll.current;
    if (!cell || !area) return;
    const bounds = area.getBoundingClientRect();
    const rect = cell.getBoundingClientRect();
    const top = bounds.top + (grid.current?.tHead?.getBoundingClientRect().height ?? 0);
    const left = bounds.left + (grid.current?.querySelector(".rg-n")?.getBoundingClientRect().width ?? 0);
    if (rect.top < top) area.scrollTop += rect.top - top;
    else if (rect.bottom > bounds.bottom) area.scrollTop += rect.bottom - bounds.bottom;
    if (rect.left < left || rect.width > bounds.right - left) area.scrollLeft += rect.left - left;
    else if (rect.right > bounds.right) area.scrollLeft += rect.right - bounds.right;
  }, [id, selectedRow, selectedCol, result]);

  return <div className="result-grid">
    <div className="rg-bar">
      <span className="muted rg-summary" title={summary}>{summary ?? (result.truncated ? t("ui.result_grid.rows_more", { count: result.rows.length }) : t("ui.result_grid.rows", { count: result.rows.length }))}</span>
      <div className="rg-actions">
        <button type="button" className="btn ghost" disabled={!result.rows.length || !result.columns.length} onClick={inspect} title={t("ui.result_grid.inspect_title")}><PanelRightOpen size={13} /> {t("ui.result_grid.inspect")}</button>
        {selected && <>
          <button type="button" className="btn ghost" onClick={() => void copyValue()}><Copy size={12} /> {t("ui.result_grid.copy_value")}</button>
          <button type="button" className="btn ghost" onClick={() => void copyAndTell(header + "\n" + rowTsv(result, selected.row), t("ui.result_grid.row_copied"))}><Copy size={12} /> {t("ui.result_grid.copy_row")}</button>
        </>}
        <button type="button" className="btn ghost" onClick={() => void copyAndTell([header, ...result.rows.map((_, row) => rowTsv(result, row))].join("\n"), t("ui.result_grid.all_copied", { count: result.rows.length }))}><Copy size={12} /> {t("ui.result_grid.copy_all")}</button>
        <ResultExportMenu result={result} />
      </div>
    </div>
    <div ref={scroll} className="rg-scroll" onKeyDown={(event) => {
      if (event.target instanceof HTMLElement && event.target.closest("button, input, select, textarea, a")) return;
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "c" && selected && !window.getSelection()?.toString()) {
        event.preventDefault(); event.stopPropagation(); void copyValue(); return;
      }
      if (event.key === "Enter") { event.preventDefault(); event.stopPropagation(); inspect(); return; }
      const moves: Record<string, [number, number]> = { ArrowDown: [1, 0], ArrowUp: [-1, 0], ArrowLeft: [0, -1], ArrowRight: [0, 1] };
      const move = moves[event.key];
      if (!move || !result.rows.length || !result.columns.length || event.metaKey || event.ctrlKey || event.altKey) return;
      event.preventDefault(); event.stopPropagation();
      const cell = selected ?? { row: start, col: 0 };
      select({ row: Math.min(result.rows.length - 1, Math.max(0, cell.row + move[0])), col: Math.min(result.columns.length - 1, Math.max(0, cell.col + move[1])) });
    }}>
      <table
        dir="ltr"
        ref={grid}
        className="rg"
        // biome-ignore lint/a11y/noNoninteractiveElementToInteractiveRole: ARIA in HTML allows role="grid" on a table; arrow keys move the selected cell
        role="grid"
        tabIndex={0} aria-label={t("ui.result_grid.label")} aria-rowcount={result.rows.length + 1} aria-colcount={result.columns.length + 1} aria-activedescendant={selected ? `${id}-${selected.row}-${selected.col}` : undefined} onFocus={(event) => {
        if (event.target !== event.currentTarget) return;
        if (result.rows.length && result.columns.length) setSelection((previous) => previous.result === result && previous.cell ? previous : { result, page: current.page, inspecting: false, cell: { row: start, col: 0 } });
      }}>
        <thead><tr aria-rowindex={1}><th className="rg-n">#</th>{result.columns.map((column, col) => {
          const direction = order?.column === column ? order.direction : null;
          const label = <><span className="rg-name">{column}</span>{columnType(types, column) && <span className="rg-type">{columnType(types, column)}</span>}</>;
          return <th key={col} className={numeric[col] ? "r" : ""} title={columnType(types, column) ? `${column} · ${columnType(types, column)}` : column}
            aria-sort={onSort ? direction === "asc" ? "ascending" : direction === "desc" ? "descending" : "none" : undefined}>
            {onSort ? <button type="button" className={`rg-sort ${direction ? "on" : ""}`} aria-disabled={sortDisabled} onClick={() => { if (!sortDisabled) onSort(column); }}
              title={direction === "desc" ? t("ui.result_grid.sort_clear") : direction === "asc" ? t("ui.result_grid.sort_descending") : t("ui.result_grid.sort_ascending")} aria-label={t("ui.result_grid.sort_by", { column })}>
              <span className="rg-sort-label">{label}</span>{direction === "asc" ? <ArrowUp size={13} /> : direction === "desc" ? <ArrowDown size={13} /> : <ArrowUpDown size={13} />}
            </button> : label}
          </th>;
        })}</tr></thead>
        <tbody>{rows.map((row, index) => {
          const r = start + index;
          return (
            <tr key={r} aria-rowindex={r + 2} className={selected?.row === r ? "on" : ""}>
              {/* biome-ignore lint/a11y/useFocusableInteractive: the grid takes focus; its cells are named by aria-activedescendant */}
              <td
                className="rg-n"
                // biome-ignore lint/a11y/noNoninteractiveElementToInteractiveRole: ARIA in HTML lets a grid's td be its row header; a th would stick to the top with the column headers
                role="rowheader"
              >
                {r + 1}
              </td>
              {row.map((value, col) => (
                // biome-ignore lint/a11y/useFocusableInteractive lint/a11y/useKeyWithClickEvents: the grid takes focus and its arrow keys move this selection; a click is the pointer's way to it
                <td
                  key={col}
                  id={`${id}-${r}-${col}`}
                  // biome-ignore lint/a11y/noNoninteractiveElementToInteractiveRole: ARIA in HTML's pattern for an interactive data grid's cells
                  role="gridcell"
                  aria-selected={selected?.row === r && selected.col === col}
                  className={`${numeric[col] ? "r" : ""} ${value.isNull ? "null" : ""} ${selected?.row === r && selected.col === col ? "sel" : ""}`}
                  title={value.title}
                  onClick={() => { select({ row: r, col }); grid.current?.focus({ preventScroll: true }); }}
                  onDoubleClick={() => select({ row: r, col }, true)}
                >
                  {value.text}
                </td>
              ))}
            </tr>
          );
        })}</tbody>
      </table>
      {!result.rows.length && <div className="rg-empty">{t("ui.result_grid.empty")}</div>}
    </div>
    <footer className="rg-footer">
      <span className="muted">{result.rows.length ? t("ui.result_grid.range", { first: start + 1, last: end, count: result.rows.length }) : t("ui.result_grid.zero_rows")}{result.truncated && ` · ${t("ui.result_grid.partial")}`}</span>
      <span className="rg-hint">{t("ui.result_grid.hint")}</span>
      {result.rows.length > pageSize && <div className="rg-pages"><button type="button" className="icon-btn" aria-label={t("ui.result_grid.previous_page")} disabled={current.page === 0} onClick={() => changePage(current.page - 1)}><ChevronLeft size={14} /></button><span>{current.page + 1} / {Math.ceil(result.rows.length / pageSize)}</span><button type="button" className="icon-btn" aria-label={t("ui.result_grid.next_page")} disabled={end === result.rows.length} onClick={() => changePage(current.page + 1)}><ChevronRight size={14} /></button></div>}
    </footer>
    {current.inspecting && selected && <ResultInspector result={result} cell={selected} types={types} onSelect={(cell) => select(cell, true)} onClose={() => setSelection({ ...current, inspecting: false })} />}
  </div>;
}
