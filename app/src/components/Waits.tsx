import { ChevronRight } from "lucide-react";
import { useMemo, type KeyboardEvent } from "react";
import type { Report } from "../gen/Report";
import type { Signal } from "../gen/Signal";
import { percent } from "../lib/format";
import { foldActivity, foldWaits, signalValue, waitColor } from "../lib/waits";
import { can, serverName } from "../lib/engines";
import { t, useLanguage } from "../lib/i18n";

/** Which part of the waits panel to open. */
export type WaitsTab = "waits" | "now" | "health";
export type OpenWaits = (tab: WaitsTab, category?: string) => void;

/** Short names for the legend under the bar, where space is tight. */
const SHORT: Record<string, () => string> = {
  cpu: () => t("ui.waits.short.cpu"),
  io: () => t("ui.waits.short.io"),
  rows: () => t("ui.waits.short.rows"),
  lock: () => t("ui.waits.short.lock"),
  latch: () => t("ui.waits.short.latch"),
  log: () => t("ui.waits.short.log"),
  network: () => t("ui.waits.short.network"),
  parallelism: () => t("ui.waits.short.parallelism"),
  memory: () => t("ui.waits.short.memory"),
  compilation: () => t("ui.waits.short.compilation"),
  other: () => t("ui.waits.short.other"),
};

/** A category's short name, or its full one where it has none. */
const short = (key: string, label: string) => SHORT[key]?.() ?? label;


function activate(e: KeyboardEvent, run: () => void) {
  if (e.key === "Enter" || e.key === " ") {
    e.preventDefault();
    run();
  }
}

/**
 * The Overview's "Where time goes" card: the biggest category, a bar you can click by segment, and a legend. It keeps its
 * place on every engine; when the scan recorded no waits it offers a live sample instead.
 */
export function WaitsVital({ report, onOpen }: { report: Report; onOpen: OpenWaits }) {
  const activity = report.snapshot.operations.activity_history;
  // Folding labels the activity in the app's language, so a new language folds it again.
  const language = useLanguage();
  const sampled = useMemo(() => {
    void language;
    return activity ? foldActivity(activity) : [];
  }, [activity, language]);
  if (activity) {
    const categories = sampled;
    const top = categories[0];
    return <div className="vital waits-vital" role="button" tabIndex={0} onClick={() => onOpen("waits", top?.key)} onKeyDown={(e) => activate(e, () => onOpen("waits", top?.key))} title={t("ui.waits.activity_title")}>
      <div className="label">{t("ui.waits.recorded_activity")}</div><div className="value waits-value">{top?.label ?? t("ui.waits.no_samples")}</div>
      <div className="waits-bar" aria-hidden>{categories.map((c) => <span key={c.key} style={{ flexGrow: c.weight, background: waitColor(c.key) }} />)}</div>
      <div className="hint">{top ? t("ui.waits.sample_weight", { share: percent(top.share) }) : t("ui.waits.sampling_may_miss")}{activity.period.partial && ` · ${t("ui.waits.partial_read")}`}</div>
    </div>;
  }
  const waits = foldWaits(report.snapshot.waits);
  if (waits.length === 0 && !can(report.snapshot.server.engine, "wait_sample")) {
    return (
      <div className="vital waits-vital" title={t("ui.waits.no_server_title", { engine: serverName(report.snapshot.server.engine) })}>
        <div className="label">{t("ui.waits.where_time_goes")}</div>
        <div className="value muted">{t("ui.waits.not_recorded")}</div>
        <div className="hint">{t("ui.waits.no_server")}</div>
      </div>
    );
  }
  if (waits.length === 0) {
    return (
      <div className="vital waits-vital" role="button" tabIndex={0} onClick={() => onOpen("now")} onKeyDown={(e) => activate(e, () => onOpen("now"))} title={t("ui.waits.sample_title")}>
        <div className="label">{t("ui.waits.where_time_goes")}</div>
        <div className="value muted">{t("ui.waits.not_tracked")}</div>
        <div className="hint waits-cta">
          {t("ui.waits.sample_now")} <ChevronRight size={11} />
        </div>
      </div>
    );
  }
  const top = waits[0]!;
  return (
    <div className="vital waits-vital" role="button" tabIndex={0} onClick={() => onOpen("waits", top.key)} onKeyDown={(e) => activate(e, () => onOpen("waits", top.key))} title={t("ui.waits.see_title")}>
      <div className="label">{t("ui.waits.where_time_goes")}</div>
      <div className="value waits-value" title={t("ui.waits.top_title", { category: top.label, share: percent(top.share) })}>
        <span className="full">{top.label}</span>
        <span className="short">{short(top.key, top.label)}</span>
      </div>
      <div className="waits-bar" aria-hidden>
        {waits.map((w) => (
          // biome-ignore lint/a11y/useKeyWithClickEvents: a pointer shortcut in an aria-hidden bar; the card opens the same view from the keyboard
          <span
            key={w.key}
            title={`${w.label} · ${percent(w.share)}`}
            style={{ flexGrow: w.ms, background: waitColor(w.key) }}
            onClick={(e) => {
              e.stopPropagation();
              onOpen("waits", w.key);
            }}
          />
        ))}
      </div>
      <div className="waits-legend">
        {waits.slice(0, 3).map((w) => (
          <span key={w.key}>
            <i style={{ background: waitColor(w.key) }} />
            {short(w.key, w.label)} {percent(w.share)}
          </span>
        ))}
      </div>
    </div>
  );
}

const RANK: Record<Signal["status"], number> = { bad: 0, watch: 1, info: 2, good: 3 };

/** One calm line under the vitals: the signals that need a look, or a note that all is well. */
export function HealthLine({ report, onOpen }: { report: Report; onOpen: OpenWaits }) {
  const signals = report.snapshot.pressure?.signals ?? [];
  const resources = report.snapshot.operations.node_resources;
  const resourceMissing = report.snapshot.sources?.some((s) => s.source === "docdb-node-resources" && s.state === "unavailable");
  if (signals.length === 0 && !resources && !resourceMissing) return null;
  const notable = signals.filter((s) => s.status === "bad" || s.status === "watch").sort((a, b) => RANK[a.status] - RANK[b.status]);
  const graded = signals.filter((s) => s.status !== "info").length;
  return (
    <div className="health-line">
      <span className="health-line-label">{t("ui.waits.health")}</span>
      {notable.length === 0 ? (
        <span className="health-pill">
          <i className={`sig-dot ${graded > 0 ? "good" : "info"}`} /> {graded > 0 ? t("ui.waits.healthy_signals", { count: graded }) : signals.length === 0 ? resources?.nodes.some((n) => n.available) ? t("ui.waits.node_observations") : t("ui.waits.node_unavailable") : t("ui.waits.no_verdict")}
        </span>
      ) : (
        notable.slice(0, 1).map((s) => (
          <button type="button" key={s.key} className="health-pill" onClick={() => onOpen("health")} title={`${s.label}: ${signalValue(s)}${s.healthy ? ` (${s.healthy})` : ""}`}>
            <i className={`sig-dot ${s.status}`} />
            <span className="health-pill-label">{s.label}</span> <b>{signalValue(s)}</b>
          </button>
        ))
      )}
      <span className="spacer" />
      <button type="button" className="health-more" onClick={() => onOpen("health")}>
        {notable.length > 1 ? `${t("ui.waits.more_signals", { count: String(notable.length - 1) })} · ` : ""}{t("ui.waits.all_signals")} <ChevronRight size={12} />
      </button>
    </div>
  );
}
