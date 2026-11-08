import { KeyRound } from "lucide-react";
import { useState } from "react";
import type { ConnectionInfo } from "../gen/ConnectionInfo";
import { api, errorMessage } from "../lib/api";
import { t } from "../lib/i18n";
import { loadConnections, scan } from "../lib/store";

/** The password inside a pasted connection URL (postgres://user:secret@host/db), or the text itself. */
function passwordFrom(input: string): string {
  const text = input.trim();
  if (text.includes("://")) {
    try {
      const url = new URL(text);
      if (url.password) return decodeURIComponent(url.password);
    } catch {}
  }
  const ado = text.match(/(?:^|;)\s*(?:password|pwd)\s*=\s*([^;]*)/i);
  return ado ? ado[1]!.trim() : text;
}

/**
 * Shown when a scan was refused for want of a password: ask for it right here, keep it in the OS keychain, scan again.
 * A pasted connection string works too; only its password is taken.
 */
export function PasswordNeeded({ connection, message }: { connection: ConnectionInfo; message: string }) {
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const save = async () => {
    const password = passwordFrom(value);
    if (!password) return;
    setBusy(true);
    setError(null);
    try {
      await (await api()).setPassword(connection.name, password);
      setValue("");
      await loadConnections();
      await scan(connection.name);
    } catch (e) {
      setError(errorMessage(e).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="empty">
      <div className="password-needed">
        <KeyRound size={18} />
        <h3>{t("ui.password_needed.title", { connection: connection.name })}</h3>
        <p className="muted">
          {connection.user != null
            ? t("ui.password_needed.explain", { user: connection.user, host: connection.host })
            : t("ui.password_needed.explain_saved_user", { host: connection.host })}
        </p>
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void save();
          }}
        >
          <input type="password" autoFocus value={value} placeholder={t("ui.password_needed.placeholder")} onChange={(e) => setValue(e.target.value)} autoComplete="off" spellCheck={false} />
          <button className="btn primary" type="submit" disabled={!value.trim() || busy}>
            {busy ? t("ui.password_needed.signing_in") : t("ui.password_needed.save_and_scan")}
          </button>
        </form>
        {(error ?? message) && <p className="muted small">{error ?? message}</p>}
      </div>
    </div>
  );
}
