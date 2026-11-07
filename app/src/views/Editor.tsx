import { Compartment, EditorState, Prec, type Extension } from "@codemirror/state";
import { keymap } from "@codemirror/view";
import { EditorView, basicSetup } from "codemirror";
import { TriangleAlert } from "lucide-react";
import { useCallback, useEffect, useEffectEvent, useMemo, useRef, useState } from "react";
import { ConfirmRun, type PendingRun } from "../components/ConfirmRun";
import { rememberLook, rememberedLook, type PlanLook } from "../components/PlanSurface";
import { SavedRail } from "../components/SavedRail";
import type { Catalog } from "../gen/Catalog";
import type { ConnectionInfo } from "../gen/ConnectionInfo";
import type { ExplainMode } from "../gen/ExplainMode";
import type { Finding } from "../gen/Finding";
import type { SavedQuery } from "../gen/SavedQuery";
import { api, errorMessage } from "../lib/api";
import { catalogFromReport } from "../lib/completion";
import { splitSqlStatements } from "../lib/editor-sql";
import { EditorTabStorage, MAX_EDITOR_TABS, newEditorTab, tabTitle, type EditorTab as Tab, type EditorTabs as Tabs } from "../lib/editor-tabs";
import { measuresRuns } from "../lib/engines";
import { allLintFixes, lintFix } from "../lib/lint-fixes";
import { remember, rememberedChoice } from "../lib/prefs";
import { currentReport, getState, loadSaved, setState, toast, useStore } from "../lib/store";
import { lintField, sqlColors, sqlLanguage } from "./editor/codemirror";
import { PLACEHOLDER, useLiveLint, useSplit } from "./editor/hooks";
import { LintPanel } from "./editor/LintPanel";
import { EmptyOutput, OutputPane, type Output, type StatementResult } from "./editor/OutputPane";
import { TabStrip } from "./editor/TabStrip";
import { Toolbar } from "./editor/Toolbar";

const RAIL_KEY = "culprit.editor.rail";
const ROW_LIMIT = 1000;

/** Catalogs fetched this session, per connection. */
const catalogs = new Map<string, Catalog>();

