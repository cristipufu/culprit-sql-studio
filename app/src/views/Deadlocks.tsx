import { ArrowUpRight, CircleCheck, ClipboardPaste, Copy, FileWarning } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { DeadlockGraph } from "../components/DeadlockGraph";
import { Sql } from "../components/Sql";
import type { Deadlock } from "../gen/Deadlock";
import type { DeadlockPattern } from "../gen/DeadlockPattern";
import type { DeadlockProcess } from "../gen/DeadlockProcess";
import type { DeadlockReport } from "../gen/DeadlockReport";
import type { Engine } from "../gen/Engine";
import { api, errorMessage, type ApiError } from "../lib/api";
import { useHotkeys } from "../lib/hotkeys";
import { t } from "../lib/i18n";
import { currentReport, setState, toast, useStore } from "../lib/store";
import { keys } from "../lib/keys";
import { copyAndTell } from "../lib/clipboard";
import { dateLocale, digits } from "../lib/format";
import { slot, withElements } from "../lib/message-parts";

function patternLabel(pattern: DeadlockPattern): string {
  switch (pattern) {
    case "key-lookup":
      return t("ui.deadlocks.pattern.key_lookup");
    case "lock-conversion":
      return t("ui.deadlocks.pattern.lock_conversion");
    case "opposite-order":
      return t("ui.deadlocks.pattern.opposite_order");
    case "lock-escalation":
      return t("ui.deadlocks.pattern.lock_escalation");
    case "parallelism":
      return t("ui.deadlocks.pattern.parallelism");
    case "cycle":
      return t("ui.deadlocks.pattern.cycle");
  }
}

const RANGES = [24, 168] as const;

function rangeLabel(hours: number): string {
  return hours === 168 ? t("ui.deadlocks.range.week") : t("ui.deadlocks.range.day");
}

type Load = { status: "loading" } | { status: "ready"; report: DeadlockReport } | { status: "error"; error: ApiError };

function when(iso: string | null): { short: string; full: string } {
  if (!iso) return { short: t("ui.deadlocks.time_unknown"), full: t("ui.deadlocks.time_unknown_full") };
  const d = new Date(iso);
  const today = new Date();
  const sameDay = d.toDateString() === today.toDateString();
  const locale = dateLocale();
  const time = d.toLocaleTimeString(locale, { hour: "numeric", minute: "2-digit" });
  const short = sameDay ? t("ui.deadlocks.today", { time }) : t("ui.format.when.this_year", { date: d.toLocaleDateString(locale, { month: "short", day: "numeric" }), time });
  return { short, full: d.toLocaleString(locale) };
}

/** Whose transaction was rolled back, said as Rust's `deadlocks::victim_text` says it: "session 202 (login root)",
 * never a bare login a reader takes for something else. */
function victimText(p: DeadlockProcess): string {
  const by = p.application ?? (p.login ? t("ui.deadlocks.victim.login", { login: p.login }) : null);
  if (p.session) return by ? t("ui.deadlocks.victim.session_by", { session: p.session, by }) : t("ui.deadlocks.victim.session", { session: p.session });
  return by ? t("ui.deadlocks.victim.by", { by }) : p.id;
}

function sourceLine(engine: Engine): string {
  switch (engine) {
    case "mssql":
      return t("ui.deadlocks.source.mssql");
    case "mysql":
      return t("ui.deadlocks.source.mysql");
    case "postgres":
      return t("ui.deadlocks.source.postgres");
    case "yugabyte":
      return t("ui.deadlocks.source.yugabyte");
    case "sqlite":
      return t("ui.deadlocks.source.sqlite");
  }
}

