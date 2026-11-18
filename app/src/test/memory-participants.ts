import nativeCases from "../../../fixtures/memory-participants.sample.json" with { type: "json" };
import type { MemoryParticipantsReport } from "../gen/MemoryParticipantsReport";
import type { RequestMemorySnapshot } from "../gen/RequestMemorySnapshot";
import { fromRust } from "./fixtures";

/** Original raw inputs and the actual shared Rust report; no browser lifecycle or owner classification. */
export function requestMemoryCase(name: keyof typeof nativeCases = "qualified") {
  return fromRust<{ evidence: RequestMemorySnapshot; report: MemoryParticipantsReport }>(nativeCases[name]);
}
