import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { InstanceResources } from "../components/InstanceResources";
import { HealthTab } from "../components/Health";
import { instanceResourceCase, instanceResourceHealthReport } from "../test/instance-resources";
import { t } from "./i18n";

test("Rust shared points retain exact large values, measured zero and unreported fields without pressure grades", () => {
  const html = renderToStaticMarkup(<InstanceResources report={instanceResourceCase().report} />);
  expect(html).toContain("9007199254740993");
  expect(html).toContain(">0<");
  expect(html).toContain(t("ui.memory_failures.not_reported"));
  expect(html).not.toContain("<details open");
  expect(html).not.toContain("sig-dot");
});

test("native partial and denied points remain distinct, and public field values render as escaped text", () => {
  const { report } = instanceResourceCase("truncated");
  report.groups[0]!.fields[0]!.value = "<img src=x onerror=alert(1)>";
  const partial = renderToStaticMarkup(<InstanceResources report={report} />);
  expect(partial).toContain(t("ui.common.read_partial"));
  expect(partial).toContain("&lt;img");
  expect(partial).not.toContain("<img");
  const denied = renderToStaticMarkup(<InstanceResources report={instanceResourceCase("denied").report} />);
  expect(denied).toContain(t("ui.dashboard.status.unavailable"));
  expect(denied).not.toContain("<article");
});

test("the actual Health path shows shared evidence or a withheld-analysis gap without engine-name interpretation", () => {
  const data = instanceResourceHealthReport("pressure");
  const render = () => renderToStaticMarkup(<HealthTab report={data} pressure={data.snapshot.pressure} resources={[]} engineLabel="PostgreSQL" />);
  expect(render()).toContain(t("view.instance_resources.title"));
  expect(render()).not.toContain(t("ui.health.empty"));
  data.instance_resources = undefined;
  expect(render()).toContain(t("view.instance_resources.invalid"));
  expect(render()).not.toContain(t("ui.health.empty"));
});

test("processor CPU disclosure preserves the canonical separate clock, exact totals and unknown state", () => {
  const { report } = instanceResourceCase("cpu_exact");
  const cpu = report.process_cpu!;
  const html = renderToStaticMarkup(<InstanceResources report={report} />);
  expect(html).toContain(cpu.title);
  expect(html).toContain("2020-01-01T12:00:00.5Z");
  expect(html).toContain("9007199254740993");
  expect(html).toContain(cpu.limitations[0]!);
  expect(html).not.toContain("<details open");
  const unknown = renderToStaticMarkup(<InstanceResources report={instanceResourceCase("cpu_null").report} />);
  expect(unknown).toContain(t("ui.memory_failures.not_reported"));
  const denied = renderToStaticMarkup(<InstanceResources report={instanceResourceCase("cpu_denied").report} />);
  expect(denied).toContain(t("ui.dashboard.status.unavailable"));
});


test("native scheduler detail keeps individual statuses, source interval, exact counters and unknown resets folded", () => {
  const { report } = instanceResourceCase("schedulers_exact");
  const schedulers = report.schedulers!;
  const html = renderToStaticMarkup(<InstanceResources report={report} />);
  expect(html).toContain(schedulers.title);
  expect(html).toContain("VISIBLE ONLINE");
  expect(html).toContain("HIDDEN ONLINE");
  expect(html).toContain("work_queue_count");
  expect(html).toContain("9007199254740993");
  expect(html).toContain(schedulers.limitations[0]!);
  expect(html).toContain(t("view.schedulers.counter.cumulative"));
  expect(html).not.toContain("<details open");
  expect(html).not.toContain("sig-dot");
});

test("scheduler native NULL, unavailable columns, native partial population and denial remain distinct", () => {
  const nativeNull = renderToStaticMarkup(<InstanceResources report={instanceResourceCase("schedulers_null").report} />);
  const unavailable = renderToStaticMarkup(<InstanceResources report={instanceResourceCase("schedulers_unavailable").report} />);
  expect(nativeNull).toContain(t("view.schedulers.value.null"));
  expect(nativeNull).not.toContain(t("view.schedulers.value.unavailable"));
  expect(unavailable).toContain(t("view.schedulers.value.unavailable"));
  const partial = renderToStaticMarkup(<InstanceResources report={instanceResourceCase("schedulers_partial").report} />);
  expect(partial).toContain(t("ui.common.read_partial"));
  const denied = renderToStaticMarkup(<InstanceResources report={instanceResourceCase("schedulers_denied").report} />);
  expect(denied).toContain(t("ui.dashboard.status.unavailable"));
  expect(denied).not.toContain(t("view.schedulers.entry", { id: "0" }));
});

test("scheduler renderer bounds malformed extra rows and fields and renders native text without HTML interpretation", () => {
  const { report } = instanceResourceCase("schedulers_limited");
  const schedulers = report.schedulers!;
  const entry = schedulers.entries[0]!;
  entry.fields[0]!.value = "<img src=x onerror=alert(1)>";
  entry.fields.push({ ...entry.fields[0]!, native_name: "extra-column", value: "OFFSCREEN-FIELD-CANARY" });
  schedulers.entries.push(...Array.from({ length: 512 }, (_, index) => ({ ...entry, observation_key: `extra-${index}`, title: "OFFSCREEN-ROW-CANARY" })));
  const html = renderToStaticMarkup(<InstanceResources report={report} />);
  expect(html).toContain("&lt;img");
  expect(html).not.toContain("<img");
  expect(html).not.toContain("OFFSCREEN-FIELD-CANARY");
  expect(html).not.toContain("OFFSCREEN-ROW-CANARY");
  expect(html).toContain(t("ui.memory_failures.page", { first: "1", last: "10", total: "256" }));
  expect(html).toContain(t("view.schedulers.limit.presentation"));
});


