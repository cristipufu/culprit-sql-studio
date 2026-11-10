import { ArrowRight, Bot, Check, ChevronDown, Clock3, FileText, KeyRound, Pencil, Trash2 } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import { ConnectionSettingsButton } from "./ConnectionSettingsButton";
import { SshSetting } from "./SshSetting";
import { EngineBadge } from "../../components/EngineBadge";
import type { AgentAccess } from "../../gen/AgentAccess";
import type { AuthKind } from "../../gen/AuthKind";
import type { ConnectionInfo } from "../../gen/ConnectionInfo";
import { api, errorMessage, isDesktop } from "../../lib/api";
import { connectFacts, serverName } from "../../lib/engines";
import { ago } from "../../lib/format";
import { t } from "../../lib/i18n";
import { loadConnections, openConnection, removeConnection, renameConnection } from "../../lib/store";

const AUTH_LABEL: Partial<Record<AuthKind, string>> = { entra: "Entra ID", "aws-iam": "AWS IAM" };

export function ConnectionCard({ connection: c }: { connection: ConnectionInfo }) {
  const [editing, setEditing] = useState(false);
  const [alias, setAlias] = useState(c.name);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const submitted = useRef(false);

  const nameId = useId();
  const product = serverName(c.engine, c.last_report?.product);
  const file = connectFacts(c.engine).file;
  const database = c.database ?? (file ? c.host.split(/[\\/]/).pop() : null);
  const host = c.host.includes(":") && !c.host.startsWith("[") ? `[${c.host}]` : c.host;
  const endpoint = file ? c.display : `${host}${c.port ? `:${c.port}` : ""}`;
  // A headline with only a count (no workload, or no finding above 5% of database time) still gets a line, so every
  // scanned card has the same shape.
  const headline = c.last_report
    ? c.last_report.no_workload
      ? t("ui.connection_card.no_query_history", { reason: c.last_report.no_workload })
      : (c.last_report.takeaway ?? t("ui.connection_card.no_takeaway"))
    : null;

  const save = async () => {
    if (submitted.current) return;
    submitted.current = true;
    if (alias === c.name) return setEditing(false);
    setSaving(true);
    try {
      await renameConnection(c.name, alias);
      setEditing(false);
      setError(null);
    } catch (e) {
      submitted.current = false;
      setError(errorMessage(e).message);
    } finally {
      setSaving(false);
    }
  };

  if (editing) {
    return (
      <article className="conn-card conn-card-editing">
        <EngineBadge engine={c.engine} environment={c.environment} product={c.last_report?.product} size="md" />
        <div className="conn-edit-name">
          <label htmlFor={nameId}>{t("ui.connection_card.name")}</label>
          <input id={nameId} className="alias-input" autoFocus value={alias} readOnly={saving}
            aria-label={t("ui.connection_card.rename", { name: c.name })} aria-busy={saving}
            onChange={(event) => setAlias(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") { event.preventDefault(); void save(); }
              if (event.key === "Escape" && !submitted.current) {
                event.preventDefault(); submitted.current = true; setEditing(false);
              }
            }} onBlur={save} />
          <span className="muted">{saving ? t("ui.common.saving") : t("ui.connection_card.rename_keys")}</span>
          {error && <span className="error" role="alert">{error}</span>}
        </div>
      </article>
    );
  }

  return (
    <article className="conn-card">
      <button type="button" className="conn-open" onClick={() => openConnection(c.name)}>
        <EngineBadge engine={c.engine} environment={c.environment} product={c.last_report?.product} size="md" />
        <span className="conn-identity">
          <strong>{c.name}</strong>
          <span className="conn-meta">{product}{database && <> · {database}</>}</span>
        </span>
        <span className="conn-count">
          {c.last_report ? <><b>{c.last_report.opportunities}</b><span>{t("ui.connection_card.opportunities", { count: c.last_report.opportunities })}</span></>
            : <span className="conn-scan">{t("ui.connection_card.first_scan")} <ArrowRight size={12} /></span>}
        </span>
        <span className="conn-endpoint" title={c.display}>
          <span>{file ? t("ui.connection_card.file") : t("ui.connection_card.host")}</span><span>{endpoint}</span>
          {AUTH_LABEL[c.auth] && <span className="conn-auth">{AUTH_LABEL[c.auth]}</span>}
        </span>
        {headline && <span className="conn-summary" title={headline}>{headline}</span>}
      </button>
      <div className="conn-card-footer">
        <span className={`conn-when ${c.last_report && stale(c.last_report.taken_at) ? "stale" : ""}`}
          title={c.last_report ? (stale(c.last_report.taken_at) ? t("ui.connection_card.stale", { time: c.last_report.taken_at }) : c.last_report.taken_at) : undefined}>
          <Clock3 size={12} aria-hidden="true" />
          {c.last_report ? t("ui.connection_card.scanned", { ago: ago(c.last_report.taken_at) }) : t("ui.connection_card.no_report")}
        </span>
        <span className="conn-tools">
          {c.auth === "password" && !c.has_secret && <SetPassword name={c.name} />}
          {(file || c.query_log) && <QueryLogSetting connection={c} />}
          {!file && <SshSetting connection={c} />}
          <ConnectionSettingsButton connection={c} />
          <AgentAccessMenu connection={c} />
          <button type="button" className="btn ghost conn-rename" title={t("ui.connection_card.rename", { name: c.name })} aria-label={t("ui.connection_card.rename", { name: c.name })}
            onClick={() => { submitted.current = false; setAlias(c.name); setError(null); setEditing(true); }}>
            <Pencil size={12} />
          </button>
          <RemoveConnection name={c.name} />
        </span>
      </div>
    </article>
  );
}

