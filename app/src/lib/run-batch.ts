import type { Engine } from "../gen/Engine";
import type { Finding } from "../gen/Finding";
import type { ResultSet } from "../gen/ResultSet";
import type { StatementCheck } from "../gen/StatementCheck";
import { type Api, type ApiError, errorMessage } from "./api";
import { t } from "./i18n";

/** What running a batch needs from the API. */
export type RunApi = Pick<Api, "checkStatement" | "lintSql" | "run">;

/** A statement the editor ran and the rows it returned. */
export type StatementResult = { result: ResultSet; sql: string };

/** A batch waiting for the go-ahead on production: the statement the question is about, and the batch's lint. */
export type PendingRun = { kind: "run"; sql: string; check: StatementCheck; lint?: Finding[] };

/**
 * What became of a batch: its rows, a question to ask before it runs, the error that stopped it, or the stop its owner
 * called before its next statement (`ran` of them had been sent).
 */
export type BatchOutcome =
  | { kind: "rows"; results: StatementResult[] }
  | { kind: "confirm"; pending: PendingRun }
  | { kind: "error"; error: ApiError }
  | { kind: "stopped"; ran: number };

export type Batch = {
  connection: string;
  engine: Engine;
  /** The text as written, for the question and its lint. */
  sql: string;
  /** Its statements, in the order they run. */
  statements: string[];
  limit: number;
  production: boolean;
  /** The person already said yes to this batch. */
  confirmed: boolean;
};

/**
 * The statement a confirmation is about: the batch's first that isn't a read, with the first warning any of those
 * carries. When the engine asked for confirmation (`required`) though every statement checked as a read here, the first
 * statement stands for the batch. None when nothing needs confirming.
 */
export function confirmationCheck(checks: StatementCheck[], required: boolean): StatementCheck | null {
  const changes = checks.filter((c) => !c.reads_only && !c.problem);
  const base = changes[0] ?? (required ? checks[0] : undefined);
  if (!base) return null;
  return { ...base, warning: changes.find((c) => c.warning)?.warning ?? base.warning };
}

async function confirmation(api: RunApi, batch: Batch, required: boolean): Promise<PendingRun | null> {
  const checks = await Promise.all(batch.statements.map((s) => api.checkStatement(batch.engine, s)));
  const check = confirmationCheck(checks, required);
  if (!check) return null;
  // The lint is context for the question, not a condition for asking it.
  const lint = await api.lintSql(batch.connection, batch.sql).catch(() => []);
  return { kind: "run", sql: batch.sql, check, lint };
}

/**
 * Run a batch's statements in order, telling `progress` as each starts. On production, a batch with a statement that
 * isn't a read is a question before anything runs. The engine decides whatever was checked here: when it asks for
 * confirmation partway, the batch stops there with the same question, and saying yes runs it again from the start
 * (only reads can have run before the statement it stopped at). `live` is asked before each statement is sent: once it
 * says no (the tab that ran the batch closed), nothing more is sent; a statement already sent finishes on the database.
 * Never throws: a failure is the outcome.
 */
export async function runBatch(
  api: RunApi,
  batch: Batch,
  progress: (label: string) => void,
  live: () => boolean = () => true,
): Promise<BatchOutcome> {
  const { statements } = batch;
  try {
    if (batch.production && !batch.confirmed) {
      const pending = await confirmation(api, batch, false);
      if (pending) return { kind: "confirm", pending };
    }
    const results: StatementResult[] = [];
    for (const [i, sql] of statements.entries()) {
      if (!live()) return { kind: "stopped", ran: i };
      const place = { number: String(i + 1), total: String(statements.length) };
      progress(statements.length > 1 ? t("ui.run_batch.running_statement", place) : t("ui.run_batch.running"));
      try {
        results.push({ result: await api.run(batch.connection, sql, batch.limit, batch.confirmed), sql });
      } catch (e) {
        const error = errorMessage(e);
        if (error.code === "confirmation_required") {
          const pending = await confirmation(api, batch, true);
          if (pending) return { kind: "confirm", pending };
        }
        const message = statements.length > 1 ? t("ui.run_batch.statement_failed", { ...place, message: error.message }) : error.message;
        return { kind: "error", error: { ...error, message } };
      }
    }
    return { kind: "rows", results };
  } catch (e) {
    return { kind: "error", error: errorMessage(e) };
  }
}
