import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { RequestMemoryParticipants } from "../components/MemoryParticipants";
import { requestMemoryCase } from "../test/memory-participants";
import { t } from "./i18n";
import { nativeIdentityKey } from "./native-identity";

function holder(name: Parameters<typeof requestMemoryCase>[0] = "qualified") {
  const value = requestMemoryCase(name);
  const raw = value.evidence.observations.find((row) => row.request?.wait?.native_type === "ASYNC_NETWORK_IO")!;
  const entry = value.report.entries.find((entry) => nativeIdentityKey(entry.observation_identity) === nativeIdentityKey(raw.identity))!;
  return { ...value, raw, entry };
}

test("the real shared report retains a qualified original request when its native resource is missing", () => {
  for (const name of ["qualified", "missing_resource", "shared_login"] as const) {
    const { evidence, report, raw, entry } = holder(name);
    expect(entry.qualified_request).toEqual(raw.request!.key);
    expect(entry.mechanism).toBe("reservation-during-client-output-wait");
    if (name === "missing_resource") expect(entry.grant_admission).toBeNull();
    const html = renderToStaticMarkup(<RequestMemoryParticipants evidence={evidence} report={report} />);
    expect(html).toContain("SELECT payload");
    expect(html).toContain("9007199254740993 KiB");
    expect(html).toContain("ASYNC_NETWORK_IO");
    expect(html).toContain(raw.observed_at);
    expect(html).not.toContain("<details open");
  }
});

test("a negative MARS peer or missing native authority preserves raw rows while withholding that request's SQL and context", () => {
  for (const name of ["negative_mars_peer", "unknown_owner", "missing_statement_identity"] as const) {
    const { evidence, report, raw, entry } = holder(name);
    expect(entry.qualified_request).toBeNull();
    expect(entry.mechanism).toBeUndefined();
    const html = renderToStaticMarkup(<RequestMemoryParticipants evidence={{ ...evidence, observations: [raw] }} report={{ ...report, entries: [entry] }} />);
    expect(html).toContain("9007199254740993 KiB");
    expect(html).toContain("ASYNC_NETWORK_IO");
    expect(html).toContain(raw.observed_at);
    expect(html).not.toContain("SELECT payload");
    expect(html).not.toContain("application-worker");
    expect(html).not.toContain("login-1");
    expect(html).toContain(t("view.request_memory.request.unqualified"));
  }
  const { evidence, report } = requestMemoryCase("negative_mars_peer");
  expect(evidence.observations).toHaveLength(3);
  expect(report.entries).toHaveLength(3);
  expect(report.entries.filter((entry) => entry.qualified_request !== null)).toHaveLength(1);
});

test("unavailable SQL does not erase an independently qualified native request and output wait", () => {
  const { evidence, report, raw, entry } = holder("sql_unavailable");
  expect(entry.qualified_request).toEqual(raw.request!.key);
  expect(entry.mechanism).toBe("reservation-during-client-output-wait");
  const html = renderToStaticMarkup(<RequestMemoryParticipants evidence={evidence} report={report} />);
  expect(html).toContain("ASYNC_NETWORK_IO");
  expect(html).toContain("application-worker");
  expect(html).toContain(t("view.request_memory.request.statement_unavailable"));
  // Inspect only the holder: the independent native waiter still has its own current statement.
  const card = renderToStaticMarkup(<RequestMemoryParticipants evidence={{ ...evidence, observations: [raw] }} report={{ ...report, entries: [entry] }} />);
  expect(card).not.toContain("SELECT payload");
});

test("same-session and same-grant lookalikes cannot substitute a different original request generation", () => {
  for (const changed of ["session-generation", "request-id", "request-generation", "transaction"] as const) {
    const { evidence, report, raw, entry } = holder();
    const request = raw.request!;
    if (changed === "session-generation") request.key.session.generation = "foreign-login";
    else if (changed === "request-id") request.key.request!.id = "1";
    else if (changed === "request-generation") request.key.request!.generation = "foreign-request";
    else request.key.transaction = "foreign-transaction";
    const html = renderToStaticMarkup(<RequestMemoryParticipants evidence={{ ...evidence, observations: [raw] }} report={{ ...report, entries: [entry] }} />);
    expect(html).not.toContain("SELECT payload");
    expect(html).not.toContain("application-worker");
    expect(html).not.toContain("foreign-");
    expect(html).toContain("9007199254740993 KiB");
  }
});
