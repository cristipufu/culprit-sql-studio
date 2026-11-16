import type { BlockNode } from "../gen/BlockNode";
import type { LiveState } from "../gen/LiveState";
import type { Session } from "../gen/Session";
import { sessionActionRoute, sessionActionTarget } from "./session-actions";

function session(id: string, blockedBy: string | null): Session {
  return {
    id,
    user: `native-login-${id}`,
    application: `native-program-${id}`,
    host: `native-host-${id}`,
    database: "native-database",
    state: "active",
    command: "SELECT",
    wait: blockedBy ? "Lock: transactionid" : null,
    wait_ms: blockedBy ? 5000 : null,
    blocked_by: blockedBy,
    elapsed_ms: 10000,
    transaction_ms: 15000,
    cpu_ms: 20,
    reads: 100,
    sql: `SELECT original_request_${id}`,
    waiting_on_lock: blockedBy !== null,
    action_target: sessionActionTarget(id),
  };
}

function node(context: Session, children: BlockNode[] = []): BlockNode {
  return { key: context.id, session: context.id, context: structuredClone(context), lock: null, waits_for: null, children };
}

/** A renderer fixture with separately owned raw sessions and already-qualified node contexts. */
export function blockingEvidence(): LiveState {
  const head = session("30", null);
  const middle = session("20", "30");
  const waiter = session("10", "20");
  return {
    taken_at: "2026-11-15T10:00:00Z",
    notes: [],
    waits: [],
    sessions: [head, middle, waiter],
    session_actions: ["cancel", "terminate"],
    session_actions_note: null,
    session_action_route: sessionActionRoute(),
    blocking: {
      waiting: 2,
      chains: [{
        head: head.id,
        head_kind: "running",
        head_ms: 10000,
        transaction_ms: 15000,
        waiters: 2,
        longest_wait_ms: 5000,
        wait_type: "Lock: transactionid",
        lock: null,
        headline: "Native blocking chain",
        advice: "Native blocking advice",
        tree: node(head, [node(middle, [node(waiter)])]),
      }],
    },
  };
}
