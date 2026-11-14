import { expect, test } from "bun:test";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import type { FileIo } from "../gen/FileIo";
import type { Report } from "../gen/Report";
import { StorageLatency } from "../components/BeyondTime";
import { partial } from "../test/fixtures";
import { fileIoKey, ioContext, storageAverage } from "./file-io";
import { t } from "./i18n";

function file(name: string, extra: Partial<FileIo> = {}): FileIo {
  return {
    name, kind: "data", database: "shop", context: { read: "storage-operation", write: "storage-operation", replica_id: null },
    reads: 1000, read_ms: 1000, read_bytes: 8192000, writes: 0, write_ms: null, write_bytes: null,
    scope: "database", stats_reset: "2026-11-12T10:00:00Z", write_operation: "write", ...extra,
  };
}

test("A storage average weights one native population and rejects unlike or incomplete measurement domains", () => {
  const first = file("data-a");
  const second = file("data-b", { reads: 3000, read_ms: 27000 });
  expect(storageAverage([first, second], false)).toEqual({ ms: 7, kind: "reads" });
  const changes: Partial<FileIo>[] = [
    { kind: "temp" }, { database: "another" }, { scope: "server" }, { stats_reset: undefined },
    { context: { ...first.context, replica_id: "replica-b" } },
    { context: { ...first.context, read: "cache-page" } },
    { context: { ...first.context, write: "unknown" } },
    { read_ms: null }, { read_ms: Number.POSITIVE_INFINITY },
  ];
  for (const change of changes) expect(storageAverage([first, { ...second, ...change }], false)).toBeNull();
  expect(storageAverage([first, file("idle", { reads: 0, read_ms: null })], false)).toEqual({ ms: 1, kind: "reads" });
  expect(storageAverage([], false)).toBeNull();
  expect(storageAverage([file("overflow-a", { reads: Number.MAX_VALUE, read_ms: Number.MAX_VALUE }), file("overflow-b", { reads: Number.MAX_VALUE, read_ms: Number.MAX_VALUE })], false)).toBeNull();
});

test("A flush cannot relabel other writes, and row identity includes the full native context", () => {
  const write = file("log", { kind: "log", writes: 1000, write_ms: 2000 });
  const flush = { ...write, write_operation: "flush" as const };
  expect(storageAverage([write], true)).toEqual({ ms: 2, kind: "writes" });
  expect(storageAverage([flush], true)).toEqual({ ms: 2, kind: "flushes" });
  expect(storageAverage([write, flush], true)).toBeNull();
  expect(fileIoKey(write)).not.toBe(fileIoKey(flush));
  expect(fileIoKey(write)).not.toBe(fileIoKey({ ...write, context: { ...write.context, replica_id: "replica-b" } }));
  expect(fileIoKey(write)).toBe(fileIoKey({ ...write, writes: 2000, write_ms: 4000 }));
});

test("The storage UI retains cache, remote, simulated and replica evidence without a physical-speed lead", () => {
  const report = partial<Report>({ findings: [] });
  for (const native of [
    file("cache", { kind: "other", context: { read: "cache-page", write: "unknown", replica_id: null } }),
    file("remote", { context: { read: "page-server-response", write: "simulated", replica_id: "7" } }),
    file("log-service", { kind: "log", reads: 0, read_ms: null, writes: 1000, write_ms: 2000, context: { read: "unknown", write: "log-service-write", replica_id: "8" } }),
    file("secondary-log", { kind: "log", reads: 0, read_ms: null, writes: 1000, write_ms: 2000, context: { read: "unknown", write: "replica-log-accounting", replica_id: "9" } }),
  ]) {
    const markup = renderToStaticMarkup(createElement(StorageLatency, { files: [native], engine: "mssql", report }));
    expect(markup).toContain(native.name);
    expect(markup).toContain(t("view.dashboard.storage.native"));
    expect(markup).toContain(t("core.io.accounting", { accounting: native.context.read }));
    expect(markup).toContain(t("core.io.accounting", { accounting: native.context.write }));
    expect(markup).not.toContain("Reads average");
    expect(ioContext(native)).toContain(native.context.replica_id == null ? "Native accounting" : `Native replica: ${native.context.replica_id}`);
  }
  const writes = file("log-write", { kind: "log", writes: 1000, write_ms: 2000 });
  const flushes = file("log-flush", { kind: "log", writes: 1000, write_ms: 3000, write_operation: "flush" });
  const mixed = renderToStaticMarkup(createElement(StorageLatency, { files: [writes, flushes], engine: "mssql", report }));
  expect(mixed).toContain(t("view.dashboard.storage.mixed"));
  expect(mixed).toContain(writes.name);
  expect(mixed).toContain(flushes.name);
});
