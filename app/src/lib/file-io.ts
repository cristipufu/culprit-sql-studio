import type { FileIo } from "../gen/FileIo";
import { t } from "./i18n";

export function sameIoContext(left: FileIo["context"], right: FileIo["context"]): boolean {
  return left.read === right.read && left.write === right.write && left.replica_id === right.replica_id;
}

function sameMeasurement(left: FileIo, right: FileIo): boolean {
  return left.kind === right.kind && left.database === right.database && sameIoContext(left.context, right.context) &&
    (left.scope ?? null) === (right.scope ?? null) && (left.stats_reset ?? null) === (right.stats_reset ?? null) &&
    (left.write_operation ?? null) === (right.write_operation ?? null);
}

/** This average covers one native storage-operation population, not unlike cache/replica/request counters. */
export function storageAverage(files: FileIo[], write: boolean): { ms: number; kind: "reads" | "writes" | "flushes" } | null {
  if (files.some((file) => {
    const n = write ? file.writes : file.reads;
    const ms = write ? file.write_ms : file.read_ms;
    return !Number.isFinite(n) || n < 0 || (ms != null && (!Number.isFinite(ms) || ms < 0 || (n === 0 && ms > 0)));
  })) return null;
  const measured = files.filter((file) => (write ? file.writes : file.reads) > 0);
  const first = measured[0];
  if (!first) return null;
  let operations = 0;
  let milliseconds = 0;
  for (const file of measured) {
    const n = write ? file.writes : file.reads;
    const ms = write ? file.write_ms : file.read_ms;
    if ((write ? file.context.write : file.context.read) !== "storage-operation" ||
      !sameMeasurement(first, file) || ms == null || !Number.isFinite(ms) || ms < 0 || !Number.isFinite(n)) return null;
    operations += n;
    milliseconds += ms;
  }
  if (!Number.isFinite(operations) || !Number.isFinite(milliseconds)) return null;
  const average = milliseconds / operations;
  return Number.isFinite(average) ? { ms: average, kind: write ? (first.write_operation === "flush" ? "flushes" : "writes") : "reads" } : null;
}

export function ioContext(file: FileIo): string {
  const context = t("core.io.context", {
    reads: t("core.io.accounting", { accounting: file.context.read }),
    writes: t("core.io.accounting", { accounting: file.context.write }),
  });
  return file.context.replica_id == null ? context : `${context} ${t("core.io.replica", { replica: file.context.replica_id })}`;
}

/** React ownership includes the native measurement identity, not only a possibly reused display filename. */
export function fileIoKey(file: FileIo): string {
  return JSON.stringify([file.name, file.kind, file.database, file.context.read, file.context.write, file.context.replica_id,
    file.scope ?? null, file.stats_reset ?? null, file.write_operation ?? null]);
}
