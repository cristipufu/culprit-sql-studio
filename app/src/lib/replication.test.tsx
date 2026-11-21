import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ReplicationEvidence, ReplicationFields } from "../components/ReplicationEvidence";
import { Replication } from "../components/BeyondTime";
import { HealthTab } from "../components/Health";
import type { Replica } from "../gen/Replica";
import { replicationFields, replicationOnlyReport, replicationReport } from "../test/replication";
import { partial } from "../test/fixtures";
import { t } from "./i18n";

test("closed replication evidence mounts its compact shared summary without native field tables or all record headers", () => {
  const report = replicationReport("limited");
  const html = renderToStaticMarkup(<ReplicationEvidence report={report} />);
  expect(report.entries_limited).toBe(true);
  expect(html).toContain(report.title);
  expect(html).toContain(report.summary);
  expect(html).toContain(t("ui.replication.limited"));
  expect(html.match(/<details/g)).toHaveLength(1);
  expect(html).not.toContain("<dl");
  expect(html).not.toContain("replication-entry-title");
  expect(html).not.toMatch(/<details[^>]*\bopen(?:=|\s|>)/);
});

test("Health and the shared legacy panel retain unavailable native explanations with no legacy replicas or other signals", () => {
  for (const name of ["unavailable", "partial", "empty"]) {
    const view = replicationReport(name);
    const report = replicationOnlyReport(view);
    const health = renderToStaticMarkup(<HealthTab pressure={null} resources={[]} report={report} engineLabel="SQL Server" />);
    const panel = renderToStaticMarkup(<Replication replicas={[]} report={report} />);
    expect(health).toContain(view.title);
    expect(health).toContain(view.summary);
    expect(health).not.toContain(t("ui.health.empty"));
    expect(panel).toContain(view.title);
    expect(panel).toContain(view.summary);
    expect(health).not.toContain("sig-dot good");
  }
});

test("native MySQL positions and offset-free clocks render from Rust text without numeric or date conversion", () => {
  const view = replicationReport("mysql_channel");
  const fields = replicationFields(view);
  const received = fields.find((field) => field.key === "received-source-log-position");
  const clock = fields.find((field) => field.key === "io_error_at_without_offset");
  expect(received?.value).toBe("9007199254740993");
  expect(clock?.value).toMatch(/^\d{6} \d{2}:\d{2}:\d{2}$/);
  const html = renderToStaticMarkup(<ReplicationFields fields={fields} />);
  expect(html).toContain(received!.value!);
  expect(html).toContain(clock!.value!);
  expect(html).not.toContain(`${clock!.value!}Z`);
  expect(html).toContain(t("ui.memory_failures.not_reported"));
});

test("unknown and empty native fields remain different while hostile database text stays text with no links", () => {
  const html = renderToStaticMarkup(<ReplicationFields fields={[
    { key: "missing", label: "Missing native field", value: null },
    { key: "empty", label: "Empty native field", value: "" },
    { key: "hostile", label: "<img src=x onerror=alert(1)>", value: "<script>private_canary()</script> javascript:alert(1) https://example.invalid" },
  ]} />);
  expect(html).toContain(t("ui.memory_failures.not_reported"));
  expect(html).toContain(t("ui.replication.empty_value"));
  expect(html).toContain("&lt;script&gt;private_canary()&lt;/script&gt;");
  expect(html).toContain("&lt;img src=x onerror=alert(1)&gt;");
  expect(html).toContain("javascript:alert(1)");
  expect(html).not.toContain("<script");
  expect(html).not.toContain("<img");
  expect(html).not.toContain("<a ");
});

test("the existing replica summary remains beside collapsed native detail", () => {
  const view = replicationReport("mssql_availability_group");
  const report = replicationOnlyReport(view);
  const legacy = partial<Replica>({ name: "Existing replica summary", role: "replica", condition: "reported", state: "Native state", lag_seconds: null, lag_bytes: null, unsent_bytes: null });
  const html = renderToStaticMarkup(<Replication replicas={[legacy]} report={report} />);
  expect(html).toContain("Existing replica summary");
  expect(html).toContain("Native state");
  expect(html).toContain(view.title);
  expect(html).not.toContain("replication-fields");
});


test("Hyperscale topology retains all native roles and nullable parent meaning without a health badge", () => {
  const view = replicationReport("mssql_hyperscale");
  expect(view.state_rows).toBe(0);
  expect(view.sources.every((source) => source.state === "partial")).toBe(true);
  const fields = replicationFields(view);
  expect(fields.filter((field) => field.key === "role_code").map((field) => field.value).sort()).toEqual(["0", "1", "2", "3"]);
  expect(fields.filter((field) => field.key === "parent_replica_id" && field.value != null)).toHaveLength(1);
  const html = renderToStaticMarkup(<HealthTab pressure={null} resources={[]} report={replicationOnlyReport(view)} engineLabel="SQL Server" />);
  expect(html).toContain(view.summary);
  expect(html).not.toContain("sig-dot good");
  const nulls = replicationReport("mssql_hyperscale_nulls");
  expect(replicationFields(nulls).filter((field) => field.key === "parent_replica_id").every((field) => field.value == null)).toBe(true);
  const native = renderToStaticMarkup(<ReplicationFields fields={replicationFields(nulls)} />);
  expect(native).toContain(t("ui.memory_failures.not_reported"));
});
