import { CalendarClock, Check, ChevronDown } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { windowSpan } from "../lib/format";
import { useHotkeys } from "../lib/hotkeys";
import { t } from "../lib/i18n";
import { setWindow, useStore, WINDOW_OPTIONS, type WindowMinutes } from "../lib/store";

const LABEL: Record<WindowMinutes, () => string> = {
  60: () => t("ui.window_picker.last_hour"),
  360: () => t("ui.window_picker.last_6_hours"),
  1440: () => t("ui.window_picker.last_24_hours"),
  10080: () => t("ui.window_picker.last_7_days"),
  43200: () => t("ui.window_picker.last_30_days"),
};
const SHORT: Record<WindowMinutes, () => string> = {
  60: () => t("ui.window_picker.short_1h"),
  360: () => t("ui.window_picker.short_6h"),
  1440: () => t("ui.window_picker.short_24h"),
  10080: () => t("ui.window_picker.short_7d"),
  43200: () => t("ui.window_picker.short_30d"),
};

/** Workload window for history-keeping sources (Query Store, the slow query log). `coveredHours` is what the source
 *  actually had; `source` names it. */
export function WindowPicker({ coveredHours, source }: { coveredHours: number | null; source: string | null }) {
  const from = source ?? "Query Store";
  const minutes = useStore((s) => s.windowMinutes);
  const [open, setOpen] = useState(false);
  const anchor = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (!anchor.current?.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [open]);

  const choose = (d: WindowMinutes) => {
    setOpen(false);
    setWindow(d);
  };
  const cycle = () => {
    const i = WINDOW_OPTIONS.indexOf(minutes);
    choose(WINDOW_OPTIONS[(i + 1) % WINDOW_OPTIONS.length]!);
  };
  useHotkeys({ w: cycle, Escape: () => setOpen(false) });

  const short = coveredHours != null && coveredHours < (minutes / 60) * 0.9;
  const held = coveredHours != null ? windowSpan(coveredHours) : "";
  // A window longer than the history shows nothing a shorter one doesn't, once the shortest such window covers it all.
  const same = (d: WindowMinutes) => coveredHours != null && WINDOW_OPTIONS.some((o) => o < d && coveredHours < (o / 60) * 0.9);

  return (
    <div className="menu-anchor" ref={anchor}>
      <button type="button" className="picker" aria-haspopup="listbox" aria-expanded={open} title={t("ui.window_picker.title")} onClick={() => setOpen((o) => !o)}>
        <CalendarClock size={13} />
        {LABEL[minutes]()}
        <ChevronDown size={13} className="muted" />
      </button>
      {short && (
        <span className="picker-note" title={t("ui.window_picker.short_history_title", { source: from, held })}>
          {t("ui.window_picker.short_history", { source: from, held })}
        </span>
      )}
      {open && (
        <div className="menu" role="listbox" aria-label={t("ui.window_picker.label")}>
          {WINDOW_OPTIONS.map((d) => (
            <button type="button" key={d} role="option" aria-selected={d === minutes} className={`menu-item ${d === minutes ? "on" : ""} ${same(d) ? "same" : ""}`} onClick={() => choose(d)}>
              <span>
                {LABEL[d]()}
                {same(d) && <span className="menu-sub">{t("ui.window_picker.same_data", { held })}</span>}
              </span>
              <span className="menu-key">{d === minutes ? <Check size={13} /> : SHORT[d]()}</span>
            </button>
          ))}
          <div className="menu-hint">{t("ui.window_picker.hint", { source: from })}</div>
        </div>
      )}
    </div>
  );
}