/** Recent deadlocks, explained: what pattern it is, who was rolled back, and the fix. Paste or drop one to explain it. */
export default function Deadlocks({ connection, engine }: { connection: string; engine: Engine }) {
  const [hours, setHours] = useState<number>(24);
  const [load, setLoad] = useState<Load>({ status: "loading" });
  const [pasted, setPasted] = useState<Deadlock[]>([]);
  const [pasteError, setPasteError] = useState<ApiError | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [dragging, setDragging] = useState(false);
  const report = useStore(currentReport);
  const listRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let cancelled = false;
    setLoad({ status: "loading" });
    api()
      .then((client) => client.deadlocks(connection, hours))
      .then((r) => !cancelled && setLoad({ status: "ready", report: r }))
      .catch((e) => !cancelled && setLoad({ status: "error", error: errorMessage(e) }));
    return () => {
      cancelled = true;
    };
  }, [connection, hours]);

  const all = useMemo(() => [...pasted, ...(load.status === "ready" ? load.report.deadlocks : [])], [pasted, load]);
  const current = all.find((d) => d.id === selected) ?? all[0] ?? null;

  const explain = useCallback(async (text: string) => {
    if (!text.trim()) return;
    try {
      const found = await (await api()).parseDeadlock(text);
      setPasted((p) => [...found, ...p.filter((x) => !found.some((f) => f.id === x.id))]);
      setSelected(found[0]?.id ?? null);
      setPasteError(null);
      toast(t("ui.deadlocks.explained", { n: found.length, count: String(found.length) }));
    } catch (e) {
      setPasteError(errorMessage(e));
    }
  }, []);

  useEffect(() => {
    const onPaste = (e: ClipboardEvent) => {
      const target = e.target as HTMLElement | null;
      if (target && (target.tagName === "INPUT" || target.tagName === "TEXTAREA" || target.isContentEditable)) return;
      const text = e.clipboardData?.getData("text") ?? "";
      if (text.includes("deadlock") || text.includes("DEADLOCK")) {
        e.preventDefault();
        void explain(text);
      }
    };
    window.addEventListener("paste", onPaste);
    return () => window.removeEventListener("paste", onPaste);
  }, [explain]);

  const move = (delta: number) => {
    if (!all.length) return;
    const i = Math.max(0, all.findIndex((d) => d.id === current?.id));
    const next = all[Math.min(all.length - 1, Math.max(0, i + delta))];
    if (next) {
      setSelected(next.id);
      listRef.current?.querySelector(`[data-id="${next.id}"]`)?.scrollIntoView({ block: "nearest" });
    }
  };
  useHotkeys({ j: () => move(1), k: () => move(-1), ArrowDown: () => move(1), ArrowUp: () => move(-1) });

  const onDrop = async (e: React.DragEvent) => {
    e.preventDefault();
    setDragging(false);
    const file = e.dataTransfer.files[0];
    if (file) await explain(await file.text());
    else await explain(e.dataTransfer.getData("text"));
  };

  const findingTitle = (id: string | null) => (id ? report?.findings.find((f) => f.id === id)?.title ?? null : null);
  const counted = load.status === "ready" ? load.report.counted : null;

  return (
    <div
      className={`dl-view ${dragging ? "dragging" : ""}`}
      onDragOver={(e) => {
        e.preventDefault();
        setDragging(true);
      }}
      onDragLeave={(e) => e.currentTarget === e.target && setDragging(false)}
      onDrop={onDrop}
    >
      <div className="dl-toolbar">
        {/* PostgreSQL only counts deadlocks since its statistics were reset: there's no window to choose. */}
        {engine !== "postgres" && (
          <span className="seg" role="radiogroup" aria-label={t("ui.deadlocks.how_far_back")}>
            {RANGES.map((r) => (
              <button type="button" key={r} role="radio" aria-checked={hours === r} className={hours === r ? "on" : ""} onClick={() => setHours(r)}>
                {rangeLabel(r)}
              </button>
            ))}
          </span>
        )}
        <span className="muted dl-source">{sourceLine(engine)}</span>
        <span className="spacer" />
        <span className="muted dl-paste-hint">
          <ClipboardPaste size={12} /> {withElements(t("ui.deadlocks.paste_hint", { key: slot(0) }), [<span key="key" className="kbd">{keys("⌘V")}</span>])}
        </span>
      </div>

      {pasteError && (
        <div className="dl-inline-note" role="alert">
          <span className="sev medium" aria-hidden /> {pasteError.message}
          {pasteError.hint && <span className="muted"> {pasteError.hint}</span>}
          <button type="button" className="btn ghost small" onClick={() => setPasteError(null)}>
            {t("ui.common.dismiss")}
          </button>
        </div>
      )}

      {load.status === "loading" && all.length === 0 ? (
        <Skeleton />
      ) : load.status === "error" && all.length === 0 ? (
        <div className="dl-empty">
          <FileWarning size={22} />
          <h3>{t("ui.deadlocks.read_failed")}</h3>
          <p>{load.error.message}</p>
          {load.error.hint && <p className="muted">{load.error.hint}</p>}
        </div>
      ) : all.length === 0 ? (
        <div className="dl-empty">
          {counted != null && counted > 0 ? (
            <>
              <div className="dl-counter">
                <span className="dl-counter-value">{digits(counted.toLocaleString("en-US"))}</span>
                <span className="dl-counter-label">
                  {load.status === "ready" && load.report.counted_since
                    ? t("ui.deadlocks.counted_since", { n: counted, time: when(load.report.counted_since).short })
                    : t("ui.deadlocks.counted", { n: counted })}
                </span>
              </div>
              <p>
                {withElements(t("ui.deadlocks.postgres_counter", { from: slot(0), to: slot(1) }), [
                  <code key="from">deadlock detected</code>,
                  <code key="to">STATEMENT</code>,
                ])}
              </p>
              {load.status === "ready" && load.report.lock_waits_logged === false && (
                <p className="muted">{t("ui.deadlocks.log_lock_waits_tip")}</p>
              )}
            </>
          ) : (
            <>
              <CircleCheck size={22} className="dl-ok" />
              {/* A counter (PostgreSQL) covers the time since statistics were reset, not the range. */}
              <h3>
                {counted != null
                  ? load.status === "ready" && load.report.counted_since
                    ? t("ui.deadlocks.none_since_reset_at", { time: when(load.report.counted_since).short })
                    : t("ui.deadlocks.none_since_reset")
                  : hours === 168
                    ? t("ui.deadlocks.none_in_week")
                    : t("ui.deadlocks.none_in_day")}
              </h3>
              <p className="muted">{load.status === "ready" && load.report.note ? load.report.note : sourceLine(engine)}</p>
            </>
          )}
          <DropZone onText={explain} />
        </div>
      ) : (
        <div className="dl-split">
          <div className="dl-list" ref={listRef} role="listbox" aria-label={t("ui.live.tab.deadlocks")}>
            {all.map((d) => {
              const victim = d.processes.find((p) => p.victim);
              const at = when(d.at);
              return (
                <button type="button"
                  key={d.id}
                  id={`dl-${d.id}`}
                  data-id={d.id}
                  role="option"
                  aria-selected={d.id === current?.id}
                  className={`dl-item ${d.id === current?.id ? "on" : ""}`}
                  onClick={() => setSelected(d.id)}
                >
                  <span className="dl-item-top">
                    <span className="dl-pattern">{patternLabel(d.pattern)}</span>
                    <span className="muted" title={at.full}>
                      {d.source === "pasted" ? t("ui.deadlocks.pasted") : at.short}
                    </span>
                  </span>
                  <span className="dl-item-objects">{d.objects.length ? d.objects.join(", ") : t("ui.deadlocks.objects_unnamed")}</span>
                  {victim && <span className="dl-item-victim muted">{t("ui.deadlocks.rolled_back_victim", { victim: victimText(victim) })}</span>}
                </button>
              );
            })}
            {load.status === "ready" && load.report.note && <p className="dl-list-note muted">{load.report.note}</p>}
          </div>
          {current && (
            <article className="dl-detail" key={current.id}>
              <div className="dl-detail-meta">
                <span className="dl-pattern">{patternLabel(current.pattern)}</span>
                <span className="muted" title={when(current.at).full}>
                  {current.source === "pasted" ? t("ui.deadlocks.pasted") : when(current.at).short} · {current.source}
                </span>
              </div>
              <h2 className="dl-headline">{current.headline}</h2>
              <DeadlockGraph deadlock={current} onPick={(p: DeadlockProcess) => document.getElementById(`dl-proc-${p.id}`)?.scrollIntoView({ block: "nearest", behavior: "smooth" })} />
              <p className="dl-explain">{current.explanation}</p>

              <h3 className="dl-section">{t("ui.deadlocks.how_to_fix")}</h3>
              <ol className="dl-fixes">
                {current.fixes.map((f, i) => {
                  const related = findingTitle(f.finding);
                  return (
                    <li key={i} className="dl-fix">
                      <div className="dl-fix-title">{f.title}</div>
                      <p>{f.detail}</p>
                      {f.sql && (
                        <div className="dl-fix-sql">
                          <Sql text={f.sql} pretty={false} />
                          <button type="button" className="btn ghost small" onClick={() => copyAndTell(f.sql!, t("ui.common.copied"))} aria-label={t("ui.deadlocks.copy_sql")}>
                            <Copy size={12} /> {t("ui.common.copy")}
                          </button>
                        </div>
                      )}
                      {related && f.finding && (
                        <button type="button" className="link-button dl-related" onClick={() => setState({ view: "overview", finding: f.finding })}>
                          {t("ui.deadlocks.same_fix", { finding: related })} <ArrowUpRight size={12} />
                        </button>
                      )}
                    </li>
                  );
                })}
              </ol>

              <h3 className="dl-section">{t("ui.deadlocks.involved")}</h3>
              <div className="dl-procs">
                {current.processes.map((p) => (
                  <div key={p.id} id={`dl-proc-${p.id}`} className={`dl-proc-card ${p.victim ? "victim" : ""}`}>
                    <div className="dl-proc-head">
                      <strong>{p.session ? t("ui.deadlocks.session", { session: p.session }) : t("ui.deadlocks.process", { id: p.id })}</strong>
                      {p.victim && <span className="pill victim">{t("ui.deadlocks.rolled_back")}</span>}
                    </div>
                    <div className="dl-proc-facts muted">
                      {[p.application, p.login && p.host ? `${p.login}@${p.host}` : (p.login ?? p.host), p.isolation, p.wait_mode ? t("ui.deadlocks.wanted", { mode: p.wait_mode }) : null, p.procedure ? t("ui.deadlocks.in_procedure", { procedure: p.procedure }) : null]
                        .filter(Boolean)
                        .join(" · ")}
                    </div>
                    {p.statement ? <Sql text={p.statement} className="dl-proc-sql" /> : <p className="muted">{t("ui.deadlocks.no_statement")}</p>}
                  </div>
                ))}
              </div>
            </article>
          )}
        </div>
      )}
      {dragging && <div className="dl-drop-overlay">{t("ui.deadlocks.drop")}</div>}
    </div>
  );
}