/** Forget the connection after an inline confirmation: its metadata and keychain password go; saved reports stay. */
function RemoveConnection({ name }: { name: string }) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const remove = async () => {
    setBusy(true);
    try {
      await removeConnection(name);
    } catch (e) {
      setError(errorMessage(e).message);
      setBusy(false);
    }
  };
  if (!open) {
    return (
      <button type="button" className="btn ghost conn-remove" title={t("ui.connection_card.remove", { name })} aria-label={t("ui.connection_card.remove", { name })}
        onClick={() => { setError(null); setOpen(true); }}>
        <Trash2 size={12} />
      </button>
    );
  }
  return (
    <span className="conn-settings-editor" role="group" aria-label={t("ui.connection_card.remove", { name })}
      onKeyDown={(e) => { if (e.key === "Escape" && !busy) { e.stopPropagation(); setOpen(false); } }}>
      <span className="muted">{t("ui.connection_card.remove_confirm", { name })}</span>
      <button type="button" className="btn small danger" autoFocus disabled={busy} aria-busy={busy} onClick={remove}>
        {busy ? t("ui.connection_card.removing") : t("ui.common.remove")}
      </button>
      <button type="button" className="btn small ghost" disabled={busy} onClick={() => setOpen(false)}>
        {t("ui.common.cancel")}
      </button>
      {error && <span className="error" role="alert">{error}</span>}
    </span>
  );
}

/** A report older than a day may describe a database that has moved on. */
function stale(iso: string): boolean {
  const t = Date.parse(iso);
  return !Number.isNaN(t) && Date.now() - t > 86_400_000;
}

function agentAccessOptions(): { value: AgentAccess; label: string; title: string }[] {
  return [
    { value: "off", label: t("ui.connection_card.agents.off"), title: t("ui.connection_card.agents.off_title") },
    { value: "stats", label: t("ui.connection_card.agents.stats"), title: t("ui.connection_card.agents.stats_title") },
    { value: "query", label: t("ui.connection_card.agents.query"), title: t("ui.connection_card.agents.query_title") },
  ];
}

/** What coding agents may do with this connection through `culprit mcp`: a compact chip that opens a small menu. */
function AgentAccessMenu({ connection: c }: { connection: ConnectionInfo }) {
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const root = useRef<HTMLSpanElement>(null);
  const items = useRef<(HTMLButtonElement | null)[]>([]);
  const menuId = useId();
  const options = agentAccessOptions();
  const currentIndex = Math.max(0, options.findIndex((a) => a.value === c.agent_access));
  const current = options[currentIndex]!;

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      if (!root.current?.contains(e.target as Node)) setOpen(false);
    };
    window.addEventListener("mousedown", onDown);
    requestAnimationFrame(() => items.current[currentIndex]?.focus());
    return () => window.removeEventListener("mousedown", onDown);
  }, [open, currentIndex]);

  const choose = async (value: AgentAccess) => {
    setOpen(false);
    if (value === c.agent_access) return;
    try {
      await (await api()).setAgentAccess(c.name, value);
      await loadConnections();
      setError(null);
    } catch (err) {
      setError(errorMessage(err).message);
    }
  };

  const onKey = (e: React.KeyboardEvent) => {
    e.stopPropagation();
    if (!open) return;
    const at = items.current.indexOf(document.activeElement as HTMLButtonElement);
    if (e.key === "Escape") setOpen(false);
    else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      items.current[(at + (e.key === "ArrowDown" ? 1 : -1) + options.length) % options.length]?.focus();
    }
  };

  return (
    <span className="agent-access" ref={root} onClick={(e) => e.stopPropagation()} onKeyDown={onKey}>
      <button type="button" className={`agent-chip access-${c.agent_access}`} aria-haspopup="menu" aria-expanded={open} aria-controls={menuId} title={current.title} onClick={() => setOpen((o) => !o)}>
        <Bot size={12} /> {t("ui.connection_card.agents.chip", { access: current.label })}
        <ChevronDown size={11} />
      </button>
      {open && (
        <span className="agent-menu" role="menu" id={menuId}>
          <span className="agent-menu-head">{t("ui.connection_card.agents.heading")}</span>
          {options.map((a, i) => (
            <button type="button"
              key={a.value}
              role="menuitemradio"
              aria-checked={a.value === c.agent_access}
              ref={(el) => {
                items.current[i] = el;
              }}
              className="agent-option"
              onClick={() => choose(a.value)}
            >
              <span className="agent-check">{a.value === c.agent_access && <Check size={13} />}</span>
              <span>
                <b>{a.label}</b>
                <span className="muted">{a.title}</span>
              </span>
            </button>
          ))}
        </span>
      )}
      {error && <span className="muted"> {error}</span>}
    </span>
  );
}

