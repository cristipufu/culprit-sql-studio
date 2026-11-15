import { useEffect, useRef, useState } from "react";
import type { Session } from "../gen/Session";
import type { SessionAction } from "../gen/SessionAction";
import type { SessionActionOutcome } from "../gen/SessionActionOutcome";
import type { SessionActionRequest } from "../gen/SessionActionRequest";
import type { SessionActionRoute } from "../gen/SessionActionRoute";
import { errorMessage } from "../lib/api";
import { t } from "../lib/i18n";
import { sessionActionCandidate, sessionActionIsCurrent } from "../lib/session-actions";
import { toast } from "../lib/store";

type Props = {
  connection: string;
  route: SessionActionRoute | null;
  session: Session;
  actions: readonly SessionAction[];
  variant: "row" | "blocker";
  execute: (request: SessionActionRequest) => Promise<SessionActionOutcome>;
};

type Confirmation = { request: SessionActionRequest; label: string };
type ActionState =
  | { status: "idle" }
  | { status: "confirm"; confirmation: Confirmation }
  | { status: "busy" }
  | { status: "retired" }
  | { status: "error"; message: string };

/** Both Live surfaces own the exact request the user confirmed; neither can retarget it from later props. */
export function SessionActionControls({ connection, route, session, actions, variant, execute }: Props) {
  const [state, setState] = useState<ActionState>({ status: "idle" });
  const inFlight = useRef(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);
  const current = (request: SessionActionRequest) => sessionActionIsCurrent(request, connection, route, session, actions);
  const retired = state.status === "confirm" && !current(state.confirmation.request);
  useEffect(() => {
    if (retired) setState({ status: "retired" });
  }, [retired]);

  const confirm = (action: SessionAction) => {
    const candidate = sessionActionCandidate(connection, route, session, actions, action);
    if (candidate) setState({ status: "confirm", confirmation: { request: structuredClone(candidate), label: session.id } });
  };
  const run = async (confirmation: Confirmation) => {
    if (inFlight.current) return;
    if (!current(confirmation.request)) {
      setState({ status: "retired" });
      return;
    }
    inFlight.current = true;
    setState({ status: "busy" });
    try {
      const outcome = await execute(confirmation.request);
      if (!mounted.current) return;
      if (outcome === "signal-sent") toast(t("ui.session_actions.signal_sent", { session: confirmation.label, connection: confirmation.request.route.connection }));
      setState({ status: "idle" });
    } catch (error) {
      if (mounted.current) setState({ status: "error", message: errorMessage(error).message });
    } finally {
      inFlight.current = false;
    }
  };

  if (state.status === "busy") return <span className="spinner" role="status" aria-label={t("ui.common.working")} />;
  if (state.status === "error" || state.status === "retired" || retired) return (
    <span className="inline-confirm">
      <span className={state.status === "error" ? "error-text" : "muted"}>{state.status === "error" ? state.message : t("ui.session_actions.changed")}</span>
      <button type="button" className="btn ghost small" onClick={() => setState({ status: "idle" })}>{t("ui.common.ok")}</button>
    </span>
  );
  if (state.status === "confirm") {
    const { confirmation } = state;
    const words = actionCopy(variant, confirmation.request.action, confirmation.label);
    return (
      <span className="inline-confirm session-action-confirm" onKeyDown={(event) => event.key === "Escape" && setState({ status: "idle" })}>
        <span className="muted">{words.explain} {t("ui.session_actions.prechecked")}</span>
        <span className="inline-confirm">
          <button type="button" className="btn danger small" autoFocus onClick={() => run(confirmation)}>{words.confirm}</button>
          <button type="button" className="btn ghost small" onClick={() => setState({ status: "idle" })}>{variant === "blocker" ? t("ui.blocking_chains.keep") : t("ui.live.keep")}</button>
        </span>
      </span>
    );
  }
  return (
    <span className={variant === "blocker" ? "block-actions" : "session-actions"}>
      {actions.filter((action) => sessionActionCandidate(connection, route, session, actions, action) !== null).map((action) => {
        const words = actionCopy(variant, action, session.id);
        return <button type="button" key={action} className="btn ghost small" title={words.explain} onClick={() => confirm(action)}>{words.button}</button>;
      })}
    </span>
  );
}

function actionCopy(variant: Props["variant"], action: SessionAction, session: string): { button: string; confirm: string; explain: string } {
  if (variant === "blocker") {
    return action === "cancel"
      ? { button: t("ui.blocking_chains.cancel.button"), confirm: t("ui.blocking_chains.cancel.confirm", { session }), explain: t("ui.blocking_chains.cancel.explain") }
      : { button: t("ui.blocking_chains.terminate.button"), confirm: t("ui.blocking_chains.terminate.confirm", { session }), explain: t("ui.blocking_chains.terminate.explain") };
  }
  return action === "cancel"
    ? { button: t("ui.common.cancel"), confirm: t("ui.live.action.cancel_confirm", { session }), explain: t("ui.live.action.cancel_explain") }
    : { button: t("ui.live.action.end"), confirm: t("ui.live.action.end_confirm", { session }), explain: t("ui.live.action.end_explain") };
}
