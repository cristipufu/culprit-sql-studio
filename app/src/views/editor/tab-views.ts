import { Compartment, EditorState, Prec, type Extension } from "@codemirror/state";
import { EditorView, basicSetup } from "codemirror";
import { useCallback, useEffect, useEffectEvent, useRef } from "react";
import type { EditorTab as Tab } from "../../lib/editor-tabs";
import { lintField, sqlColors } from "./codemirror";

/**
 * The editor's CodeMirror view and each tab's editing state (its text, selection and undo history). One view, created
 * when the editor mounts and destroyed when it leaves (or rebuilt when `rebuild` changes), shows one tab's state at a
 * time; the others are kept for when their tab comes back, until it closes or the SQL language changes.
 */
export function useTabViews({
  language,
  keys,
  onEdit,
  initial,
  rebuild,
}: {
  /** The SQL language every state is built with: dialect, keywords and the completion catalog. */
  language: () => Extension;
  /** The editor's own key bindings, above CodeMirror's (which bind Mod-Enter to "insert blank line"). */
  keys: Extension;
  /** The visible tab's text changed. */
  onEdit: (text: string) => void;
  /** The tab the view shows when it's created. */
  initial: () => Tab;
  /** What the view is rebuilt on: the engine, whose dialect it is. */
  rebuild: unknown;
}) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const kept = useRef(new Map<string, EditorState>());
  const compartment = useRef(new Compartment());
  // The tab whose state the view shows; null once it closed.
  const shown = useRef<string | null>(null);
  const edited = useRef(onEdit);
  edited.current = onEdit;

  const fresh = useCallback(
    (tab: Tab) =>
      EditorState.create({
        doc: tab.sql,
        extensions: [
          basicSetup,
          Prec.high(sqlColors),
          lintField,
          compartment.current.of(language()),
          Prec.highest(keys),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) edited.current(update.state.doc.toString());
          }),
        ],
      }),
    [language, keys],
  );

  /** Show `tab`, keeping the state of the tab shown until now for when it comes back. */
  const show = useCallback(
    (tab: Tab, focus = true) => {
      const editor = view.current;
      if (!editor) return;
      if (shown.current !== null) kept.current.set(shown.current, editor.state);
      editor.setState(kept.current.get(tab.id) ?? fresh(tab));
      shown.current = tab.id;
      if (focus) editor.focus();
    },
    [fresh],
  );

  /** A closed tab's state is gone; the view keeps showing it until another tab is shown. */
  const forget = useCallback((id: string) => {
    kept.current.delete(id);
    if (shown.current === id) shown.current = null;
  }, []);

  const firstState = useEffectEvent(() => {
    const tab = initial();
    shown.current = tab.id;
    return fresh(tab);
  });
  useEffect(() => {
    if (!host.current) return;
    const editor = new EditorView({ parent: host.current, state: firstState() });
    view.current = editor;
    editor.focus();
    return () => {
      kept.current.clear();
      editor.destroy();
      view.current = null;
    };
  }, [rebuild]);

  // New table and column names: the visible state takes them now, and the kept ones are rebuilt when their tab returns.
  useEffect(() => {
    const editor = view.current;
    if (!editor) return;
    editor.dispatch({ effects: compartment.current.reconfigure(language()) });
    for (const id of [...kept.current.keys()]) if (id !== shown.current) kept.current.delete(id);
  }, [language]);

  const focus = useCallback(() => view.current?.focus(), []);
  /** The visible tab's text. */
  const text = useCallback(() => view.current?.state.doc.toString() ?? "", []);
  /** The text to act on: the selection when there is one, otherwise the whole tab. */
  const target = useCallback(() => {
    const state = view.current?.state;
    if (!state) return "";
    const selected = state.selection.ranges.map((r) => state.sliceDoc(r.from, r.to)).join("\n").trim();
    return selected || state.doc.toString().trim();
  }, []);
  /** A tab's text as edited: the visible one's from the view, another's from its kept state, else as stored. */
  const textOf = useCallback(
    (tab: Tab) =>
      tab.id === shown.current && view.current ? view.current.state.doc.toString() : (kept.current.get(tab.id)?.doc.toString() ?? tab.sql),
    [],
  );

  return { host, view, show, forget, focus, text, target, textOf };
}
