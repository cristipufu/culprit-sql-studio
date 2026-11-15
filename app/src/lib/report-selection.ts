import type { NativeIdentity } from "../gen/NativeIdentity";
import type { Report } from "../gen/Report";
import { nativeIdentityKey } from "./native-identity";

/** The requested default or exact native population owns its own saved report and workload window. */
export type ReportSelection = { name: string; windowMinutes: number; population: NativeIdentity | null };

export function samePopulation(left: NativeIdentity | null, right: NativeIdentity | null): boolean {
  return left === null || right === null ? left === right : nativeIdentityKey(left) === nativeIdentityKey(right);
}

export function populationKey(population: NativeIdentity | null): string {
  return population === null ? "default" : nativeIdentityKey(population);
}

/** Actual workload identity may be known on a default scan; it never changes which saved view was requested. */
export function reportMatchesSelection(report: Report, selection: ReportSelection): boolean {
  const options = report.snapshot.options;
  if (options?.window_minutes !== selection.windowMinutes
    || !samePopulation(options?.workload_population ?? null, selection.population)) return false;
  const actual = report.snapshot.workload?.population;
  return selection.population === null || report.snapshot.workload == null
    || (actual != null && samePopulation(actual, selection.population));
}
