import examples from "../../../fixtures/plan-view.sample.json" with { type: "json" };
import english from "../../../locales/en.json" with { type: "json" };
import type { Metric } from "../gen/Metric";
import type { Plan } from "../gen/Plan";
import type { PlanPageServerIoView } from "../gen/PlanPageServerIoView";
import { fromRust } from "./fixtures";

type PageServerMetricKey = "view.plan_page_server_io.reads" | "view.plan_page_server_io.read_aheads"
  | "view.plan_page_server_io.lob_reads" | "view.plan_page_server_io.lob_read_aheads";

/** Controlled display contracts, not a native interpreter: the renderer must preserve these supplied strings. */
export function pageServerDisplay(partial = false): PlanPageServerIoView {
  const unknown = english["view.plan_page_server_io.unknown"];
  const metric = (key: PageServerMetricKey, text: string): Metric => ({
    key, label: english[key], text, value: 0, unit: "text",
  });
  return {
    metrics: [
      metric("view.plan_page_server_io.reads", partial ? unknown : "9007199254740995"),
      metric("view.plan_page_server_io.read_aheads", partial ? "0" : "7"),
      metric("view.plan_page_server_io.lob_reads", partial ? "5" : "10"),
      metric("view.plan_page_server_io.lob_read_aheads", partial ? unknown : "0"),
    ],
    limitations: [
      english["view.plan_page_server_io.limit.recorded"],
      ...(partial ? [english["view.plan_page_server_io.limit.partial"]] : []),
    ],
  };
}

/** A Rust-produced base tree with one supplied operator display; no new source/capture authority is constructed. */
export function pageServerPlan(display: PlanPageServerIoView | null = pageServerDisplay()): Plan {
  const sample = examples.find((example) => example.name === "retained-old");
  if (sample == null) throw new Error("Missing Rust retained plan fixture");
  const plan = fromRust<{ plan: Plan }>(sample).plan;
  const operator = plan.view?.nodes.find((operator) => operator.id === plan.root.id);
  if (operator == null || plan.view == null) throw new Error("Missing Rust operator display");
  plan.root.children = [];
  plan.view.nodes = [operator];
  operator.page_server_io = display ?? undefined;
  return plan;
}
