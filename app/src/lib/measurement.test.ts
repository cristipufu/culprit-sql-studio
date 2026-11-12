import { expect, test } from "bun:test";
import { measurementOrigin } from "./measurement";

test("measurement wording follows confirmed execution protection and leaves unknown protection unstated", () => {
  expect(measurementOrigin("read-only-transaction")).toContain("read-only");
  expect(measurementOrigin("rolled-back-transaction")).toContain("rolled back");
  expect(measurementOrigin(null)).toBe("Measured just now: Culprit ran the statement once.");
  expect(measurementOrigin(undefined)).toBe(measurementOrigin(null));
});
