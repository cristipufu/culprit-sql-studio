import { FolderOpen, ShieldCheck, ShieldOff } from "lucide-react";
import { useEffect, useId, useRef, type RefObject } from "react";
import type { Engine } from "../../gen/Engine";
import type { ConnectionSecurity } from "../../gen/ConnectionSecurity";
import type { ConnectionSecurityInfo } from "../../gen/ConnectionSecurityInfo";
import { api, errorMessage, type ApiError } from "../../lib/api";
import { connectionSecurityFacts } from "../../lib/engines";
import { securityMode } from "../../lib/connection-security";
import { key, t } from "../../lib/i18n";
import { LatestRequest } from "../../lib/requests";
import "./connection-settings.css";

const SOURCE_KEYS = {
  default: key("ui.connection_security.provider_default"),
  from_string: key("ui.connection_security.from_string"),
  saved: key("ui.connection_security.saved"),
  custom: key("ui.connection_security.custom"),
};

const SUMMARY_KEYS = {
  may_be_unencrypted: key("ui.connection_security.may_be_unencrypted"),
  unencrypted: key("ui.connection_security.unencrypted"),
  verify_identity: key("ui.connection_security.verify_identity"),
  encryption_only: key("ui.connection_security.encryption_only"),
};

type Props = {
  engine: Engine;
  info?: ConnectionSecurityInfo | null;
  value: ConnectionSecurity;
  source: "default" | "from_string" | "saved" | "custom";
  disabled?: boolean;
  modeRef?: RefObject<HTMLSelectElement | null>;
  onChange: (settings: ConnectionSecurity) => void;
  onError: (error: ApiError) => void;
};

export function ConnectionSecurityFields({ engine, info, value, source, disabled = false, modeRef, onChange, onError }: Props) {
  const id = useId();
  const choosing = useRef(new LatestRequest());
  useEffect(() => () => choosing.current.invalidate(), []);
  const facts = connectionSecurityFacts(engine);
  if (!facts) return <div className="connection-file-security"><ShieldCheck size={17} aria-hidden />{t("ui.connection_security.sqlite")}</div>;
  const mode = securityMode(facts, info, value);
  const defaultMode = facts.modes.find((mode) => mode.value === (info?.default_mode ?? facts.default_mode));
  const requiresVerification = info?.requires_verified_tls ?? false;
  const caution = mode?.allows_plaintext || value.trust_server_certificate;
  const summary = mode?.allows_plaintext
    ? mode.certificate_options ? "may_be_unencrypted" : "unencrypted"
    : mode?.verifies_identity && !value.trust_server_certificate ? "verify_identity" : "encryption_only";
  const update = (changes: Partial<ConnectionSecurity>) => {
    choosing.current.invalidate();
    onChange({ ...value, ...changes });
  };
  const choose = async () => {
    const owns = choosing.current.start();
    try {
      const chosen = await (await api()).chooseCaCertificate();
      if (owns() && chosen) onChange({ ...value, ca_certificate: chosen });
    } catch (error) { if (owns()) onError(errorMessage(error)); }
  };
  return <section className={`connection-security${caution ? " caution" : ""}`} aria-labelledby={`${id}-title`}>
    <header className="connection-security-heading">
      {caution ? <ShieldOff size={19} aria-hidden /> : <ShieldCheck size={19} aria-hidden />}
      <div><h3 id={`${id}-title`}>{t("ui.connection_security.title")}</h3><p>{t("ui.connection_security.subtitle")}</p></div>
      <span className="connection-security-summary">{t(SUMMARY_KEYS[summary])}</span>
    </header>
    <div className="connection-security-body">
      <div className="connection-security-label"><label htmlFor={`${id}-mode`}>{t("ui.connection_security.tls_mode")}</label>{mode && <code>{facts.mode_option}={mode.value}</code>}</div>
      <select ref={modeRef} id={`${id}-mode`} value={value.mode ?? ""} disabled={disabled} onChange={(event) => {
        const next = facts.modes.find((mode) => mode.value === event.target.value);
        update({ mode: event.target.value || null, trust_server_certificate: next?.allows_trust_certificate ? value.trust_server_certificate : false });
      }}>
        <option value="">{t("ui.connection_security.default", { mode: defaultMode ? t(defaultMode.label) : "" })}</option>
        {value.mode && !mode && <option value={value.mode} disabled>{t("ui.connection_security.unknown", { mode: value.mode })}</option>}
        {facts.modes.map((mode) => <option key={mode.value} value={mode.value} disabled={requiresVerification && !mode.verifies_identity}>{t(mode.label)}</option>)}
      </select>
      {mode && <p className={`connection-security-description${mode.allows_plaintext ? " caution" : ""}`}>{t(mode.description)}</p>}
      {mode?.ca_verifies_certificate && value.ca_certificate && <p className="connection-security-hint">{t("ui.connection_security.postgres_ca")}</p>}
      <p className="connection-security-source">{t(SOURCE_KEYS[value.mode === null && source !== "custom" ? "default" : source])}</p>
      {requiresVerification && <p className="connection-security-auth">{t("ui.connection_security.required")}</p>}
      {mode?.certificate_options && <details className="connection-certificate-options">
        <summary>{t("ui.connection_security.certificate_options")}</summary>
        <div className="connection-certificate-fields">
          <label htmlFor={`${id}-ca`}>{t("ui.connection_security.ca_certificate")}</label>
          <div className="connection-certificate-file"><input id={`${id}-ca`} value={value.ca_certificate ?? ""} placeholder={t("ui.connection_security.system_trust")} spellCheck={false} disabled={disabled} onChange={(event) => update({ ca_certificate: event.target.value || null })} />
            <button type="button" className="btn" onClick={choose} disabled={disabled}><FolderOpen size={13} aria-hidden />{t("ui.connection_security.choose_file")}</button>
          </div>
          {facts.trust_certificate_option && <div className="connection-certificate-trust">
            <label><input type="checkbox" checked={value.trust_server_certificate} disabled={disabled || requiresVerification || !mode.allows_trust_certificate} onChange={(event) => update({ trust_server_certificate: event.target.checked })} />{t("ui.connection_security.trust")}</label>
            <p className="connection-security-hint">{value.trust_server_certificate ? t("ui.connection_security.trust_warning") : t("ui.connection_security.trust_hint")}</p>
          </div>}
          {facts.hostname_option && <div><label htmlFor={`${id}-hostname`}>{t("ui.connection_security.hostname")}</label>
            <input id={`${id}-hostname`} value={value.certificate_hostname ?? ""} placeholder={t("ui.connection_security.hostname_hint")} spellCheck={false} disabled={disabled} onChange={(event) => update({ certificate_hostname: event.target.value || null })} />
          </div>}
        </div>
      </details>}
    </div>
  </section>;
}
