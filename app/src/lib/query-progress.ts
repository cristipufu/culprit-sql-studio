import type { Plan } from "../gen/Plan";
import type { QueryProgress } from "../gen/QueryProgress";

export type ProgressRead = { withPlan: boolean; requestKey: string | null };

/** One mounted connection/session owns one bounded plan cache and one attempt per newly observed request. */
export class ProgressPlanCache {
  private initial = true;
  private observedKey: string | null = null;
  private requestedKey: string | null = null;
  private cached: { key: string; plan: Plan } | null = null;

  beginRead(): ProgressRead {
    const withPlan = this.initial || (this.observedKey != null && this.observedKey !== this.requestedKey);
    this.initial = false;
    if (withPlan) this.requestedKey = this.observedKey;
    return { withPlan, requestKey: this.observedKey };
  }

  accept(next: QueryProgress, read: ProgressRead): QueryProgress {
    if (next.running !== true || next.session_context != null) {
      this.unavailable();
      return { ...next, plan: null };
    }
    const key = next.request_key;
    if (!key) {
      this.unavailable();
      // A plan in this native response remains its own point; no later reply may borrow it without a key.
      return next;
    }
    this.observedKey = key;
    if (this.cached?.key !== key) this.cached = null;
    if (next.plan) {
      this.cached = { key, plan: next.plan };
      this.requestedKey = key;
    } else if (read.withPlan && (read.requestKey == null || read.requestKey === key)) {
      this.requestedKey = key;
    }
    return { ...next, plan: next.plan ?? this.cached?.plan ?? null };
  }

  unavailable(): void {
    this.observedKey = null;
    this.cached = null;
  }
}

export function progressFraction(value: number | null): number | null {
  return value != null && Number.isFinite(value) && value >= 0 && value <= 1 ? value : null;
}
