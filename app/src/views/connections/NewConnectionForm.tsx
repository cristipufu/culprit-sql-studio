import { Check, Database, FolderOpen } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import type { ConnectionSecurity } from "../../gen/ConnectionSecurity";
import type { Engine } from "../../gen/Engine";
import type { Environment } from "../../gen/Environment";
import type { ParsedConnection } from "../../gen/ParsedConnection";
import { api, errorMessage, type ApiError } from "../../lib/api";
import { connectionSecurityFacts, connectFacts, ENGINES } from "../../lib/engines";
import { t } from "../../lib/i18n";
import { LatestRequest } from "../../lib/requests";
import { loadConnections, openConnection } from "../../lib/store";
import { remember, rememberedChoice } from "../../lib/prefs";
import { SshFields } from "./SshFields";
import { sshConfig, type SshDraft } from "../../lib/ssh";
import { EnginePicker } from "./EnginePicker";

import { ConnectionSecurityFields } from "./ConnectionSecurityFields";
import { EnvironmentPicker } from "./EnvironmentPicker";
import { securityDraft } from "../../lib/connection-security";

type TestState = { status: "idle" } | { status: "busy" } | { status: "ok"; label: string } | { status: "error"; error: ApiError };

/** The engine picked last time, picked again on the next new connection. */
const ENGINE_KEY = "culprit.new.engine";

/**
 * The engine first, then what it needs: a connection string for a server, the file for SQLite. The pick settles what
 * the text leaves open (a bare host, an ADO.NET string); a text that names another engine (a postgres:// URL, a .db
 * path) switches the pick and says so. Before anything is picked, a pasted connection string picks its own engine.
 */
