import { Check, KeyRound, Settings2, X } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import { createPortal } from "react-dom";
import type { ConnectionInfo } from "../../gen/ConnectionInfo";
import type { ConnectionSettings } from "../../gen/ConnectionSettings";
import { api, errorMessage, type ApiError } from "../../lib/api";
import { emptySecurity, securityDraft, sameSecurity } from "../../lib/connection-security";
import { connectionSecurityFacts, connectFacts, serverName } from "../../lib/engines";
import { useFocusTrap } from "../../lib/focus";
import { t } from "../../lib/i18n";
import { LatestRequest } from "../../lib/requests";
import { loadConnections, toast } from "../../lib/store";
import { ConnectionSecurityFields } from "./ConnectionSecurityFields";
import { EnvironmentPicker } from "./EnvironmentPicker";

export function ConnectionSettingsButton({ connection }: { connection: ConnectionInfo }) {
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  const opened = useRef(false);
  useEffect(() => {
    if (open) opened.current = true;
    else if (opened.current) { opened.current = false; trigger.current?.focus(); }
  }, [open]);
  return <>
    <button ref={trigger} type="button" className="btn ghost conn-settings" title={t("ui.connection_settings.edit_for", { name: connection.name })} aria-label={t("ui.connection_settings.edit_for", { name: connection.name })} onClick={() => setOpen(true)}><Settings2 size={13} /></button>
    {open && <ConnectionSettingsDialog key={connection.name} connection={connection} onClose={() => setOpen(false)} />}
  </>;
}

type Status = { kind: "idle" } | { kind: "testing" | "saving" } | { kind: "ok"; server: string } | { kind: "error"; error: ApiError };

