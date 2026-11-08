import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { frontendCatalog, frontendKeys } from "../../build/catalogs";

test("the UI bundle keeps frontend and generated engine messages, leaving backend prose in Rust", () => {
  const root = join(import.meta.dir, "../..");
  const english = JSON.parse(readFileSync(join(root, "../locales/en.json"), "utf8"));
  const selected = frontendCatalog(english, frontendKeys(join(root, "src"), english));
  for (const key of ["ui.sidebar.language", "common.metric.rows", "core.engines.planning.planning", "model.replica.state.slot_active", "check.row_lock_waits.metric.time"]) {
    expect(selected[key], key).toBe(english[key]);
  }
  expect(selected["check.heap_table.summary.heap"]).toBeUndefined();
  expect(Object.keys(selected).length).toBeLessThan(Object.keys(english).length / 2);
  expect(selected._meta).not.toHaveProperty("reviewed");
});
