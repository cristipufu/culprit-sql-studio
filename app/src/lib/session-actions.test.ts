import { expect, test } from "bun:test";
import type { Session } from "../gen/Session";
import type { SessionAction } from "../gen/SessionAction";
import type { SessionActionRequest } from "../gen/SessionActionRequest";
import { blockingEvidence } from "../test/blocking";
import { sessionActionRoute } from "../test/session-actions";
import { sessionActionCandidate, sessionActionIsCurrent, sessionActionKey } from "./session-actions";

const actions: SessionAction[] = ["cancel", "terminate"];
const connection = "pg-demo";

function session(): Session {
  return blockingEvidence().sessions[0]!;
}

function pending(value: Session, action: SessionAction = "cancel"): SessionActionRequest {
  return structuredClone(sessionActionCandidate(connection, sessionActionRoute(), value, actions, action)!);
}

test("only a source target, exact route and offered action can create a candidate", () => {
  const value = session();
  expect(sessionActionCandidate(connection, null, value, actions, "cancel")).toBeNull();
  expect(sessionActionCandidate(connection, sessionActionRoute("another-route"), value, actions, "cancel")).toBeNull();
  expect(sessionActionCandidate(connection, { connection, revision: "" }, value, actions, "cancel")).toBeNull();
  expect(sessionActionCandidate(connection, sessionActionRoute(), value, [], "cancel")).toBeNull();
  delete value.action_target;
  expect(sessionActionCandidate(connection, sessionActionRoute(), value, actions, "cancel")).toBeNull();
  expect(sessionActionCandidate(connection, sessionActionRoute(), value, actions, "terminate")).toBeNull();
});

test("unknown native source or actor identity stays unavailable despite a visible session ID and SQL", () => {
  const mutations: ((value: Session) => void)[] = [
    (value) => { value.action_target!.context.partition = { kind: "target" }; },
    (value) => { value.action_target!.context.partition = { kind: "instance-local", member: null, epoch: "known-epoch" }; },
    (value) => { const partition = value.action_target!.context.partition; if (partition.kind === "instance-local") partition.epoch = null; },
    (value) => { value.action_target!.activity.session.generation = ""; },
    (value) => { value.action_target!.activity.session.id = "a-different-native-actor"; },
  ];
  for (const mutate of mutations) {
    const value = session();
    mutate(value);
    for (const action of actions) expect(sessionActionCandidate(connection, sessionActionRoute(), value, actions, action)).toBeNull();
  }
});

test("read clocks, counter updates, display state and qualifier insertion order do not change native action identity", () => {
  const value = session();
  const captured = pending(value);
  value.sql = "SELECT a_later_display_copy";
  value.application = "a-different-displayed-program";
  value.state = "another displayed state";
  value.cpu_ms = 999;
  value.reads = 999999;
  value.elapsed_ms = 20000;
  value.action_target!.context.observed_from = "2026-11-15T10:01:00Z";
  value.action_target!.context.observed_until = "2026-11-15T10:01:01Z";
  value.action_target!.statement!.qualifiers = { scope: "request", native: "fixture" };
  const partition = value.action_target!.context.partition;
  if (partition.kind === "instance-local") partition.member!.namespace.qualifiers = { boundary: "test", cluster: "fixture" };
  const current = sessionActionCandidate(connection, sessionActionRoute(), value, actions, "cancel")!;
  expect(sessionActionKey(current)).toBe(sessionActionKey(captured));
  expect(sessionActionIsCurrent(captured, connection, sessionActionRoute(), value, actions)).toBe(true);
});

test("changed native session, source member, epoch, role or database rejects the pending request", () => {
  const mutations: ((value: Session) => void)[] = [
    (value) => { value.action_target!.activity.session.generation = "reused-native-session"; },
    (value) => { const partition = value.action_target!.context.partition; if (partition.kind === "instance-local") partition.member!.native.id = "other-native-member"; },
    (value) => { const partition = value.action_target!.context.partition; if (partition.kind === "instance-local") partition.epoch = "restarted-native-process"; },
    (value) => { value.action_target!.context.database_writable = false; },
    (value) => { value.action_target!.context.database_writable = null; },
    (value) => { const target = value.action_target!.context.target; if (target.scope === "database") target.identity.incarnation = "other-native-database"; },
    (value) => { const target = value.action_target!.context.target; if (target.scope === "database" && target.identity.location.kind === "server") target.identity.location.host = "other.fixture.invalid"; },
  ];
  for (const action of actions) {
    for (const mutate of mutations) {
      const value = session();
      const captured = pending(value, action);
      mutate(value);
      expect(sessionActionIsCurrent(captured, connection, sessionActionRoute(), value, actions)).toBe(false);
    }
  }
});

test("cancel binds the original request and optional statement while termination remains whole-session", () => {
  for (const change of ["request", "statement", "idle"] as const) {
    const value = session();
    const cancel = pending(value, "cancel");
    const terminate = pending(value, "terminate");
    if (change === "request") value.action_target!.activity.request!.generation = "next-native-request";
    if (change === "statement") value.action_target!.statement!.id = "next-native-statement";
    if (change === "idle") {
      value.action_target!.activity.request = null;
      value.action_target!.statement = null;
    }
    expect(sessionActionIsCurrent(cancel, connection, sessionActionRoute(), value, actions)).toBe(false);
    expect(sessionActionIsCurrent(terminate, connection, sessionActionRoute(), value, actions)).toBe(true);
  }
  const value = session();
  value.action_target!.statement = null;
  expect(sessionActionCandidate(connection, sessionActionRoute(), value, actions, "cancel")).not.toBeNull();
});

test("route changes and retired actions cannot reuse a frozen request", () => {
  const value = session();
  const captured = pending(value);
  expect(sessionActionIsCurrent(captured, "other-connection", sessionActionRoute("other-connection"), value, actions)).toBe(false);
  expect(sessionActionIsCurrent(captured, connection, { connection, revision: "b".repeat(64) }, value, actions)).toBe(false);
  expect(sessionActionIsCurrent(captured, connection, sessionActionRoute(), value, ["terminate"])).toBe(false);
  expect(captured.route).toEqual(sessionActionRoute());
  expect(captured.target.activity.request!.generation).toBe("native-request-generation-30");
});

test("unknown updateability stays unknown and never supplies a missing actor or source identity", () => {
  const value = session();
  value.action_target!.context.database_writable = null;
  const captured = pending(value);
  expect(sessionActionIsCurrent(captured, connection, sessionActionRoute(), value, actions)).toBe(true);
  expect(captured.target.context.database_writable).toBeNull();
  value.action_target!.context.partition = { kind: "target" };
  expect(sessionActionCandidate(connection, sessionActionRoute(), value, actions, "cancel")).toBeNull();
});