function DropZone({ onText }: { onText: (text: string) => Promise<void> }) {
  const [text, setText] = useState("");
  return (
    <div className="dl-dropzone">
      <ClipboardPaste size={16} />
      <div>
        <strong>{t("ui.deadlocks.dropzone.heading")}</strong>
        <p className="muted">{t("ui.deadlocks.dropzone.detail")}</p>
        <textarea
          aria-label={t("ui.deadlocks.dropzone.label")}
          placeholder={t("ui.deadlocks.dropzone.placeholder", { key: keys("⌘V") })}
          value={text}
          onChange={(e) => setText(e.target.value)}
          rows={3}
          spellCheck={false}
        />
        <button type="button" className="btn small" disabled={!text.trim()} onClick={() => onText(text).then(() => setText(""))}>
          {t("ui.deadlocks.explain")}
        </button>
      </div>
    </div>
  );
}

function Skeleton() {
  return (
    <div className="dl-split" role="status" aria-busy="true" aria-label={t("ui.deadlocks.loading")}>
      <div className="dl-list">
        {[0, 1, 2].map((i) => (
          <div key={i} className="dl-item skeleton">
            <span className="sk sk-short" />
            <span className="sk" />
          </div>
        ))}
      </div>
      <div className="dl-detail">
        <span className="sk sk-title" />
        <span className="sk sk-block" />
      </div>
    </div>
  );
}