export default function Editor({ connection }: { connection: ConnectionInfo }) {
  const host = useRef<HTMLDivElement>(null);
  const view = useRef<EditorView | null>(null);
  const states = useRef(new Map<string, EditorState>());
  const language = useRef(new Compartment());
  const [tabStorage] = useState(() => new EditorTabStorage(connection, PLACEHOLDER));
  const [tabs, setTabs] = useState<Tabs>(tabStorage.initial);
  const [persistenceError, setPersistenceError] = useState(tabStorage.error);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    setPersistenceError(tabStorage.migrate());
    return () => { mounted.current = false; };
  }, [tabStorage]);
  const tabsRef = useRef(tabs);
  tabsRef.current = tabs;
  const [outputs, setOutputs] = useState<Record<string, Output>>({});
  const [pending, setPending] = useState<PendingRun | null>(null);
  const [naming, setNaming] = useState<string | null>(null);
  const [rail, setRail] = useState(() => rememberedChoice(RAIL_KEY, "open", ["open", "closed"] as const));
  const [look, setLook] = useState<PlanLook>(rememberedLook);
  const [catalog, setCatalog] = useState<Catalog | null>(() => catalogs.get(connection.name) ?? null);
  const [catalogBusy, setCatalogBusy] = useState(false);
  const report = useStore(currentReport);
  const saved = useStore((s) => s.saved);
  const openSaved = useStore((s) => s.editorSaved);
  const incoming = useStore((s) => s.editorSql);
  const production = connection.environment === "production";
  // Loading and closing tabs always leave one tab.
  const activeTab = tabs.tabs.find((t) => t.id === tabs.active) ?? tabs.tabs[0]!;
  const output = outputs[activeTab.id] ?? { kind: "none" };

  const fallbackCatalog = useMemo(() => (report?.snapshot.connection === connection.name ? catalogFromReport(report) : null), [report, connection.name]);
  const effectiveCatalog = catalog ?? fallbackCatalog;

  const languageExtension = useCallback(
    (): Extension => sqlLanguage(connection.engine, connection.last_report?.product, effectiveCatalog, report?.snapshot.connection === connection.name ? report : null),
    [connection.engine, connection.last_report?.product, effectiveCatalog, report, connection.name],
  );

  // ── tab bookkeeping ────────────────────────────────────────────────────────────────────────────
  const updateTabs = useCallback((next: (t: Tabs) => Tabs) => {
    if (!mounted.current || getState().active !== connection.name) return;
    const value = next(tabsRef.current);
    tabsRef.current = value;
    setTabs(value);
    setPersistenceError(tabStorage.write(value));
  }, [tabStorage, connection.name]);

  const currentText = () => view.current?.state.doc.toString() ?? "";

  /** The text to act on: the selection when there is one, otherwise the whole tab. */
  const target = () => {
    const state = view.current?.state;
    if (!state) return "";
    const selected = state.selection.ranges.map((r) => state.sliceDoc(r.from, r.to)).join("\n").trim();
    return selected || state.doc.toString().trim();
  };

  const setOutput = (id: string, value: Output) => {
    if (!mounted.current || !tabsRef.current.tabs.some((t) => t.id === id)) return;
    setOutputs((o) => ({ ...o, [id]: value }));
  };

  const stateFor = useCallback(
    (tab: Tab, actions: Extension) =>
      EditorState.create({
        doc: tab.sql,
        extensions: [
          basicSetup,
          Prec.high(sqlColors),
          lintField,
          language.current.of(languageExtension()),
          // Above CodeMirror's defaults, which bind Mod-Enter to "insert blank line".
          Prec.highest(actions),
          EditorView.updateListener.of((update) => {
            if (!update.docChanged) return;
            const text = update.state.doc.toString();
            const id = tabsRef.current.active;
            updateTabs((t) => ({ ...t, tabs: t.tabs.map((x) => (x.id === id ? { ...x, sql: text } : x)) }));
          }),
        ],
      }),
    [languageExtension, updateTabs],
  );

  const liveLint = useLiveLint(connection.name, activeTab, view);

  const applyLintFix = (f: Finding) => {
    const editor = view.current;
    const fix = editor && lintFix(editor.state.doc.toString(), f);
    if (!editor || !fix) return;
    editor.dispatch({ changes: fix.change });
    toast(fix.message);
  };

  const applyAllLintFixes = () => {
    const editor = view.current;
    if (!editor) return;
    const { changes, applied } = allLintFixes(editor.state.doc.toString(), liveLint);
    if (applied === 0) return;
    editor.dispatch({ changes });
    toast(`Applied ${applied} safe DDL fixes`);
  };

  // ── actions ────────────────────────────────────────────────────────────────────────────────────
  const run = async (sql: string, confirmed = false) => {
    if (!sql || sql === PLACEHOLDER.trim()) return;
    const batch = splitSqlStatements(sql);
    if (batch.length === 0) {
      return toast("No runnable SQL statement — uncomment a statement or fill in placeholders first");
    }
    const tab = tabsRef.current.active;
    const client = await api();
    if (production && !confirmed) {
      const checks = await Promise.all(batch.map((stmt) => client.checkStatement(connection.engine, stmt)));
      const nonReads = checks.filter((c) => !c.reads_only && !c.problem);
      if (nonReads.length > 0) {
        const warning = nonReads.find((c) => c.warning)?.warning ?? null;
        const check = { ...nonReads[0]!, warning };
        const lint = await client.lintSql(connection.name, sql).catch(() => []);
        return setPending({ kind: "run", sql, check, lint });
      }
    }
    setOutput(tab, { kind: "busy", label: batch.length > 1 ? `Running 1 of ${batch.length}…` : "Running…" });
    let currentIdx = 0;
    try {
      const results: StatementResult[] = [];
      for (let i = 0; i < batch.length; i++) {
        currentIdx = i;
        const stmt = batch[i]!;
        if (batch.length > 1 && i > 0) {
          setOutput(tab, { kind: "busy", label: `Running ${i + 1} of ${batch.length}…` });
        }
        const result = await client.run(connection.name, stmt, ROW_LIMIT, confirmed);
        results.push({ result, sql: stmt });
      }
      setOutput(tab, { kind: "rows", results });
      const name = tabsRef.current.tabs.find((t) => t.id === tab)?.saved;
      if (name) void client.savedQueryRan(name).then(() => loadSaved());
    } catch (e) {
      const error = errorMessage(e);
      if (error.code === "confirmation_required") {
        const checks = await Promise.all(batch.map((stmt) => client.checkStatement(connection.engine, stmt)));
        const nonReads = checks.filter((c) => !c.reads_only && !c.problem);
        const warning = nonReads.find((c) => c.warning)?.warning ?? null;
        const baseCheck = nonReads[0] ?? checks[0]!;
        const check = { ...baseCheck, warning: warning ?? baseCheck.warning };
        const lint = await client.lintSql(connection.name, sql).catch(() => []);
        setOutput(tab, { kind: "none" });
        return setPending({ kind: "run", sql, check, lint });
      }
      if (batch.length > 1) {
        error.message = `Statement ${currentIdx + 1} of ${batch.length}: ${error.message}`;
      }
      setOutput(tab, { kind: "error", error });
    }
  };

  const explain = async (mode: ExplainMode) => {
    const raw = target();
    if (!raw || raw === PLACEHOLDER.trim() || (mode === "analyze" && !measuresRuns(connection.engine))) return;
    const statements = splitSqlStatements(raw);
    if (statements.length === 0) {
      return toast("No runnable SQL statement — uncomment a statement or fill in placeholders first");
    }
    const sql = statements[statements.length - 1]!;
    const tab = tabsRef.current.active;
    setOutput(tab, { kind: "busy", label: mode === "analyze" ? "Running and measuring…" : "Planning…" });
    try {
      // No consent travels with a plan request: Culprit only measures provable reads on production, and the engine
      // enforces that whatever the UI sends.
      const plan = await (await api()).explain(connection.name, sql, mode, false);
      setOutput(tab, { kind: "plan", plan });
    } catch (e) {
      setOutput(tab, { kind: "error", error: errorMessage(e) });
    }
  };

  const save = async (name?: string) => {
    const tab = tabsRef.current.tabs.find((t) => t.id === tabsRef.current.active);
    if (!tab) return;
    const text = currentText().trim();
    if (!text || text === PLACEHOLDER.trim()) return toast("Write a query before saving it");
    const existing = getState().saved?.find((q) => q.name === tab.saved);
    if (!tab.saved && !name) return setNaming("");
    try {
      const stored = await (await api()).saveQuery({
        name: name ?? tab.saved!,
        sql: text,
        connection: existing?.connection ?? connection.name,
        description: existing?.description ?? null,
        previous_name: tab.saved,
      });
      updateTabs((t) => ({ ...t, tabs: t.tabs.map((x) => (x.id === tab.id ? { ...x, saved: stored.name, title: stored.name } : x)) }));
      setNaming(null);
      await loadSaved();
      toast(`Saved “${stored.name}”`);
    } catch (e) {
      toast(errorMessage(e).message);
    }
  };

  const activate = useCallback(
    (id: string, added?: Tab) => {
      const editor = view.current;
      const current = tabsRef.current.active;
      if (!added && !tabsRef.current.tabs.some((t) => t.id === id)) return;
      if (editor && tabsRef.current.tabs.some((t) => t.id === current)) states.current.set(current, editor.state);
      updateTabs((t) => ({ tabs: added ? [...t.tabs, added] : t.tabs, active: id }));
      const tab = tabsRef.current.tabs.find((t) => t.id === id);
      if (editor && tab) {
        editor.setState(states.current.get(id) ?? stateFor(tab, actionKeys.current));
        editor.focus();
      }
    },
    [stateFor, updateTabs],
  );

  const newTab = useCallback(
    (sql = "", title?: string, savedName: string | null = null) => {
      if (tabsRef.current.tabs.length >= MAX_EDITOR_TABS) {
        toast(`Close a tab before opening another (${MAX_EDITOR_TABS} tabs open)`);
        return null;
      }
      const tab = newEditorTab(sql, title ?? tabTitle(sql, tabsRef.current.tabs.length + 1), savedName);
      const { id } = tab;
      activate(id, tab);
      return id;
    },
    [activate],
  );

  const closeTab = useCallback(
    (id: string) => {
      const list = tabsRef.current.tabs;
      if (!list.some((t) => t.id === id)) return;
      if (list.length === 1) {
        states.current.delete(id);
        setOutputs({});
        const fresh = newEditorTab();
        updateTabs(() => ({ tabs: [fresh], active: fresh.id }));
        view.current?.setState(stateFor(fresh, actionKeys.current));
        return;
      }
      const wasActive = tabsRef.current.active === id;
      const index = list.findIndex((t) => t.id === id);
      const rest = list.filter((t) => t.id !== id);
      states.current.delete(id);
      setOutputs((o) => {
        const { [id]: _, ...others } = o;
        return others;
      });
      const next = tabsRef.current.active === id ? rest[Math.max(0, index - 1)]!.id : tabsRef.current.active;
      updateTabs(() => ({ tabs: rest, active: next }));
      if (view.current && wasActive) {
        const tab = rest.find((t) => t.id === next)!;
        view.current.setState(states.current.get(next) ?? stateFor(tab, actionKeys.current));
        view.current.focus();
      }
    },
    [stateFor, updateTabs],
  );

  const cycle = (delta: number) => {
    const list = tabsRef.current.tabs;
    const index = list.findIndex((t) => t.id === tabsRef.current.active);
    activate(list[(index + delta + list.length) % list.length]!.id);
  };

  const actions = useRef({ run, explain, save, newTab, closeTab, cycle, target });
  actions.current = { run, explain, save, newTab, closeTab, cycle, target };

  const openQuery = useCallback(
    (query: SavedQuery, thenRun: boolean) => {
      const open = tabsRef.current.tabs.find((t) => t.saved === query.name);
      if (open) activate(open.id);
      else if (newTab(query.sql, query.name, query.name) === null) return;
      // The latest run, against the connection as it is now.
      if (thenRun) void actions.current.run(query.sql);
    },
    [activate, newTab],
  );
  const actionKeys = useRef<Extension>(
    keymap.of([
      { key: "Mod-Enter", preventDefault: true, run: () => (void actions.current.run(actions.current.target()), true) },
      { key: "Mod-e", preventDefault: true, run: () => (void actions.current.explain("estimated"), true) },
      { key: "Mod-Shift-e", preventDefault: true, run: () => (void actions.current.explain("analyze"), true) },
      { key: "Mod-s", preventDefault: true, run: () => (void actions.current.save(), true) },
      { key: "Mod-t", preventDefault: true, run: () => (actions.current.newTab(), true) },
      { key: "Mod-Shift-w", preventDefault: true, run: () => (actions.current.closeTab(tabsRef.current.active), true) },
      { key: "Mod-Shift-]", preventDefault: true, run: () => (actions.current.cycle(1), true) },
      { key: "Mod-Shift-[", preventDefault: true, run: () => (actions.current.cycle(-1), true) },
    ]),
  );

  // ── lifecycle ──────────────────────────────────────────────────────────────────────────────────
  // The connection owns this view; tab switches swap states, so a new state function doesn't rebuild it.
  const initialState = useEffectEvent((tab: Tab) => stateFor(tab, actionKeys.current));
  useEffect(() => {
    if (!host.current) return;
    const current = tabsRef.current;
    const tab = current.tabs.find((tab) => tab.id === current.active) ?? current.tabs[0]!;
    const editor = new EditorView({ parent: host.current, state: initialState(tab) });
    view.current = editor;
    editor.focus();
    return () => {
      states.current.clear();
      editor.destroy();
      view.current = null;
    };
  }, [connection.engine]);

  // New table/column names: reconfigure the visible tab and drop cached states so other tabs pick them up on switch.
  useEffect(() => {
    const editor = view.current;
    if (!editor) return;
    editor.dispatch({ effects: language.current.reconfigure(languageExtension()) });
    for (const id of [...states.current.keys()]) if (id !== tabsRef.current.active) states.current.delete(id);
  }, [languageExtension]);

  const refreshCatalog = useCallback(async () => {
    setCatalogBusy(true);
    try {
      const fresh = await (await api()).catalog(connection.name);
      catalogs.set(connection.name, fresh);
      setCatalog(fresh);
    } catch {
      // Completion falls back to the latest report's tables; nothing to tell the person here.
    } finally {
      setCatalogBusy(false);
    }
  }, [connection.name]);

  useEffect(() => {
    setCatalog(catalogs.get(connection.name) ?? null);
    if (!catalogs.has(connection.name)) void refreshCatalog();
  }, [connection.name, refreshCatalog]);

  useEffect(() => {
    if (saved === null) void loadSaved();
  }, [saved]);

  // "Open in editor" from findings and queries: a new tab with that SQL, or the tab that already holds it.
  useEffect(() => {
    if (!incoming || getState().editorSql !== incoming || !view.current) return;
    setState({ editorSql: null });
    const wanted = incoming.trim();
    const textOf = (tab: Tab) =>
      tab.id === tabsRef.current.active && view.current ? view.current.state.doc.toString() : (states.current.get(tab.id)?.doc.toString() ?? tab.sql);
    const open = wanted ? tabsRef.current.tabs.find((t) => textOf(t).trim() === wanted) : undefined;
    if (open) activate(open.id);
    else newTab(incoming);
  }, [incoming, newTab, activate]);

  // A saved query picked in the command palette.
  useEffect(() => {
    if (!openSaved || getState().editorSaved !== openSaved || saved === null) return;
    const query = saved.find((q) => q.name === openSaved.name);
    setState({ editorSaved: null });
    if (query) openQuery(query, openSaved.run);
  }, [openSaved, saved, openQuery]);

  const { split, body, drag } = useSplit();

  const savedQuery = saved?.find((q) => q.name === activeTab.saved);
  const dirty = (tab: Tab) => {
    const stored = saved?.find((q) => q.name === tab.saved);
    return stored ? stored.sql.trim() !== tab.sql.trim() : false;
  };

  return (
    <div className="view editor-view">
      <TabStrip
        tabs={tabs.tabs}
        activeId={activeTab.id}
        dirty={dirty}
        production={production}
        railOpen={rail === "open"}
        onToggleRail={() => {
          const next = rail === "open" ? "closed" : "open";
          setRail(next);
          remember(RAIL_KEY, next);
        }}
        onActivate={activate}
        onClose={closeTab}
        onNew={() => newTab()}
      />
      {persistenceError && <div className="editor-storage-error" role="alert">
        <TriangleAlert size={15} /><span>{persistenceError}</span>
        <button className="btn" onClick={() => save()}>Save query</button>
      </div>}
      <div className="editor-main">
        {rail === "open" && <SavedRail current={connection} saved={saved} active={activeTab.saved} onOpen={openQuery} onRenamed={(from, to) => updateTabs((t) => ({ ...t, tabs: t.tabs.map((x) => (x.saved === from ? { ...x, saved: to, title: to } : x)) }))} onDeleted={(name) => updateTabs((t) => ({ ...t, tabs: t.tabs.map((x) => (x.saved === name ? { ...x, saved: null } : x)) }))} />}
        <div className="editor-work">
          <Toolbar
            engine={connection.engine}
            production={production}
            savedName={activeTab.saved}
            naming={naming}
            onNaming={setNaming}
            onRun={() => run(target())}
            onExplain={explain}
            onSave={save}
            onCancelNaming={() => view.current?.focus()}
            catalogBusy={catalogBusy}
            tables={effectiveCatalog ? effectiveCatalog.tables.length : null}
            onRefreshCatalog={refreshCatalog}
          />
          {savedQuery?.connection && savedQuery.connection !== connection.name && (
            <div className="editor-note">
              Saved for <b>{savedQuery.connection}</b>; running on <b>{connection.name}</b>.
            </div>
          )}
          <LintPanel findings={liveLint} onFix={applyLintFix} onFixAll={applyAllLintFixes} />
          <div className="editor-body" ref={body}>
            <div className="editor-host" ref={host} style={{ flex: `0 0 ${split * 100}%` }} />
            <div className="editor-splitter" onPointerDown={drag} role="separator" aria-orientation="horizontal" />
            <div className="editor-output">
              <OutputPane
                empty={<EmptyOutput saved={(saved ?? []).filter((q) => q.connection === connection.name).slice(0, 4)} onOpen={(q) => openQuery(q, false)} />}
                output={output}
                look={look}
                onLook={(next) => {
                  setLook(next);
                  rememberLook(next);
                }}
              />
            </div>
          </div>
        </div>
      </div>
      {pending && (
        <ConfirmRun
          pending={pending}
          connection={connection}
          onCancel={() => {
            setPending(null);
            view.current?.focus();
          }}
          onConfirm={() => {
            const sql = pending.sql;
            setPending(null);
            void run(sql, true);
          }}
        />
      )}
    </div>
  );
}
