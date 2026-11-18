import type { ActivityKey } from "../gen/ActivityKey";
import type { LiveState } from "../gen/LiveState";
import type { Session } from "../gen/Session";

/** Presentation equality only; native generations and action authentication remain backend-owned. */
export function activityPresentationKey(key: ActivityKey): string {
  return JSON.stringify([key.session.id, key.session.generation, key.request?.id, key.request?.generation, key.transaction]);
}
export function sessionPresentationKey(session: Session): string {
  return session.activity_key ? activityPresentationKey(session.activity_key) : session.id;
}

/** One linear projection joins display rows to original typed idle facts and keeps MARS requests separate. */
export function liveSessionPresentation(state: LiveState | null) {
  const idleByKey = new Map(state?.activity_census?.observations.map((row) => [activityPresentationKey(row.key), row.idle]));
  const rowsBySession = new Map<string, number>();
  for (const row of state?.sessions ?? []) rowsBySession.set(row.id, (rowsBySession.get(row.id) ?? 0) + 1);
  const isIdle = (session: Session): boolean => session.activity_key
    ? idleByKey.get(activityPresentationKey(session.activity_key)) ?? false
    : session.state === "idle" || session.state === "sleeping";
  // The existing progress endpoint accepts only a session ID, so an ambiguous current request stays unselected.
  const canInspect = (session: Session): boolean => state?.activity_census
    ? session.progress_eligible === true
    : !isIdle(session) && (!session.activity_key || (
      session.activity_key.request != null && rowsBySession.get(session.id) === 1
    ));
  const active = state?.session_census?.observed_active_sessions
    ?? new Set(state?.sessions.filter((row) => !isIdle(row)).map((row) => row.id)).size;
  const blocked = state?.session_census?.observed_blocked_sessions
    ?? new Set(state?.sessions.filter((row) => row.blocked_by).map((row) => row.id)).size;
  return { isIdle, canInspect, active, blocked };
}
