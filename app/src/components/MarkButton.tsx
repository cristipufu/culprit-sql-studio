import { Flag } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { api, errorMessage } from "../lib/api";
import { eventsChanged } from "../lib/events";
import { t } from "../lib/i18n";
import { toast } from "../lib/store";
import "./timeline.css";

function localInput(t: number): string {
  const d = new Date(t);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/**
 * "Mark…": put a deploy, a migration or a config change on the connection's timeline, now or at an earlier time. The
 * charts and What changed line queries up with it; CI and coding agents do the same with `culprit mark` and the
 * `mark` MCP tool.
 */
export function MarkButton({ connection, label = t("ui.mark_button.mark") }: { connection: string; label?: string }) {
  const [open, setOpen] = useState(false);
  const [text, setText] = useState("");
  const [at, setAt] = useState(() => localInput(Date.now()));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    setAt(localInput(Date.now()));
    setError(null);
    input.current?.focus();
    const away = (e: PointerEvent) => !box.current?.contains(e.target as Node) && setOpen(false);
    document.addEventListener("pointerdown", away);
    return () => document.removeEventListener("pointerdown", away);
  }, [open]);

  const save = async () => {
    if (!text.trim() || busy) return;
    const time = Date.parse(at);
    setBusy(true);
    try {
      const client = await api();
      // Within a minute of now means "now"; an earlier time is kept as entered.
      const when = Number.isFinite(time) && Date.now() - time > 60_000 ? new Date(time).toISOString() : null;
      await client.mark(connection, text.trim(), when);
      setText("");
      setOpen(false);
      eventsChanged();
      toast(t("ui.mark_button.marked"));
    } catch (e) {
      setError(errorMessage(e).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <span className="mark-anchor" ref={box}>
      <button type="button" className="btn small" onClick={() => setOpen((o) => !o)} title={t("ui.mark_button.hint")} aria-expanded={open}>
        <Flag size={12} /> {label}
      </button>
      {open && (
        <div
          className="mark-pop"
          role="dialog"
          aria-label={t("ui.mark_button.title")}
          onKeyDown={(e) => {
            e.stopPropagation();
            if (e.key === "Escape") setOpen(false);
            if (e.key === "Enter") void save();
          }}
        >
          <h4>
            <Flag size={13} /> {t("ui.mark_button.title")}
          </h4>
          <p>{t("ui.mark_button.explain")}</p>
          <label>
            {t("ui.mark_button.what")}
            <input ref={input} value={text} maxLength={200} placeholder={t("ui.mark_button.placeholder")} onChange={(e) => setText(e.target.value)} />
          </label>
          <label>
            {t("ui.mark_button.when")}
            <input type="datetime-local" value={at} max={localInput(Date.now())} onChange={(e) => setAt(e.target.value)} />
          </label>
          {error && <div className="warn-text" style={{ fontSize: "var(--fs-ui-sm)", marginBottom: 6 }}>{error}</div>}
          <div className="row">
            <button type="button" className="btn small ghost" onClick={() => setOpen(false)}>
              {t("ui.common.cancel")}
            </button>
            <button type="button" className="btn small primary" onClick={() => void save()} disabled={!text.trim() || busy}>
              {busy ? t("ui.mark_button.marking") : t("ui.mark_button.mark")}
            </button>
          </div>
        </div>
      )}
    </span>
  );
}
