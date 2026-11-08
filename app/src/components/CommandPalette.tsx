import { Activity, Bookmark, Camera, CircleArrowUp, CircleDot, Database, Gauge, GitCompareArrows, Languages, ListOrdered, Moon, Plus, RefreshCw, RotateCw, Skull, SquareTerminal, Sun, Table2, TextSearch } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { can } from "../lib/engines";
import { useFocusTrap } from "../lib/focus";
import { rowCount } from "../lib/format";
import { openLive } from "../views/Live";
import { languages, setLanguage, t, useLanguage } from "../lib/i18n";
import { useTheme } from "../lib/theme";
import { EngineBadge } from "./EngineBadge";
import { useHotkeys } from "../lib/hotkeys";
import { currentReport, loadSaved, openConnection, scan, setState, useStore, type View } from "../lib/store";
import { keys } from "../lib/keys";
import { canScreenshot, screenshotWorkspace } from "../lib/screenshot";
import { checkForUpdates, installUpdate, restartToUpdate, setAutoCheck, useAutoCheck, useUpdate } from "../lib/updates";

/** `rank` orders equally good matches: findings keep the report's impact order. */
type Item = { id: string; section: string; title: string; hint?: string; icon?: ReactNode; run: () => void; rank?: number };

/** The views the palette goes to, each with its name in the app's language. */
function views(): { view: View; title: string; icon: ReactNode }[] {
  return [
    { view: "dashboard", title: t("ui.palette.view.dashboard"), icon: <Gauge size={13} /> },
    { view: "overview", title: t("ui.palette.view.overview"), icon: <CircleDot size={13} /> },
    { view: "queries", title: t("ui.palette.view.queries"), icon: <ListOrdered size={13} /> },
    { view: "live", title: t("ui.palette.view.live"), icon: <Activity size={13} /> },
    { view: "schema", title: t("ui.palette.view.schema"), icon: <Table2 size={13} /> },
    { view: "editor", title: t("ui.palette.view.editor"), icon: <SquareTerminal size={13} /> },
    { view: "plans", title: t("ui.palette.view.plans"), icon: <GitCompareArrows size={13} /> },
    { view: "connections", title: t("ui.palette.view.connections"), icon: <Database size={13} /> },
  ];
}

const SEVERITY_COLOR: Record<string, string> = { high: "var(--high-text)", medium: "var(--medium-text)", low: "var(--low-text)" };

const icon = (node: ReactNode) => <span className="p-icon">{node}</span>;

function score(text: string, query: string): number {
  if (!query) return 1;
  const lower = text.toLowerCase();
  const q = query.toLowerCase();
  const direct = lower.indexOf(q);
  // A match at the start of a word ranks above one inside it.
  if (direct >= 0) return 100 - direct + (direct === 0 || /[\s._\-·/]/.test(lower[direct - 1] ?? "") ? 50 : 0);
  // One or two letters scattered through a title match almost everything: short queries need a real substring.
  if (q.length < 3) return 0;
  let ti = 0;
  let hits = 0;
  for (const ch of q) {
    const found = lower.indexOf(ch, ti);
    if (found < 0) return 0;
    hits += found === ti ? 2 : 1;
    ti = found + 1;
  }
  return hits;
}

/** How a match reads: 2 at the start of a word, 1 inside one, 0 for scattered letters. */
function matchTier(text: string, query: string): number {
  const lower = text.toLowerCase();
  const at = lower.indexOf(query.toLowerCase());
  if (at < 0) return 0;
  return at === 0 || /[\s._\-·/]/.test(lower[at - 1] ?? "") ? 2 : 1;
}