test("Health withholds a healthy empty state when an original scheduler source has no admitted explanation", () => {
  const report = instanceResourceHealthReport();
  report.instance_resources = undefined;
  report.snapshot.pressure = null;
  report.snapshot.sources = [{ source: "schedulers", state: "collected", reason: null }];
  const html = renderToStaticMarkup(<HealthTab report={report} pressure={null} resources={[]} engineLabel="PostgreSQL" />);
  expect(html).toContain(t("view.instance_resources.invalid"));
  expect(html).not.toContain(t("ui.health.empty"));
});


test("modern scheduler fields keep SQL layout absence and native NULL separate from unqualified spinlock diagnostics", () => {
  for (const [name, raw] of [["schedulers_exact", "0"], ["schedulers_spinlock_raw_positive", "7"]] as const) {
    const { report } = instanceResourceCase(name);
    const field = report.schedulers!.entries[0]!.fields.find((field) => field.native_name === "spinlock_wait_count")!;
    expect(field.value).toBeNull();
    expect(field.availability).toBe("instrumentation-unqualified");
    expect(field.unqualified?.native_value).toBe(raw);
    const html = renderToStaticMarkup(<InstanceResources report={report} />);
    expect(html).toContain(t("view.schedulers.value.unqualified", { value: raw }));
    expect(html).toContain(t("view.schedulers.counter.instrumentation", { flag: "8134" }));
  }
  const sql2019 = renderToStaticMarkup(<InstanceResources report={instanceResourceCase("schedulers_sql2019").report} />);
  expect(sql2019).toContain(t("view.schedulers.value.unavailable"));
  const nativeNull = renderToStaticMarkup(<InstanceResources report={instanceResourceCase("schedulers_modern_nulls").report} />);
  expect(nativeNull).toContain(t("view.schedulers.value.null"));
  expect(nativeNull).not.toContain(t("view.schedulers.value.unqualified", { value: "0" }));
});


test("boxed CPU originals retain independent native points and qualified interval arithmetic from Rust", () => {
  const { report } = instanceResourceCase("cpu_profile_exact");
  const html = renderToStaticMarkup(<InstanceResources report={report} />);
  expect(html).toContain(report.process_cpu_interval!.summary);
  expect(html).toContain("4000");
  expect(html).toContain(t("view.process_cpu.limit.native_sample"));
  expect(html).not.toContain("<details open");
  expect(html).not.toContain("sig-dot");
});
test("Linux and unknown-platform idle remain unavailable while raw native values stay separate", () => {
  for (const name of ["cpu_profile_linuxidle", "cpu_profile_unknownplatform"] as const) {
    const { report } = instanceResourceCase(name);
    const cpu = report.process_cpu_originals![1]!;
    expect(cpu.populations[1]!.entries[0]!.fields.find((field) => field.key === "idle_cpu_percent")!.value).toBeNull();
    const html = renderToStaticMarkup(<InstanceResources report={report} />);
    expect(html).toContain(t("view.process_cpu.field.raw_idle"));
    expect(html).toContain(t("ui.memory_failures.not_reported"));
    expect(html).toContain(report.process_cpu_interval!.summary);
  }
});
test("missing or reset original CPU evidence never becomes healthy empty or measured interval", () => {
  const data = instanceResourceHealthReport("cpu_profile_reset");
  expect(data.instance_resources!.process_cpu_interval!.available).toBe(false);
  data.snapshot.operations.process_cpu_originals = [{ kind: "unavailable", reason: { kind: "catalog", key: "view.process_cpu.invalid", arguments: {} } }];
  data.snapshot.pressure = null;
  data.snapshot.sources = [];
  data.instance_resources = undefined;
  const html = renderToStaticMarkup(<HealthTab report={data} pressure={data.snapshot.pressure} resources={[]} engineLabel="PostgreSQL" />);
  expect(html).not.toContain(t("ui.health.empty"));
  expect(html).toContain(t("view.instance_resources.invalid"));
});
test("native CPU values remain text and bounded fields preserve later raw context", () => {
  const { report } = instanceResourceCase("cpu_profile_exact");
  report.process_cpu_originals![0]!.populations[0]!.entries[0]!.fields[0]!.value = "<img src=x onerror=alert(1)>";
  const html = renderToStaticMarkup(<InstanceResources report={report} />);
  expect(html).toContain("&lt;img");expect(html).not.toContain("<img");expect(html).toContain(t("view.process_cpu.field.raw_idle"));
});


test("native resource-pool originals use the existing folded pagination and retain later pools", () => {
  const { report } = instanceResourceCase("cpu_profile_manypools");
  const html = renderToStaticMarkup(<InstanceResources report={report} />);
  expect(html).toContain(t("ui.memory_failures.page", { first: "1", last: "10", total: "12" }));
  expect(html).not.toContain(t("view.process_cpu.pool.title", { id: "11" }));
  expect(html).toContain(report.process_cpu_interval!.summary);
});
