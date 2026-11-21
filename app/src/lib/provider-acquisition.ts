import type { ObservedProbeAttempt } from "../gen/ObservedProbeAttempt";
import type { ObservedScanAttempt } from "../gen/ObservedScanAttempt";
import type { ProviderAcquisitionReport } from "../gen/ProviderAcquisitionReport";
type AcquisitionListener = (report: ProviderAcquisitionReport) => void;
/** Each caller owns this attempt; neither errors nor a global last-result slot are mutated. */
export function requireProbe(attempt: ObservedProbeAttempt, observe?: AcquisitionListener) {
  observe?.(attempt.acquisition);
  const outcome = attempt.outcome;
  switch (outcome.kind) {
    case "connected":
      return outcome.server;
    case "failed":
    case "finalization-failed":
    case "native-work-unconfirmed":
      throw outcome.error;
  }
}
export function requireScan(attempt: ObservedScanAttempt, observe?: AcquisitionListener) {
  switch (attempt.kind) {
    case "complete":
      if (attempt.report.provider_acquisition) observe?.(attempt.report.provider_acquisition);
      return attempt.report;
    case "failed":
    case "finalization-failed":
      observe?.(attempt.acquisition);
      throw attempt.error;
    case "native-work-unconfirmed":
      observe?.(attempt.acquisition);
      throw attempt.error;
  }
}
