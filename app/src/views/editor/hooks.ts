import type { EditorView } from "@codemirror/view";
import { useEffect, useRef, useState, type PointerEvent as ReactPointerEvent, type RefObject } from "react";
import type { Finding } from "../../gen/Finding";
import { api } from "../../lib/api";
import { t } from "../../lib/i18n";
import { keys } from "../../lib/keys";
import { remember, rememberedAs } from "../../lib/prefs";
import { setLintEffect } from "./codemirror";

/** The text a new tab starts with, in the app's language: never run, saved or linted. */
export function placeholder(): string {
  return `${t("ui.editor.placeholder", { run: keys("⌘↵"), explain: keys("⌘E"), save: keys("⌘S") })}\n`;
}

/** SQL that could hold DDL; anything else skips the migration linter altogether. */
const HAS_DDL = /\b(create|alter|drop|truncate|rename|reindex|vacuum|cluster)\b/i;

/**
 * The migration linter's findings for the visible tab, shown as marks in the editor too. It runs 200 ms after typing
 * stops, only on SQL with DDL keywords, and clears when the tab or connection changes.
 */
export function useLiveLint(connection: string, tab: { id: string; sql: string }, view: RefObject<EditorView | null>): Finding[] {
  const [findings, setFindings] = useState<Finding[]>([]);
  const context = useRef("");

  useEffect(() => {
    const show = (next: Finding[]) => {
      setFindings((prev) => (next.length === 0 && prev.length === 0 ? prev : next));
      view.current?.dispatch({ effects: setLintEffect.of(next) });
    };
    const key = `${connection}:${tab.id}`;
    if (context.current !== key) {
      context.current = key;
      show([]);
    }
    const sql = tab.sql;
    if (!sql || sql === placeholder().trim() || !HAS_DDL.test(sql)) {
      show([]);
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(() => {
      void api()
        .then((client) => client.lintSql(connection, sql))
        .then((next) => !cancelled && show(next))
        .catch(() => !cancelled && show([]));
    }, 200);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [tab.id, tab.sql, connection, view]);

  return findings;
}

const SPLIT_KEY = "culprit.editor.split";

/** The editor's share of the height above its output (remembered), the box it divides, and the splitter's drag. */
export function useSplit(): { split: number; body: RefObject<HTMLDivElement | null>; drag: (event: ReactPointerEvent) => void } {
  const [split, setSplit] = useState(() => rememberedAs(SPLIT_KEY, (v) => Math.min(0.8, Math.max(0.2, Number(v) || 0.45)), 0.45));
  const body = useRef<HTMLDivElement>(null);
  const drag = (event: ReactPointerEvent) => {
    const box = body.current?.getBoundingClientRect();
    if (!box) return;
    event.preventDefault();
    let latest = split;
    const move = (e: PointerEvent) => {
      latest = Math.min(0.8, Math.max(0.2, (e.clientY - box.top) / box.height));
      setSplit(latest);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      remember(SPLIT_KEY, String(latest));
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };
  return { split, body, drag };
}
