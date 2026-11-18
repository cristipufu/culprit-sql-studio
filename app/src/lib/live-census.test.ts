import { expect, test } from "bun:test";
import type { LiveState } from "../gen/LiveState";
import type { Session } from "../gen/Session";
import { sessionCensusLive } from "../test/session-census";
import { partial } from "../test/fixtures";
import { liveSessionPresentation } from "./live-census";

function singleCurrentState() {
  const state = sessionCensusLive("mars");
  state.sessions = state.sessions.slice(0, 1);
  const read = state.activity_census!;
  read.observations = read.observations.slice(0, 1);
  if (read.source.observation.kind === "activity") {
    const native = read.source.observation.data;
    native.observations = 1;
    native.native_rows = 1;
    native.coverage.records = 1;
  }
  return state;
}

test("current census inspection uses the backend decision even when the display looks active", () => {
  const state = singleCurrentState();
  const session = state.sessions[0]!;
  session.state = "running";
  session.progress_eligible = false;
  expect(liveSessionPresentation(state).canInspect(session)).toBe(false);
  session.progress_eligible = true;
  expect(liveSessionPresentation(state).canInspect(session)).toBe(true);
});

test("acquired census without a derived eligibility decision cannot dispatch inspection", () => {
  const state = singleCurrentState();
  state.sessions[0]!.progress_eligible = undefined;
  expect(liveSessionPresentation(state).canInspect(state.sessions[0]!)).toBe(false);
});

test("a current provider without acquired census retains independent session inspection", () => {
  const session = partial<Session>({ id: "42", state: "running" });
  const state = partial<LiveState>({ sessions: [session] });
  expect(liveSessionPresentation(state).canInspect(session)).toBe(true);
  session.state = "sleeping";
  expect(liveSessionPresentation(state).canInspect(session)).toBe(false);
});