export function NewConnectionForm({ onDone, canCancel }: { onDone: () => void; canCancel: boolean }) {
  const [engine, setEngine] = useState<Engine | null>(() => {
    const last = rememberedChoice<Engine | "">(ENGINE_KEY, "", ENGINES);
    return last || null;
  });
  const [switched, setSwitched] = useState(false);
  const [input, setInput] = useState("");
  const [parsed, setParsed] = useState<ParsedConnection | null>(null);
  const parsedInput = useRef("");
  const parsedViaSsh = useRef(false);
  const [parseError, setParseError] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [nameTouched, setNameTouched] = useState(false);
  const [environment, setEnvironment] = useState<Environment>("development");
  const [environmentTouched, setEnvironmentTouched] = useState(false);
  const [user, setUser] = useState("");
  const [password, setPassword] = useState("");
  const [security, setSecurity] = useState<ConnectionSecurity | null>(null);
  const securityMode = useRef<HTMLSelectElement>(null);
  const testAbort = useRef<AbortController | null>(null);
  const [ssh, setSsh] = useState<SshDraft | null>(null);
  const form = useRef<HTMLFormElement>(null);
  const [test, setTest] = useState<TestState>({ status: "idle" });
  const [saving, setSaving] = useState(false);
  const field = useRef<HTMLInputElement>(null);
  const focusInput = useRef(engine !== null);
  const ids = useId();
  const facts = engine ? connectFacts(engine) : null;
  const viaSsh = ssh !== null && !!facts && !facts.file;
  const action = useRef(new LatestRequest());

  useEffect(() => {
    action.current.invalidate();
    testAbort.current?.abort();
    setTest({ status: "idle" });
    return () => { action.current.invalidate(); testAbort.current?.abort(); };
  }, [input, engine, name, environment, user, password, ssh, security]);

  useEffect(() => { setSecurity(null); }, [input, engine]);

  useEffect(() => {
    if (!input.trim()) {
      setParsed(null);
      setParseError(null);
      return;
    }
    let live = true;
    const handle = setTimeout(async () => {
      try {
        const p = await (await api()).parseConnection(input, engine, viaSsh);
        if (!live) return;
        // The text names another engine than the one picked (or none was): it's that engine's, read again as it.
        if (p.engine && p.engine !== engine) {
          focusInput.current = true;
          setSwitched(engine !== null);
          setEngine(p.engine);
          return;
        }
        parsedInput.current = input;
        parsedViaSsh.current = viaSsh;
        setParsed(p);
        setParseError(null);
        if (!nameTouched) setName(p.suggested_name);
        if (!environmentTouched) setEnvironment(p.environment);
      } catch (e) {
        if (!live) return;
        setParsed(null);
        setParseError(errorMessage(e).message);
      }
    }, 150);
    return () => {
      live = false;
      clearTimeout(handle);
    };
  }, [input, engine, viaSsh, nameTouched, environmentTouched]);

  // Restore the input on opening or after paste detection. Manual selection keeps focus in the picker for Tab.
  useEffect(() => {
    if (engine && focusInput.current) {
      field.current?.focus();
      focusInput.current = false;
    }
  }, [engine]);

  const pick = (next: Engine) => {
    if (next === engine) return;
    // A path and a connection string are different things: switching between a file and a server starts over.
    if (engine && connectFacts(engine).file !== connectFacts(next).file) setInput("");
    setEngine(next);
    setSwitched(false);
    setTest({ status: "idle" });
  };

  // Before an engine is picked, a pasted connection string picks its own.
  const onPaste = (e: React.ClipboardEvent) => {
    if (engine || e.target === field.current) return;
    const text = e.clipboardData.getData("text").trim();
    if (!text) return;
    e.preventDefault();
    setInput(text);
  };

  const onChoose = async () => {
    const current = action.current.start();
    try {
      const chosen = await (await api()).chooseDatabaseFile();
      if (current() && chosen) setInput(chosen);
    } catch (err) {
      if (current()) setParseError(errorMessage(err).message);
    }
  };

  // What was read is the picked engine's: the details and the buttons follow.
  const ready = !!parsed && !!engine && parsed.engine === engine && parsedInput.current === input;
  const defaultsCurrent = ready && parsedViaSsh.current === viaSsh;
  const needsUser = ready && parsed.auth === "password" && !parsed.user;
  const needsPassword = ready && parsed.auth === "password" && !parsed.has_password;

  const resolvedSecurity = security ?? securityDraft(parsed?.security, engine ? connectionSecurityFacts(engine) : null);

  const request = () => ({
    name,
    input,
    environment,
    engine,
    database: null,
    user: needsUser && user ? user : null,
    password: needsPassword && password ? password : null,
    password_env: null,
    security: resolvedSecurity,
    auth: null,
    ssh: facts?.file ? undefined : sshConfig(ssh) ?? undefined,
  });

  const startTest = () => {
    const current = action.current.start();
    testAbort.current?.abort();
    const controller = new AbortController();
    testAbort.current = controller;
    return { current, signal: controller.signal };
  };

  const onTest = async () => {
    if (!defaultsCurrent) return;
    if (!form.current?.reportValidity()) return;
    const { current, signal } = startTest();
    setTest({ status: "busy" });
    try {
      const server = await (await api()).testConnection(request(), signal);
      if (current()) setTest({ status: "ok", label: `${server.product} · ${server.database}` });
    } catch (e) {
      if (current()) setTest({ status: "error", error: errorMessage(e) });
    }
  };

  const onSave = async () => {
    if (!defaultsCurrent) return;
    if (!form.current?.reportValidity()) return;
    const { current, signal } = startTest();
    setTest({ status: "busy" });
    try {
      const candidate = request();
      const client = await api();
      if (!current()) return;
      await client.testConnection(candidate, signal);
      if (!current()) return;
      setSaving(true);
      const saved = await client.addConnection(candidate);
      if (engine) remember(ENGINE_KEY, engine);
      await loadConnections();
      if (!current()) return;
      onDone();
      openConnection(saved.name);
    } catch (e) {
      if (current()) setTest({ status: "error", error: errorMessage(e) });
    } finally { setSaving(false); }
  };

  return (
    <form
      ref={form}
      className="new-conn"
      onPaste={onPaste}
      onSubmit={(e) => {
        e.preventDefault();
        if (defaultsCurrent && name && test.status !== "busy") onSave();
      }}
      onKeyDown={(e) => {
        if (e.key === "Escape" && canCancel && !saving) onDone();
      }}
    >
      <fieldset className="new-conn-fields" disabled={saving}>
        <div className="eyebrow">{t("ui.connections.new")}</div>
        <div className="new-conn-connect">
          <div className="new-conn-field">
            <label htmlFor={`${ids}-engine`}>{t("ui.new_connection_form.engine")}</label>
            <EnginePicker id={`${ids}-engine`} value={engine} onChange={pick} autoFocus={!engine} />
          </div>
          <div className="new-conn-field">
            <label htmlFor={`${ids}-input`}>{facts?.file ? t("ui.new_connection_form.file") : t("ui.new_connection_form.connection_string")}</label>
            <div className="smart-input">
              <Database size={16} color="var(--text-3)" aria-hidden />
              <input ref={field} id={`${ids}-input`} value={input} spellCheck={false}
                autoComplete="off" autoCapitalize="off" placeholder={facts?.example ?? t("ui.new_connection_form.paste")}
                aria-describedby={`${ids}-help`}
                onChange={(event) => { setInput(event.target.value); setSwitched(false); setTest({ status: "idle" }); }} />
              {facts?.file && (
                <button type="button" className={`btn ${input.trim() ? "" : "primary"}`} onClick={onChoose}>
                  <FolderOpen size={13} /> {t("ui.new_connection_form.choose_file")}
                </button>
              )}
            </div>
          </div>
        </div>
        <p id={`${ids}-help`} className={`new-conn-help ${parseError ? "error" : ""}`}>
          {parseError ? parseError : ready && !facts?.file ? (
            <>
              <Check size={12} className="read" aria-hidden />
              {switched && <b>{t("ui.new_connection_form.switched", { engine: facts?.choice ?? "" })} </b>}
              <span className="mono">{parsed.display}</span>
              {parsed.auth === "entra" && ` · ${t("ui.new_connection_form.entra")}`}
            </>
          ) : (
            <>
              {switched && <b>{t("ui.new_connection_form.switched", { engine: facts?.choice ?? "" })} </b>}
              {facts ? t(facts.accepts) : t("ui.new_connection_form.accepts")}
            </>
          )}
        </p>

        {ready && (
          <div className="form-grid">
            <label htmlFor={`${ids}-name`}>{t("ui.new_connection_form.name")}</label>
            <input
              id={`${ids}-name`}
              value={name}
              spellCheck={false}
              onChange={(e) => {
                setName(e.target.value);
                setNameTouched(true);
              }}
            />
            <span id={`${ids}-environment`} className="form-grid-label">{t("ui.new_connection_form.environment")}</span>
            <EnvironmentPicker value={environment} labelledBy={`${ids}-environment`} onChange={(value) => { setEnvironment(value); setEnvironmentTouched(true); }} />
            {needsUser && (
              <>
                <label htmlFor={`${ids}-user`}>{t("ui.new_connection_form.user")}</label>
                <input id={`${ids}-user`} value={user} spellCheck={false} autoComplete="off" onChange={(e) => setUser(e.target.value)} placeholder={t("ui.new_connection_form.user_placeholder")} />
              </>
            )}
            {needsPassword && (
              <>
                <label htmlFor={`${ids}-password`}>{t("ui.new_connection_form.password")}</label>
                <input id={`${ids}-password`} type="password" value={password} onChange={(e) => setPassword(e.target.value)} placeholder={t("ui.new_connection_form.password_placeholder")} />
              </>
            )}
            <span />
            <ConnectionSecurityFields engine={engine} info={parsed.security} value={resolvedSecurity} source={security ? "custom" : parsed.security?.settings.mode ? "from_string" : "default"} modeRef={securityMode} onChange={setSecurity} onError={(error) => setTest({ status: "error", error })} />
            {!facts?.file && <><span /><SshFields value={ssh} disabled={test.status === "busy"} onChange={(value) => {
              action.current.invalidate(); setSsh(value); setTest({ status: "idle" });
            }} /></>}
            <span />
            <div className="new-conn-actions">
              <button type="submit" className="btn primary" disabled={!name || !defaultsCurrent || test.status === "busy"}>
                {t("ui.new_connection_form.connect")}
              </button>
              <button type="button" className="btn" disabled={!defaultsCurrent || test.status === "busy"} onClick={onTest}>
                {t("ui.new_connection_form.test")}
              </button>
              {canCancel && (
                <button type="button" className="btn ghost" onClick={onDone}>
                  {t("ui.common.cancel")}
                </button>
              )}
              {test.status === "busy" && <span className="spinner" />}
              {test.status === "ok" && <span className="tag good">✓ {test.label}</span>}
            </div>
            {test.status === "error" && (
              <>
                <span />
                <div className="notice error">
                  <div>
                    <b>{test.error.message}</b>
                    {test.error.hint && <div>{test.error.hint}</div>}
                    {test.error.detail === "connection-security" && <button type="button" className="btn small" onClick={() => securityMode.current?.focus()}>{t("ui.connection_security.configure")}</button>}
                  </div>
                </div>
              </>
            )}
            {environment === "production" && (
              <>
                <span />
                <div className="notice">{t("ui.new_connection_form.production_notice")}</div>
              </>
            )}
          </div>
        )}
      </fieldset>
    </form>
  );
}
