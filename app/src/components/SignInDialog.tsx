import { Copy, ExternalLink } from "lucide-react";
import { useEffect, useState } from "react";
import type { DeviceCodePrompt } from "../gen/DeviceCodePrompt";
import { api } from "../lib/api";
import { t } from "../lib/i18n";
import { toast } from "../lib/store";
import { copyAndTell } from "../lib/clipboard";

type Dialog = { status: "closed" } | { status: "code"; prompt: DeviceCodePrompt; until: number } | { status: "failed"; message: string };

function remaining(until: number): string {
  const seconds = Math.max(0, Math.round((until - Date.now()) / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

/** Microsoft Entra ID device-code sign-in, shown when a connection needs a token and the Azure CLI can't provide one. */
export function SignInDialog() {
  const [dialog, setDialog] = useState<Dialog>({ status: "closed" });
  const [, tick] = useState(0);

  useEffect(() => {
    let unsubscribe: (() => void) | undefined;
    let gone = false;
    api()
      .then((client) =>
        client.onSignIn((event) => {
          if (event.state === "code") setDialog({ status: "code", prompt: event.prompt, until: Date.now() + event.prompt.expires_in_secs * 1000 });
          if (event.state === "done") {
            setDialog({ status: "closed" });
            toast(t("ui.sign_in_dialog.signed_in"));
          }
          if (event.state === "failed") setDialog((d) => (d.status === "code" ? { status: "failed", message: event.message } : d));
        }),
      )
      .then((off) => {
        if (gone) off();
        else unsubscribe = off;
      });
    return () => {
      gone = true;
      unsubscribe?.();
    };
  }, []);

  useEffect(() => {
    if (dialog.status !== "code") return;
    const timer = setInterval(() => tick((n) => n + 1), 1000);
    return () => clearInterval(timer);
  }, [dialog.status]);

  if (dialog.status === "closed") return null;
  const cancel = async () => {
    setDialog({ status: "closed" });
    await (await api()).cancelSignIn();
  };

  return (
    <div className="overlay" onKeyDown={(e) => e.key === "Escape" && cancel()}>
      <div className="dialog" role="dialog" aria-modal="true" aria-labelledby="sign-in-title">
        <h2 id="sign-in-title">{t("ui.sign_in_dialog.title")}</h2>
        {dialog.status === "code" ? (
          <>
            <p>{t("ui.sign_in_dialog.enter_code")}</p>
            <div className="device-code">
              <span className="code">{dialog.prompt.user_code}</span>
              <button type="button" className="btn ghost" onClick={() => copyAndTell(dialog.prompt.user_code, t("ui.sign_in_dialog.code_copied"))}>
                <Copy size={13} /> {t("ui.common.copy")}
              </button>
            </div>
            <div className="actions">
              <button type="button" className="btn primary" autoFocus onClick={async () => (await api()).openSignInPage()}>
                <ExternalLink size={13} /> {t("ui.sign_in_dialog.open", { site: dialog.prompt.verification_uri.replace("https://", "") })}
              </button>
              <button type="button" className="btn ghost" onClick={cancel}>
                {t("ui.common.cancel")}
              </button>
            </div>
            <div className="waiting">
              <span className="spinner" /> {t("ui.sign_in_dialog.waiting", { time: remaining(dialog.until) })}
            </div>
          </>
        ) : (
          <>
            <p>{dialog.message}</p>
            <div className="actions">
              <button type="button" className="btn" autoFocus onClick={() => setDialog({ status: "closed" })}>
                {t("ui.common.close")}
              </button>
            </div>
          </>
        )}
      </div>
    </div>
  );
}