function ConnectionSettingsDialog({ connection, onClose }: { connection: ConnectionInfo; onClose: () => void }) {
  const initial = useRef<ConnectionSettings>({ environment: connection.environment, security: connection.security?.settings ?? emptySecurity() });
  const [draft, setDraft] = useState<ConnectionSettings>(() => ({ ...initial.current, security: securityDraft(connection.security, connectionSecurityFacts(connection.engine)) }));
  const [status, setStatus] = useState<Status>({ kind: "idle" });
  const form = useRef<HTMLFormElement>(null);
  const closeButton = useRef<HTMLButtonElement>(null);
  const mode = useRef<HTMLSelectElement>(null);
  const action = useRef(new LatestRequest());
  const testAbort = useRef<AbortController | null>(null);
  const id = useId();
  const saving = status.kind === "saving";
  const securityChanged = !sameSecurity(draft.security, initial.current.security);
  const changed = draft.environment !== initial.current.environment || securityChanged;
  const file = connectFacts(connection.engine).file;
  useFocusTrap(form, true, () => { if (!saving) onClose(); });
  useEffect(() => { closeButton.current?.focus(); return () => { action.current.invalidate(); testAbort.current?.abort(); }; }, []);
  const update = (changes: Partial<ConnectionSettings>) => {
    action.current.invalidate(); testAbort.current?.abort(); setDraft((current) => ({ ...current, ...changes })); setStatus({ kind: "idle" });
  };
  const test = async () => {
    if (!form.current?.reportValidity()) return;
    const owns = action.current.start();
    testAbort.current?.abort(); const controller = new AbortController(); testAbort.current = controller;
    setStatus({ kind: "testing" });
    try {
      const server = await (await api()).testConnectionSettings(connection.name, draft, controller.signal);
      if (owns()) setStatus({ kind: "ok", server: `${server.product} · ${server.database}` });
    } catch (error) { if (owns()) setStatus({ kind: "error", error: errorMessage(error) }); }
  };
  const save = async () => {
    if (!form.current?.reportValidity() || saving) return;
    if (!changed) { onClose(); return; }
    const owns = action.current.start(); testAbort.current?.abort(); setStatus({ kind: "saving" });
    try {
      await (await api()).setConnectionSettings(connection.name, draft);
      await loadConnections();
      if (owns()) { toast(t("ui.connection_settings.saved", { name: connection.name })); onClose(); }
    } catch (error) { if (owns()) setStatus({ kind: "error", error: errorMessage(error) }); }
  };
  return createPortal(<div className="connection-settings-backdrop" onMouseDown={(event) => { if (event.target === event.currentTarget && !saving) onClose(); }}>
    <form ref={form} className="connection-settings-dialog" tabIndex={-1} role="dialog" aria-modal="true" aria-labelledby={`${id}-title`} onSubmit={(event) => { event.preventDefault(); void save(); }} onKeyDown={(event) => {
      event.stopPropagation();
      if (event.key === "Escape") { event.preventDefault(); if (!saving) onClose(); }
    }}>
      <header className="connection-settings-header"><div><div className="connection-settings-title"><h2 id={`${id}-title`}>{t("ui.connection_settings.title")}</h2>{changed && <span className="connection-settings-unsaved">{t("ui.connection_settings.unsaved")}</span>}</div><p>{connection.name}</p></div>
        <button ref={closeButton} type="button" className="icon-btn" disabled={saving} aria-label={t("ui.common.cancel")} onClick={onClose}><X size={18} /></button>
      </header>
      <div className="connection-settings-content">
        <p className="connection-settings-intro">{t("ui.connection_settings.subtitle")}</p>
        <dl className="connection-settings-endpoint"><div><dt>{t("ui.new_connection_form.engine")}</dt><dd>{serverName(connection.engine, connection.last_report?.product)}</dd></div>
          <div><dt>{file ? t("ui.new_connection_form.file") : t("ui.connection_settings.server")}</dt><dd className="mono">{file ? connection.database ?? connection.host : `${connection.host}${connection.port ? `:${connection.port}` : ""}`}</dd></div>
          {!file && connection.database && <div><dt>{t("ui.connection_settings.database")}</dt><dd className="mono">{connection.database}</dd></div>}
          {connection.user && <div><dt>{t("ui.connection_settings.user")}</dt><dd className="mono">{connection.user}</dd></div>}
        </dl>
        <div className="connection-settings-environment"><span id={`${id}-environment`}>{t("ui.new_connection_form.environment")}</span><EnvironmentPicker value={draft.environment} disabled={saving} labelledBy={`${id}-environment`} onChange={(environment) => update({ environment })} /></div>
        {connection.has_secret && <p className="connection-settings-credentials"><KeyRound size={13} aria-hidden />{t("ui.connection_settings.credentials_saved")}</p>}
        <ConnectionSecurityFields engine={connection.engine} info={connection.security} value={draft.security} source={securityChanged ? "custom" : "saved"} disabled={saving} modeRef={mode} onChange={(security) => update({ security })} onError={(error) => setStatus({ kind: "error", error })} />
        <div aria-live="polite">
          {status.kind === "error" && <div className="notice error connection-settings-status" role="alert"><div><b>{status.error.message}</b>{status.error.hint && <div>{status.error.hint}</div>}{status.error.detail === "connection-security" && <button type="button" className="btn small" onClick={() => mode.current?.focus()}>{t("ui.connection_security.configure")}</button>}</div></div>}
          {status.kind === "ok" && <div className="connection-settings-success"><Check size={15} aria-hidden />{t("ui.connection_settings.success", { server: status.server })}</div>}
        </div>
        <p className="connection-settings-scope">{t("ui.connection_settings.scope")}</p>
      </div>
      <footer className="connection-settings-actions"><button type="button" className="btn" disabled={saving || status.kind === "testing"} onClick={() => void test()}>{status.kind === "testing" ? <><span className="spinner" />{t("ui.connection_settings.testing")}</> : t("ui.connection_settings.test")}</button><span className="connection-settings-spacer" /><button type="button" className="btn ghost" disabled={saving} onClick={onClose}>{t("ui.common.cancel")}</button><button type="submit" className="btn primary" disabled={saving}>{saving ? t("ui.common.saving") : t("ui.connection_settings.save")}</button></footer>
    </form>
  </div>, document.body);
}