export function CommandPalette() {
  const connections = useStore((s) => s.connections);
  const report = useStore(currentReport);
  const active = useStore((s) => s.active);
  const saved = useStore((s) => s.saved);
  const [query, setQuery] = useState("");

  useEffect(() => {
    if (saved === null) void loadSaved();
  }, [saved]);
  const [index, setIndex] = useState(0);
  const [theme, , setTheme] = useTheme();
  const language = useLanguage();
  const update = useUpdate();
  const autoCheck = useAutoCheck();
  const panel = useRef<HTMLDivElement>(null);
  useFocusTrap(panel, true, () => setState({ palette: false }));

  const items = useMemo<Item[]>(() => {
    const close = () => setState({ palette: false });
    const list: Item[] = [];
    const actions = t("ui.palette.section.actions");
    const goTo = t("ui.palette.section.go_to");
    if (active) {
      list.push({ id: "rescan", section: actions, title: t("ui.palette.scan_now"), hint: keys("⌘R"), icon: icon(<RefreshCw size={13} />), run: () => (close(), scan(active)) });
      list.push({ id: "new-tab", section: actions, title: t("ui.palette.new_tab"), hint: keys("⌘T"), icon: icon(<Plus size={13} />), run: () => setState({ view: "editor", editorSql: " ", palette: false }) });
    }
    if (canScreenshot()) {
      list.push({ id: "screenshot", section: actions, title: t("ui.palette.screenshot"), hint: t("ui.palette.screenshot_hint"), icon: icon(<Camera size={13} />), run: () => void screenshotWorkspace() });
    }
    list.push({ id: "paste-plan", section: actions, title: t("ui.palette.open_plan"), icon: icon(<GitCompareArrows size={13} />), run: () => setState({ view: "plans", palette: false }) });
    const dark = theme === "dark" || (theme === "system" && window.matchMedia?.("(prefers-color-scheme: dark)").matches);
    list.push({
      id: "theme",
      section: actions,
      title: dark ? t("ui.palette.light_theme") : t("ui.palette.dark_theme"),
      icon: icon(dark ? <Sun size={13} /> : <Moon size={13} />),
      run: () => (setTheme(dark ? "light" : "dark"), close()),
    });
    for (const l of languages().filter((l) => l.code !== language)) {
      list.push({ id: `language-${l.code}`, section: actions, title: t("ui.palette.show_in", { language: l.name }), hint: l.englishName, icon: icon(<Languages size={13} />), run: () => (setLanguage(l.code), close()) });
    }
    if (update.status === "available" || update.status === "failed") {
      list.push({ id: "update-install", section: actions, title: t("ui.palette.update", { version: update.info.version }), icon: icon(<CircleArrowUp size={13} />), run: () => (close(), void installUpdate()) });
    } else if (update.status === "ready") {
      list.push({ id: "update-restart", section: actions, title: t("ui.palette.restart_to_update", { version: update.info.version }), icon: icon(<RotateCw size={13} />), run: () => void restartToUpdate() });
    } else {
      list.push({ id: "update-check", section: actions, title: t("ui.palette.check_updates"), icon: icon(<CircleArrowUp size={13} />), run: () => (close(), void checkForUpdates(true)) });
    }
    list.push({
      id: "update-auto",
      section: actions,
      title: autoCheck ? t("ui.palette.stop_daily_check") : t("ui.palette.daily_check"),
      hint: autoCheck ? t("ui.palette.on") : t("ui.palette.off"),
      icon: icon(<CircleArrowUp size={13} />),
      run: () => (setAutoCheck(!autoCheck), close()),
    });
    const engine = connections.find((c) => c.name === active)?.engine;
    const sessions = !engine || can(engine, "sessions");
    views().filter((v) => (active || v.view === "plans" || v.view === "connections") && (v.view !== "live" || sessions)).forEach((v) =>
      list.push({ id: `view:${v.view}`, section: goTo, title: v.title, icon: icon(v.icon), run: () => setState({ view: v.view, palette: false }) }),
    );
    if (active && engine && can(engine, "deadlocks")) {
      list.push({ id: "view:deadlocks", section: goTo, title: t("ui.palette.deadlocks"), hint: t("ui.palette.deadlocks_hint"), icon: icon(<Skull size={13} />), run: () => (close(), openLive("deadlocks")) });
    }
    saved?.forEach((q) => {
      const hint = q.connection ? t("ui.palette.saved_query_on", { connection: q.connection }) : t("ui.palette.saved_query");
      list.push({ id: `s:${q.name}`, section: t("ui.palette.section.saved"), title: t("ui.palette.run_saved", { name: q.name }), hint, icon: icon(<Bookmark size={13} />), run: () => setState({ view: "editor", editorSaved: { name: q.name, run: true }, palette: false }) });
    });
    report?.findings.forEach((f, rank) =>
      list.push({ id: `f:${f.id}`, section: t("ui.palette.section.findings"), title: f.title, hint: f.impact.label, rank, icon: icon(<CircleDot size={12} color={SEVERITY_COLOR[f.severity]} />), run: () => setState({ view: "overview", finding: f.id, palette: false }) }),
    );
    report?.snapshot.tables.forEach((table) =>
      list.push({
        id: `t:${table.schema}.${table.name}`,
        section: t("ui.palette.section.tables"),
        title: `${table.schema}.${table.name}`,
        hint: table.rows != null ? rowCount(table.rows) : t("ui.palette.table"),
        icon: icon(<Table2 size={13} />),
        run: () => setState({ view: "schema", schemaTable: `${table.schema}.${table.name}`, palette: false }),
      }),
    );
    report?.snapshot.workload?.queries.forEach((q) =>
      list.push({ id: `q:${q.id}`, section: t("ui.palette.section.queries"), title: q.text.replace(/\s+/g, " ").slice(0, 120), hint: t("ui.palette.query", { id: q.id }), icon: icon(<TextSearch size={13} />), run: () => setState({ view: "queries", query: q.id, palette: false }) }),
    );
    connections.forEach((c) =>
      list.push({ id: `c:${c.name}`, section: t("ui.palette.section.connections"), title: c.name, hint: c.display, icon: <EngineBadge engine={c.engine} environment={c.environment} product={c.last_report?.product} />, run: () => (close(), openConnection(c.name)) }),
    );
    list.push({ id: "new", section: actions, title: t("ui.palette.add_connection"), icon: icon(<Plus size={13} />), run: () => setState({ view: "connections", palette: false }) });
    return list;
  }, [connections, report, active, saved, theme, setTheme, update, autoCheck, language]);

  const results = useMemo(() => {
    const scored = items
      .map((item) => {
        const text = `${item.title} ${item.hint ?? ""}`;
        return { item, s: score(text, query), tier: query ? matchTier(text, query) : 0 };
      })
      .filter((x) => x.s > 0);
    if (!query) return scored.slice(0, 60).map((x) => x.item);
    // Word matches first; among them the most important finding (the report's order) before the earliest match, so
    // "orders" lists the slow update of orders before a minor index on it.
    scored.sort((a, b) => b.tier - a.tier || (a.item.rank ?? 0) - (b.item.rank ?? 0) || b.s - a.s);
    const sections = [...new Set(scored.map((x) => x.item.section))];
    return sections.flatMap((section) => scored.filter((x) => x.item.section === section).slice(0, 8)).map((x) => x.item);
  }, [items, query]);

  const choose = (i: number) => results[i]?.run();

  useHotkeys({ Escape: () => setState({ palette: false }) });

  return (
    <div className="overlay" onMouseDown={() => setState({ palette: false })}>
      <div ref={panel} className="palette" role="dialog" aria-modal="true" aria-label={t("ui.palette.label")} onMouseDown={(e) => e.stopPropagation()}>
        <input
          autoFocus
          role="combobox"
          aria-expanded="true"
          aria-controls="palette-results"
          aria-autocomplete="list"
          aria-activedescendant={results[index] ? `palette-item-${index}` : undefined}
          aria-label={t("ui.palette.search_label")}
          value={query}
          placeholder={t("ui.palette.search")}
          onChange={(e) => {
            setQuery(e.target.value);
            setIndex(0);
          }}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") {
              e.preventDefault();
              setIndex((i) => Math.min(results.length - 1, i + 1));
            } else if (e.key === "ArrowUp") {
              e.preventDefault();
              setIndex((i) => Math.max(0, i - 1));
            } else if (e.key === "Enter") {
              e.preventDefault();
              choose(index);
            }
          }}
        />
        <div className="results" id="palette-results" role="listbox" aria-label={t("ui.palette.results")}>
          {results.map((item, i) => (
            <div key={item.id} role="presentation">
              {(i === 0 || results[i - 1]?.section !== item.section) && (
                <div className="section eyebrow" role="presentation">
                  {item.section}
                </div>
              )}
              <button type="button"
                id={`palette-item-${i}`}
                role="option"
                aria-selected={i === index}
                tabIndex={-1}
                className={`item ${i === index ? "on" : ""}`}
                onMouseEnter={() => setIndex(i)}
                onClick={() => choose(i)}
              >
                {item.icon}
                <span className="t">{item.title}</span>
                {item.hint && <span className="k">{item.hint}</span>}
              </button>
            </div>
          ))}
          {results.length === 0 && <div className="muted" style={{ padding: 12 }}>{t("ui.palette.no_matches")}</div>}
        </div>
        <div className="palette-foot" aria-hidden="true">
          <span>
            <span className="kbd">↑</span>
            <span className="kbd">↓</span> {t("ui.palette.move")}
          </span>
          <span>
            <span className="kbd">↵</span> {t("ui.palette.open")}
          </span>
          <span>
            <span className="kbd">esc</span> {t("ui.palette.close")}
          </span>
        </div>
      </div>
    </div>
  );
}
