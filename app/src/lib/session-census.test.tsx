import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { SessionCensus } from "../components/SessionCensus";
import { sessionCensusCase, sessionCensusLive } from "../test/session-census";
import { liveSessionPresentation, sessionPresentationKey } from "./live-census";
import { t } from "./i18n";

test("the shared Rust census renders exact totals, measured zero and unavailable fields with folded raw context", () => {
  const { report } = sessionCensusCase();
  const html = renderToStaticMarkup(<SessionCensus report={report} />);
  expect(html).toContain("9007199254740993");
  expect(html).toContain(">0<");
  expect(html).toContain(t("ui.memory_failures.not_reported"));
  expect(html).toContain("2020-01-01T11:59:54.0000000");
  expect(html).toContain("2020-01-01T11:59:55.0000000");
  expect(html).not.toContain("<details open");
  expect(html).not.toContain("sig-dot");
});

test("MARS rows have separate presentation keys and never select a representative request for the session-only progress API", () => {
  const state = sessionCensusLive("mars");
  const [first, second] = state.sessions;
  expect(first!.id).toBe(second!.id);
  expect(sessionPresentationKey(first!)).not.toBe(sessionPresentationKey(second!));
  expect(liveSessionPresentation(state).canInspect(first!)).toBe(false);
  expect(liveSessionPresentation(state).canInspect(second!)).toBe(false);
  expect(liveSessionPresentation(state).active).toBe(1);
  const html = renderToStaticMarkup(<SessionCensus report={state.session_census!} />);
  expect(html).toContain("11111111-1111-1111-1111-111111111111");
  expect(html).toContain("22222222-2222-2222-2222-222222222222");
});

test("idle and dormant sessions stay visible as context without active counts or progress dispatch", () => {
  for (const name of ["idle", "dormant"] as const) {
    const state = sessionCensusLive(name);
    expect(liveSessionPresentation(state).isIdle(state.sessions[0]!)).toBe(true);
    expect(liveSessionPresentation(state).canInspect(state.sessions[0]!)).toBe(false);
    expect(liveSessionPresentation(state).active).toBe(0);
  }
});

test("partial or unavailable census remains an explicit gap and native client text stays escaped", () => {
  const { report } = sessionCensusCase("partial");
  report.entries[0]!.fields.find((field) => field.key === "library")!.value = "<img src=x onerror=alert(1)>";
  const html = renderToStaticMarkup(<SessionCensus report={report} />);
  expect(html).toContain(t("ui.common.read_partial"));
  expect(html).toContain("&lt;img");
  expect(html).not.toContain("<img");
  const unavailable = renderToStaticMarkup(<SessionCensus report={sessionCensusCase("unavailable").report} />);
  expect(unavailable).toContain(t("ui.dashboard.status.unavailable"));
  expect(unavailable).not.toContain("<article");
});
