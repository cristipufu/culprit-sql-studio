import type { NativeIdentity } from "../gen/NativeIdentity";
import type { Report } from "../gen/Report";
import type { RuntimePopulationCatalog } from "../gen/RuntimePopulationCatalog";

export const GROUP_A: NativeIdentity = { id: "17", qualifiers: {} };
export const GROUP_B: NativeIdentity = { id: "18", qualifiers: {} };
export const GROUP_B_LABEL = 'Read group <img id="population-html" src=x onerror="window.populationHtml=true">';

export const POPULATIONS: RuntimePopulationCatalog = {
  membership: { kind: "native-catalog" },
  source: "fixture.runtime",
  partitioned: true,
  requires_selection: true,
  populations: [
    { identity: GROUP_A, label: { kind: "text", text: "Write group" }, native_properties: { history: "original source label" } },
    { identity: GROUP_B, label: { kind: "text", text: GROUP_B_LABEL }, native_properties: { history: "independent source label" } },
  ],
};

/** Bounded UI-only reports: one shared query id in independent groups, with no borrowed plan or compiled data. */
export function executionGroupReport(base: Report, name: string, windowMinutes: number, population: NativeIdentity | null): Report {
  const report = structuredClone(base);
  if (!report.snapshot.options || !report.snapshot.workload) throw new Error("The fixture requires a recorded workload and options");
  report.snapshot.id = `fixture-${name}-${population?.id ?? "overview"}-${windowMinutes}`;
  report.snapshot.connection = name;
  report.snapshot.runtime_populations = structuredClone(POPULATIONS);
  report.snapshot.options = { ...report.snapshot.options, window_minutes: windowMinutes, workload_population: population ?? undefined };
  report.findings = [];
  if (!population) {
    report.snapshot.workload = null;
    return report;
  }
  const query = report.snapshot.workload.queries[0];
  if (!query) throw new Error("The fixture requires one original query");
  query.id = "shared-query-id";
  query.text = `SELECT id FROM execution_group_${population.id}`;
  query.query_hash = null;
  query.parameters = null;
  query.object_name = null;
  query.requests = null;
  query.resolved_write_target = null;
  query.temporary_work = null;
  query.waits = [];
  query.plan = null;
  query.plans = [];
  query.costs = null;
  query.spread = null;
  query.variant_family = undefined;
  report.snapshot.workload = {
    ...report.snapshot.workload,
    population: structuredClone(population),
    population_catalog: structuredClone(POPULATIONS),
    queries: [query],
    query_count: 1,
    timeline: null,
  };
  return report;
}
