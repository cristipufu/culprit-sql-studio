import type { RuntimeDuration } from "../gen/RuntimeDuration";
import type { RuntimeQuantile } from "../gen/RuntimeQuantile";
import { digits } from "./format";

const MAX_UNSIGNED = 18_446_744_073_709_551_615n;
export const MAX_RUNTIME_DISTRIBUTIONS = 8;
export const MAX_RUNTIME_BUCKETS = 512;

/** Exact native counts remain strings; conversion to number is only for finite chart geometry. */
export function sampleCount(value: string | null | undefined): bigint | null {
  if (value == null || !/^(0|[1-9][0-9]{0,19})$/.test(value)) return null;
  const count = BigInt(value);
  return count <= MAX_UNSIGNED ? count : null;
}

export function exactSamples(value: string): string {
  const count = sampleCount(value);
  return count == null ? "—" : digits(count.toString().replace(/\B(?=(\d{3})+(?!\d))/g, ","));
}

/** Preserve raw timer units. No statement maximum, minimum, mean or count participates in this conversion. */
export function runtimeMilliseconds(value: RuntimeDuration | null | undefined): number | null {
  if (value == null) return null;
  const native = value.unit === "picoseconds" ? sampleCount(value.value) : null;
  const milliseconds = value.unit === "milliseconds" ? value.value : native == null ? null : Number(native) / 1e9;
  return milliseconds != null && Number.isFinite(milliseconds) && milliseconds >= 0 ? milliseconds : null;
}

export function quantileUpper(value: RuntimeQuantile | null | undefined): number | null {
  return value == null ? null : runtimeMilliseconds(value.kind === "sample-value" ? value.value : value.upper);
}

export function quantileLower(value: RuntimeQuantile | null | undefined): number | null {
  return value == null ? null : runtimeMilliseconds(value.kind === "sample-value" ? value.value : value.lower);
}
