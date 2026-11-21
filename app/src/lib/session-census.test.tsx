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

test("session and request databases render separately without filling unknown names from the selected database", () => {
  for (const [name, expected] of [
    ["mars", ["4", "session-default", "7"]],
    ["dormant", ["0", null, null]],
    ["unknown", [null, null, null]],
  ] as const) {
    const { report } = sessionCensusCase(name);
    const html = renderToStaticMarkup(<SessionCensus report={report} />);
    for (const [key, value] of [
      ["session_database_id", expected[0]],
      ["session_database_name", expected[1]],
      ["request_database_id", expected[2]],
    ] as const) {
      const field = report.entries[0]!.fields.find((field) => field.key === key)!;
      expect(field.value).toBe(value);
      expect(html).toContain(field.label);
      expect(html).toContain(field.value ?? t("ui.memory_failures.not_reported"));
    }
  }
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

test("observed client groups share Rust session deduplication and exact session-age totals in a folded disclosure", () => {
  const { report } = sessionCensusCase("client-groups");
  const group = report.client_groups.entries[0]!;
  expect(report.client_groups.grouped_sessions).toBe(2);
  expect(group.observed_sessions).toBe(2);
  expect(group.observed_requests).toBe(3);
  expect(group.fields.find((field) => field.key === "session_age_ms")!.value).toBe("900000");
  expect(group.fields.find((field) => field.key === "session_logical_reads")!.value).toBe("9007199254741000");
  const html = renderToStaticMarkup(<SessionCensus report={report} />);
  expect(html).toContain(report.client_groups.title);
  expect(html).toContain(report.client_groups.summary);
  expect(html).toContain("9007199254741000");
  for (const limitation of report.client_groups.limitations) expect(html).toContain(limitation);
  expect(html).not.toContain("<details open");
});

test("conflicting MARS sessions and unknown totals remain explicit alongside the unchanged native population", () => {
  for (const name of ["group-conflict", "group-unknown"] as const) {
    const { report } = sessionCensusCase(name);
    const groups = report.client_groups;
    const html = renderToStaticMarkup(<SessionCensus report={report} />);
    expect(html).toContain(groups.summary);
    expect(report.entries.length).toBe(2);
    if (name === "group-conflict") {
      expect(groups.ungrouped_sessions).toBe(1);
      expect(groups.entries.length).toBe(0);
    } else {
      expect(groups.grouped_sessions).toBe(2);
      expect(groups.entries[0]!.fields.find((field) => field.key === "session_age_ms")!.value).toBeNull();
      expect(groups.entries[0]!.fields.find((field) => field.key === "session_writes")!.value).toBe("0");
      expect(html).toContain(t("ui.memory_failures.not_reported"));
    }
  }
});

test("client-group labels remain text-only and do not create a SQL, session or owner action", () => {
  const { report } = sessionCensusCase("client-groups");
  report.client_groups.entries[0]!.fields.find((field) => field.key === "application")!.value = "<img src=x onerror=alert(1)>";
  const html = renderToStaticMarkup(<SessionCensus report={report} />);
  expect(html).toContain("&lt;img");
  expect(html).not.toContain("<img");
  expect(html).not.toContain("<button");
  expect(html).not.toContain("sig-dot");
});

test("negative native session elapsed remains a point value while only its grouped age is unavailable", () => {
  const { report } = sessionCensusCase("signed-elapsed");
  const group = report.client_groups.entries[0]!;
  expect(group.observed_sessions).toBe(2);
  expect(group.observed_requests).toBe(3);
  expect(group.fields.find((field) => field.key === "session_age_ms")!.value).toBeNull();
  expect(group.fields.find((field) => field.key === "session_logical_reads")!.value).toBe("9007199254741000");
  const html = renderToStaticMarkup(<SessionCensus report={report} />);
  expect(html).toContain(t("view.session_census.groups.limit.elapsed"));
  expect(html).toContain(">-1<");
});

test("native backend groups retain UTC start while mutable default schema groups retain unknown lifetime", () => {
  for (const name of ["postgres-shared", "mysql-shared", "mysql-switched"] as const) {
    const { report } = sessionCensusCase(name);
    const group = report.client_groups.entries[0]!;
    const html = renderToStaticMarkup(<SessionCensus report={report} />);
    expect(html).toContain(report.client_groups.summary);
    expect(group.fields.find((field) => field.key === "session_age_ms")!.value).toBeNull();
    expect(group.fields.find((field) => field.key === "session_logical_reads")!.value).toBeNull();
    if (name === "postgres-shared") {
      expect(group.key.database.kind).toBe("backend");
      expect(group.fields.find((field) => field.key === "earliest_backend_start_utc")!.value?.endsWith("Z")).toBe(true);
    } else {
      expect(group.key.database.kind).toBe("current-default");
      expect(group.fields.find((field) => field.key === "session_database_id")!.value).toBeNull();
      expect(group.fields.find((field) => field.key === "session_database_name")!.value).toBe(name === "mysql-switched" ? "other" : "db");
    }
    for (const limitation of group.limitations) expect(html).toContain(limitation);
  }
});

test("native worker participants keep raw evidence without entering client groups from shared labels", () => {
  for (const name of ["postgres-worker", "mysql-worker"] as const) {
    const { report } = sessionCensusCase(name);
    expect(report.client_groups.ungrouped_sessions).toBe(1);
    expect(report.entries.length).toBe(name === "postgres-worker" ? 3 : 2);
    const html = renderToStaticMarkup(<SessionCensus report={report} />);
    expect(html).toContain(report.client_groups.summary);
    expect(html).toContain(name === "postgres-worker" ? "parallel worker" : "thread/sql/slave_sql");
    expect(html).not.toContain("sig-dot");
  }
});