/** The log the application's framework writes (Rails, Django, EF Core, Phoenix), which scans read the statements from
 *  when the database keeps none (SQLite). Nothing changes in the application: Culprit reads the file it already writes. */
function QueryLogSetting({ connection: c }: { connection: ConnectionInfo }) {
  const [open, setOpen] = useState(false);
  const [path, setPath] = useState(c.query_log ?? "");
  const [error, setError] = useState<string | null>(null);
  const file = c.query_log?.split(/[\\/]/).pop();
  const save = async (value: string | null) => {
    try {
      await (await api()).setQueryLog(c.name, value);
      setOpen(false);
      setError(null);
      await loadConnections();
    } catch (e) {
      setError(errorMessage(e).message);
    }
  };
  // The desktop app picks the file in the system's own dialog; the browser demo can only take a typed path.
  const choose = async () => {
    try {
      const chosen = await (await api()).chooseQueryLog(c.name);
      setError(null);
      if (chosen) {
        setOpen(false);
        await loadConnections();
      }
    } catch (e) {
      setError(errorMessage(e).message);
    }
  };
  if (isDesktop() && open) {
    return (
      <span className="conn-settings-editor">
        <span className="muted mono" title={c.query_log ?? ""}>
          {file}
        </span>
        <button type="button" className="btn" onClick={choose}>
          {t("ui.connection_card.query_log.choose_another")}
        </button>
        <button type="button" className="btn ghost" onClick={() => save(null)}>
          {t("ui.common.remove")}
        </button>
        <button type="button" className="btn ghost" onClick={() => setOpen(false)}>
          {t("ui.common.done")}
        </button>
        {error && <span className="muted">{error}</span>}
      </span>
    );
  }
  if (!open) {
    return (
      <button type="button"
        className="btn ghost conn-log"
        aria-label={c.query_log ? t("ui.connection_card.query_log.label", { name: c.name, file: file ?? "" }) : t("ui.connection_card.query_log.add_label", { name: c.name })}
        title={c.query_log ? t("ui.connection_card.query_log.title", { path: c.query_log }) : t("ui.connection_card.query_log.add_title")}
        onClick={(e) => {
          e.stopPropagation();
          if (isDesktop() && !c.query_log) void choose();
          else { setPath(c.query_log ?? ""); setOpen(true); }
        }}
      >
        <FileText size={12} />
      </button>
    );
  }
  return (
    <span className="conn-settings-editor">
      <input
        className="alias-input"
        autoFocus
        value={path}
        placeholder="/path/to/log/production.log"
        onChange={(e) => setPath(e.target.value)}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Enter" && path.trim()) save(path.trim());
          if (e.key === "Escape") setOpen(false);
        }}
      />
      <button type="button" className="btn" disabled={!path.trim()} onClick={() => save(path.trim())}>
        {t("ui.common.save")}
      </button>
      {c.query_log && (
        <button type="button" className="btn ghost" onClick={() => save(null)}>
          {t("ui.common.remove")}
        </button>
      )}
      {error && <span className="muted">{error}</span>}
    </span>
  );
}

/** Discovered password-auth databases arrive without a secret; this stores one in the OS keychain. */
function SetPassword({ name }: { name: string }) {
  const [open, setOpen] = useState(false);
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const save = async () => {
    try {
      await (await api()).setPassword(name, password);
      setPassword("");
      setOpen(false);
      await loadConnections();
    } catch (e) {
      setError(errorMessage(e).message);
    }
  };
  if (!open) {
    return (
      <button type="button"
        className="btn ghost conn-key" title={t("ui.connection_card.set_password")} aria-label={t("ui.connection_card.set_password_for", { name })}
        onClick={(e) => {
          e.stopPropagation();
          setOpen(true);
        }}
      >
        <KeyRound size={12} />
      </button>
    );
  }
  return (
    <span className="conn-settings-editor">
      <input
        className="alias-input"
        type="password"
        autoFocus
        value={password}
        placeholder={t("ui.connection_card.password_placeholder")}
        onChange={(e) => setPassword(e.target.value)}
        onKeyDown={(e) => {
          e.stopPropagation();
          if (e.key === "Enter") save();
          if (e.key === "Escape") setOpen(false);
        }}
      />
      <button type="button" className="btn" disabled={!password} onClick={save}>
        {t("ui.common.save")}
      </button>
      {error && <span className="muted">{error}</span>}
    </span>
  );
}
