import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { CounterRatios } from "../components/CounterRatios";
import type { CounterRatioReport } from "../gen/CounterRatioReport";
import { t } from "./i18n";

function report(): CounterRatioReport {
  return {
    source_state: "partial", source_reason: "The original source is partial.", read_complete: false,
    declared_ratios: 1, presented_ratios: 1, entries_limited: false,
    title: t("view.counter_ratios.title"), summary: t("view.counter_ratios.summary.partial"),
    limitations: [t("view.counter_ratios.limit.aggregate"), t("view.counter_ratios.limit.scope")],
    entries: [{
      observation_key: "counter-read", kind: "local-ssd-cache-hit", scope: "server", observed_at: "2020-01-01T12:00:00.5Z",
      native_object: "<img src=x onerror=alert(1)>", native_instance: null, issues: ["missing-base"],
      title: t("view.counter_ratios.local_ssd.title"), explanation: t("view.counter_ratios.issue.missing_base"),
      metrics: [
        { key: "view.counter_ratios.metric.numerator", label: t("view.counter_ratios.metric.numerator"), value: 0, unit: "text", text: "9007199254740993" },
        { key: "view.counter_ratios.metric.base", label: t("view.counter_ratios.metric.base"), value: 0, unit: "text", text: t("view.counter_ratios.unknown") },
      ],
    }],
  };
}

test("native counter details remain folded and preserve exact text, clocks, partial state and safe native labels", () => {
  const html = renderToStaticMarkup(<CounterRatios report={report()} />);
  expect(html).toContain("9007199254740993"); expect(html).not.toContain("9007199254740992");
  expect(html).toContain("2020-01-01T12:00:00.5Z"); expect(html).toContain(t("view.counter_ratios.unknown"));
  expect(html).toContain(t("view.counter_ratios.issue.missing_base")); expect(html).not.toContain("100.00%");
  expect(html).not.toContain("<img"); expect(html).toContain("&lt;img");
  expect(html).not.toContain("<details open"); expect(html).toContain(t("view.counter_ratios.limit.aggregate"));
});

test("an absent expected native pair never invents object or instance identity or a percentage", () => {
  const source = report(); const entry = source.entries[0]!;
  entry.native_object = null; entry.native_instance = null; entry.observed_at = null;
  entry.issues = ["missing-numerator", "missing-base"];
  entry.metrics[0]!.text = t("view.counter_ratios.unknown");
  const html = renderToStaticMarkup(<CounterRatios report={source} />);
  expect(html).not.toContain("0.00%"); expect(html).not.toContain("100.00%");
  expect(html).not.toContain(t("view.counter_ratios.uninstanced"));
  expect(html).toContain(t("view.counter_ratios.unknown"));
});
