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
