import { Braces, ChevronDown, Download, FileSpreadsheet } from "lucide-react";
import { useCallback, useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { ResultSet } from "../gen/ResultSet";
import { api, errorMessage } from "../lib/api";
import { t } from "../lib/i18n";
import { resultFile, type ResultFileFormat } from "../lib/result-export";
import { toast } from "../lib/store";

type MenuState = { result: ResultSet; open: boolean; operation: symbol | null };

export function ResultExportMenu({ result }: { result: ResultSet }) {
  const [state, setState] = useState<MenuState>({ result, open: false, operation: null });
  const activeOperation = useRef<symbol | null>(null);
  let current = state;
  if (current.result !== result) { activeOperation.current = null; current = { result, open: false, operation: null }; setState(current); }
  const [position, setPosition] = useState({ top: 0, left: 0 });
  const trigger = useRef<HTMLButtonElement>(null);
  const menu = useRef<HTMLDivElement>(null);
  const id = useId();
  const busy = current.operation !== null;
  const close = useCallback((restore: boolean) => {
    if (restore && menu.current?.contains(document.activeElement)) trigger.current?.focus();
    setState((old) => ({ ...old, open: false }));
  }, []);

  useLayoutEffect(() => {
    if (!current.open || !menu.current || !trigger.current) return;
    const anchor = trigger.current.getBoundingClientRect();
    const bounds = menu.current.getBoundingClientRect();
    setPosition({ left: Math.max(12, Math.min(anchor.right - bounds.width, innerWidth - bounds.width - 12)), top: anchor.bottom + bounds.height + 6 <= innerHeight - 12 ? anchor.bottom + 6 : Math.max(12, anchor.top - bounds.height - 6) });
    menu.current.querySelector<HTMLButtonElement>("button")?.focus();
    const onDown = (event: MouseEvent) => {
      if (!menu.current?.contains(event.target as Node) && !trigger.current?.contains(event.target as Node)) close(false);
    };
    const onResize = () => close(true);
    const onScroll = (event: Event) => { if (!menu.current?.contains(event.target as Node)) close(true); };
    document.addEventListener("mousedown", onDown);
    document.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onResize);
    return () => { document.removeEventListener("mousedown", onDown); document.removeEventListener("scroll", onScroll, true); window.removeEventListener("resize", onResize); };
  }, [current.open, close]);

  const save = async (format: ResultFileFormat) => {
    if (activeOperation.current !== null) return;
    const operation = Symbol();
    activeOperation.current = operation;
    setState({ result, open: true, operation });
    try {
      const file = await resultFile(result, format);
      const where = await (await api()).saveFile(file.name, file.contents);
      const shown = where.split(/[\\/]/).slice(-2).join("/");
      toast(file.protectedCells ? t("ui.result_export_menu.saved_escaped", { file: shown, count: file.protectedCells }) : t("ui.common.saved_file", { file: shown }));
    } catch (error) {
      toast(errorMessage(error).message);
    } finally {
      if (activeOperation.current === operation) {
        activeOperation.current = null;
        close(true);
        setState((old) => old.result === result && old.operation === operation ? { ...old, operation: null } : old);
      }
    }
  };

  return <>
    <button type="button" ref={trigger} className="btn ghost" disabled={!result.columns.length} aria-disabled={busy} aria-haspopup="menu" aria-expanded={current.open} aria-controls={current.open ? id : undefined} onClick={() => { if (!busy) setState({ ...current, open: !current.open }); }} onKeyDown={(event) => {
      if (event.key === "ArrowDown") { event.preventDefault(); event.stopPropagation(); if (!busy) setState({ ...current, open: true }); }
    }}><Download size={13} /> {busy ? t("ui.result_export_menu.exporting") : t("ui.result_export_menu.export")} <ChevronDown size={11} /></button>
    {current.open && createPortal(<div ref={menu} id={id} className="rg-export-menu" role="menu" aria-label={t("ui.result_export_menu.title")} style={position} onKeyDown={(event) => {
      event.stopPropagation();
      if (event.key === "Escape") { event.preventDefault(); close(true); }
      else if (event.key === "Tab") close(true);
      else if ((event.metaKey || event.ctrlKey) && event.key === "Enter") event.preventDefault();
      else if (["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
        event.preventDefault();
        const items = [...menu.current!.querySelectorAll<HTMLButtonElement>("button:not(:disabled)")];
        const at = items.indexOf(document.activeElement as HTMLButtonElement);
        const next = event.key === "Home" ? 0 : event.key === "End" ? items.length - 1 : (at + (event.key === "ArrowDown" ? 1 : -1) + items.length) % items.length;
        items[next]?.focus();
      }
    }}>
      <div className="rg-export-head"><b>{t("ui.result_export_menu.title")}</b><span>{t("ui.result_export_menu.loaded", { rows: result.rows.length, columns: result.columns.length })}</span></div>
      <button type="button" role="menuitem" disabled={busy} onClick={() => void save("csv")}><span className="rg-export-symbol"><FileSpreadsheet size={17} /></span><span><b>{t("ui.result_export_menu.csv")}</b><small>{t("ui.result_export_menu.csv_hint")}</small></span></button>
      <button type="button" role="menuitem" disabled={busy} onClick={() => void save("json")}><span className="rg-export-symbol"><Braces size={17} /></span><span><b>{t("ui.result_export_menu.json")}</b><small>{t("ui.result_export_menu.json_hint")}</small></span></button>
      <div className="rg-export-foot">{result.truncated ? t("ui.result_export_menu.partial") : t("ui.result_export_menu.complete")}<span>{t("ui.result_export_menu.foot")}</span></div>
    </div>, document.body)}
  </>;
}
