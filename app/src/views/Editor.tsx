import type { Extension } from "@codemirror/state";
import { keymap } from "@codemirror/view";
import { TriangleAlert } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ConfirmRun } from "../components/ConfirmRun";
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
import { type Acted, EditorExecution, type Output } from "../lib/editor-execution";
import type { PendingRun } from "../lib/run-batch";
import { remember, rememberedChoice } from "../lib/prefs";
import { t, useLanguage } from "../lib/i18n";
import { currentReport, getState, loadSaved, setState, toast, useStore } from "../lib/store";
import { sqlLanguage } from "./editor/codemirror";
import { placeholder, useLiveLint, useSplit } from "./editor/hooks";
import { LintPanel } from "./editor/LintPanel";
import { EmptyOutput, OutputPane } from "./editor/OutputPane";
import { TabStrip } from "./editor/TabStrip";
import { useTabViews } from "./editor/tab-views";
import { Toolbar } from "./editor/Toolbar";
import { slot, withElements } from "../lib/message-parts";

const RAIL_KEY = "culprit.editor.rail";
const ROW_LIMIT = 1000;

/** Catalogs fetched this session, per connection. */
const catalogs = new Map<string, Catalog>();

export default function Editor({ connection }: { connection: ConnectionInfo }) {
  const [tabStorage] = useState(() => new EditorTabStorage(connection, placeholder()));
  const [tabs, setTabs] = useState<Tabs>(tabStorage.initial);
  const [persistenceError, setPersistenceError] = useState(tabStorage.error);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  const tabsRef = useRef(tabs);
  tabsRef.current = tabs;
  const [outputs, setOutputs] = useState<Record<string, Output>>({});
  // Questions before a production change, by the tab that asked.
  const [asks, setAsks] = useState<Record<string, PendingRun>>({});
  // One operation per tab: it alone shows that tab's output, and closing the tab ends it.
  const [execution] = useState(
    () =>
      new EditorExecution(
        api,
        (tab, output) => setOutputs((o) => ({ ...o, [tab]: output })),
        (tab, pending) =>
          setAsks((a) => {
            const { [tab]: _, ...others } = a;
            return pending ? { ...others, [tab]: pending } : others;
          }),
      ),
  );
  useEffect(() => () => { if (execution.retireAll() > 0) toast(t("ui.editor.still_running")); }, [execution]);
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

  // Completion details are worded in the app's language: a new language rebuilds the SQL language with them.
  const appLanguage = useLanguage();
  const languageExtension = useCallback(
    (): Extension => {
      void appLanguage;
      return sqlLanguage(connection.engine, connection.last_report?.product, effectiveCatalog, report?.snapshot.connection === connection.name ? report : null);
    },
    [connection.engine, connection.last_report?.product, effectiveCatalog, report, connection.name, appLanguage],
  );

  // ── tab bookkeeping ────────────────────────────────────────────────────────────────────────────
  const updateTabs = useCallback((next: (t: Tabs) => Tabs) => {
    if (!mounted.current || getState().active !== connection.name) return;
    const value = next(tabsRef.current);
    tabsRef.current = value;
    setTabs(value);
    setPersistenceError(tabStorage.write(value));
  }, [tabStorage, connection.name]);

  // The editor's own keys, read when pressed: the actions they call are this render's.
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

  // The CodeMirror view and each tab's editing state.
  const views = useTabViews({
    language: languageExtension,
    keys: actionKeys.current,
    onEdit: (text) => {
      const id = tabsRef.current.active;
      updateTabs((t) => ({ ...t, tabs: t.tabs.map((x) => (x.id === id ? { ...x, sql: text } : x)) }));
    },
    initial: () => tabsRef.current.tabs.find((tab) => tab.id === tabsRef.current.active) ?? tabsRef.current.tabs[0]!,
    rebuild: connection.engine,
  });
  const { show, forget, view, textOf } = views;

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
    toast(t("ui.editor.applied_fixes", { count: applied }));
  };

  // ── actions ────────────────────────────────────────────────────────────────────────────────────
  /** What a tab's action came to, for what follows it here: a refusal said, a saved query's run counted. */
  const settle = (tab: string, acted: Acted) => {
    if (acted === "busy") return toast(t("ui.editor.busy"));
    if (acted !== "rows") return;
    const name = tabsRef.current.tabs.find((t) => t.id === tab)?.saved;
    if (name) void api().then((client) => client.savedQueryRan(name)).then(() => loadSaved(), () => {});
  };

  const run = async (sql: string) => {
    if (!sql || sql === placeholder().trim()) return;
    const statements = splitSqlStatements(sql);
    if (statements.length === 0) {
      return toast(t("ui.editor.nothing_to_run"));
    }
    const tab = tabsRef.current.active;
    const batch = { connection: connection.name, engine: connection.engine, sql, statements, limit: ROW_LIMIT, production, confirmed: false };
    settle(tab, await execution.run(tab, batch));
  };

  const explain = async (mode: ExplainMode) => {
    const raw = views.target();
    if (!raw || raw === placeholder().trim() || (mode === "analyze" && !measuresRuns(connection.engine))) return;
    const statements = splitSqlStatements(raw);
    if (statements.length === 0) {
      return toast(t("ui.editor.nothing_to_run"));
    }
    const sql = statements[statements.length - 1]!;
    const tab = tabsRef.current.active;
    settle(tab, await execution.explain(tab, connection.name, sql, mode));
  };

  const save = async (name?: string) => {
    const tab = tabsRef.current.tabs.find((t) => t.id === tabsRef.current.active);
    if (!tab) return;
    const text = views.text().trim();
    if (!text || text === placeholder().trim()) return toast(t("ui.editor.write_first"));
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
      toast(t("ui.editor.saved", { name: stored.name }));
    } catch (e) {
      toast(errorMessage(e).message);
    }
  };

  const activate = useCallback(
    (id: string, added?: Tab) => {
      if (!added && !tabsRef.current.tabs.some((t) => t.id === id)) return;
      updateTabs((t) => ({ tabs: added ? [...t.tabs, added] : t.tabs, active: id }));
      const tab = tabsRef.current.tabs.find((t) => t.id === id);
      if (tab) show(tab);
    },
    [show, updateTabs],
  );

  const newTab = useCallback(
    (sql = "", title?: string, savedName: string | null = null) => {
      if (tabsRef.current.tabs.length >= MAX_EDITOR_TABS) {
        toast(t("ui.editor.tabs_full", { count: MAX_EDITOR_TABS }));
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
      if (execution.retire(id)) toast(t("ui.editor.still_running"));
      forget(id);
      if (list.length === 1) {
        setOutputs({});
        const fresh = newEditorTab();
        updateTabs(() => ({ tabs: [fresh], active: fresh.id }));
        show(fresh, false);
        return;
      }
      const wasActive = tabsRef.current.active === id;
      const index = list.findIndex((t) => t.id === id);
      const rest = list.filter((t) => t.id !== id);
      setOutputs((o) => {
        const { [id]: _, ...others } = o;
        return others;
      });
      const next = tabsRef.current.active === id ? rest[Math.max(0, index - 1)]!.id : tabsRef.current.active;
      updateTabs(() => ({ tabs: rest, active: next }));
      if (wasActive) show(rest.find((t) => t.id === next)!);
    },
    [show, forget, updateTabs, execution],
  );

  const cycle = (delta: number) => {
    const list = tabsRef.current.tabs;
    const index = list.findIndex((t) => t.id === tabsRef.current.active);
    activate(list[(index + delta + list.length) % list.length]!.id);
  };

  const actions = useRef({ run, explain, save, newTab, closeTab, cycle, target: views.target });
  actions.current = { run, explain, save, newTab, closeTab, cycle, target: views.target };

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
  // ── lifecycle ──────────────────────────────────────────────────────────────────────────────────
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
    const open = wanted ? tabsRef.current.tabs.find((t) => textOf(t).trim() === wanted) : undefined;
    if (open) activate(open.id);
    else newTab(incoming);
  }, [incoming, newTab, activate, view, textOf]);

  // A saved query picked in the command palette.
  useEffect(() => {
    if (!openSaved || getState().editorSaved !== openSaved || saved === null) return;
    const query = saved.find((q) => q.name === openSaved.name);
    setState({ editorSaved: null });
    if (query) openQuery(query, openSaved.run);
  }, [openSaved, saved, openQuery]);

  const { split, body, drag } = useSplit();

  const savedQuery = saved?.find((q) => q.name === activeTab.saved);
  // One question at a time: the active tab's when it asked one, else the first tab's that did.
  const asking = (() => {
    const tab = asks[activeTab.id] ? activeTab.id : Object.keys(asks)[0];
    return tab ? { tab, pending: asks[tab]! } : null;
  })();
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
        <button type="button" className="btn" onClick={() => save()}>{t("ui.editor.save_query")}</button>
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
            onRun={() => run(views.target())}
            onExplain={explain}
            onSave={save}
            onCancelNaming={views.focus}
            catalogBusy={catalogBusy}
            tables={effectiveCatalog ? effectiveCatalog.tables.length : null}
            onRefreshCatalog={refreshCatalog}
          />
          {savedQuery?.connection && savedQuery.connection !== connection.name && (
            <div className="editor-note">
              {withElements(t("ui.editor.saved_elsewhere", { saved: slot(0), running: slot(1) }), [<b key="saved">{savedQuery.connection}</b>, <b key="running">{connection.name}</b>])}
            </div>
          )}
          <LintPanel findings={liveLint} onFix={applyLintFix} onFixAll={applyAllLintFixes} />
          <div className="editor-body" ref={body}>
            <div className="editor-host" ref={views.host} style={{ flex: `0 0 ${split * 100}%` }} />
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
      {asking && (
        <ConfirmRun
          key={asking.tab}
          pending={asking.pending}
          connection={connection}
          onCancel={() => {
            execution.cancel(asking.tab);
            views.focus();
          }}
          onConfirm={() => void execution.confirm(asking.tab).then((acted) => settle(asking.tab, acted))}
        />
      )}
    </div>
  );
}
