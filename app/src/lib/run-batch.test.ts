import { expect, test } from "bun:test";
import type { ApiError } from "./api";
import type { StatementCheck } from "../gen/StatementCheck";
import { type Batch, type BatchOutcome, type RunApi, confirmationCheck, runBatch } from "./run-batch";

const read: StatementCheck = { reads_only: true, verb: "SELECT", warning: null, problem: null };
const change = (verb: string, warning: string | null = null): StatementCheck => ({ reads_only: false, verb, warning, problem: null });

/** An API whose statements check as `checks` says (reads otherwise), whose engine refuses or fails the ones listed. */
function fakeApi(options: {
  checks?: Record<string, StatementCheck>;
  refuse?: string[];
  fail?: Record<string, ApiError>;
  lintFails?: boolean;
  checkFails?: boolean;
}) {
  const ran: { sql: string; confirmed: boolean }[] = [];
  const api: RunApi = {
    checkStatement: async (_engine, sql) => {
      if (options.checkFails) throw new Error("the check is unavailable");
      return options.checks?.[sql] ?? read;
    },
    lintSql: async () => {
      if (options.lintFails) throw new Error("lint failed");
      return [];
    },
    run: async (_connection, sql, _limit, confirmed) => {
      if (options.refuse?.includes(sql) && !confirmed) {
        throw { code: "confirmation_required", message: "This UPDATE changes data", hint: null } satisfies ApiError;
      }
      const failure = options.fail?.[sql];
      if (failure) throw failure;
      ran.push({ sql, confirmed });
      return { columns: ["sql"], rows: [[sql]], truncated: false, elapsed_ms: 1, messages: [] };
    },
  };
  return { api, ran };
}

function batch(statements: string[], overrides: Partial<Batch> = {}): Batch {
  return { connection: "orders", engine: "postgres", sql: statements.join(";\n"), statements, limit: 1000, production: false, confirmed: false, ...overrides };
}

async function outcome(api: RunApi, b: Batch) {
  const labels: string[] = [];
  const result: BatchOutcome = await runBatch(api, b, (label) => labels.push(label));
  return { result, labels };
}

test("On production, a batch that changes data asks first, about its first change, and runs nothing", async () => {
  const { api, ran } = fakeApi({ checks: { "UPDATE a": change("UPDATE"), "DELETE b": change("DELETE", "It deletes every row") } });
  const { result, labels } = await outcome(api, batch(["SELECT 1", "UPDATE a", "DELETE b"], { production: true }));
  expect(result.kind).toBe("confirm");
  if (result.kind !== "confirm") return;
  expect(result.pending.check.verb).toBe("UPDATE");
  // The warning is the batch's, from whichever change carries one.
  expect(result.pending.check.warning).toBe("It deletes every row");
  expect(result.pending.sql).toBe("SELECT 1;\nUPDATE a;\nDELETE b");
  expect(ran).toEqual([]);
  expect(labels).toEqual([]);
});

test("Reads run without asking, in order, each announced", async () => {
  const { api, ran } = fakeApi({});
  const { result, labels } = await outcome(api, batch(["SELECT 1", "SELECT 2"], { production: true }));
  expect(result.kind).toBe("rows");
  if (result.kind === "rows") expect(result.results.map((r) => r.sql)).toEqual(["SELECT 1", "SELECT 2"]);
  expect(ran).toEqual([{ sql: "SELECT 1", confirmed: false }, { sql: "SELECT 2", confirmed: false }]);
  expect(labels).toEqual(["Running 1 of 2…", "Running 2 of 2…"]);
  expect((await outcome(fakeApi({}).api, batch(["SELECT 1"]))).labels).toEqual(["Running…"]);
});

test("A confirmed batch runs without checking again, and carries the consent to every statement", async () => {
  const { api, ran } = fakeApi({ checks: { "UPDATE a": change("UPDATE") }, refuse: ["UPDATE a"] });
  const { result } = await outcome(api, batch(["UPDATE a", "SELECT 1"], { production: true, confirmed: true }));
  expect(result.kind).toBe("rows");
  expect(ran).toEqual([{ sql: "UPDATE a", confirmed: true }, { sql: "SELECT 1", confirmed: true }]);
});

test("Off production, changes run without asking", async () => {
  const { api, ran } = fakeApi({ checks: { "UPDATE a": change("UPDATE") } });
  expect((await outcome(api, batch(["UPDATE a"]))).result.kind).toBe("rows");
  expect(ran).toEqual([{ sql: "UPDATE a", confirmed: false }]);
});

test("The engine asking partway stops the batch with the same question, about the first statement when all checked as reads", async () => {
  // The engine decides whatever the UI checked: here the UI thought the connection wasn't production.
  const { api, ran } = fakeApi({ refuse: ["SELECT nextval('s')"] });
  const { result, labels } = await outcome(api, batch(["SELECT 1", "SELECT nextval('s')", "SELECT 3"]));
  expect(result.kind).toBe("confirm");
  if (result.kind === "confirm") expect(result.pending.check).toEqual(read);
  expect(ran).toEqual([{ sql: "SELECT 1", confirmed: false }]);
  expect(labels).toEqual(["Running 1 of 3…", "Running 2 of 3…"]);

  const withChange = fakeApi({ checks: { "UPDATE a": change("UPDATE", "No WHERE clause") }, refuse: ["UPDATE a"] });
  const asked = (await outcome(withChange.api, batch(["SELECT 1", "UPDATE a"]))).result;
  if (asked.kind !== "confirm") throw new Error(`expected a question, got ${asked.kind}`);
  expect(asked.pending.check).toEqual(change("UPDATE", "No WHERE clause"));
});

test("Lint that fails doesn't stop the question", async () => {
  const { api } = fakeApi({ checks: { "UPDATE a": change("UPDATE") }, lintFails: true });
  const { result } = await outcome(api, batch(["UPDATE a"], { production: true }));
  expect(result.kind === "confirm" && result.pending.lint).toEqual([]);
});

test("An error stops the batch and says which statement, when there are several", async () => {
  const error: ApiError = { code: "query_failed", message: "relation \"b\" does not exist", hint: null };
  const { api, ran } = fakeApi({ fail: { "SELECT * FROM b": error } });
  const { result } = await outcome(api, batch(["SELECT 1", "SELECT * FROM b", "SELECT 3"]));
  expect(result).toEqual({ kind: "error", error: { ...error, message: "Statement 2 of 3: relation \"b\" does not exist" } });
  expect(ran.map((r) => r.sql)).toEqual(["SELECT 1"]);
  // The engine's error is left as it was.
  expect(error.message).toBe("relation \"b\" does not exist");

  const alone = await outcome(fakeApi({ fail: { "SELECT * FROM b": error } }).api, batch(["SELECT * FROM b"]));
  expect(alone.result).toEqual({ kind: "error", error });
});

test("A check that fails is the batch's error, not a silent stop", async () => {
  const { api, ran } = fakeApi({ checkFails: true });
  const { result } = await outcome(api, batch(["UPDATE a"], { production: true }));
  expect(result).toEqual({ kind: "error", error: { code: "internal", message: "the check is unavailable", hint: null } });
  expect(ran).toEqual([]);
});

test("Text that isn't one statement isn't a change to confirm: the engine refuses it on its own", () => {
  const unclosed: StatementCheck = { reads_only: false, verb: "", warning: null, problem: "A quote is never closed" };
  expect(confirmationCheck([unclosed], false)).toBeNull();
  expect(confirmationCheck([read, read], false)).toBeNull();
  expect(confirmationCheck([read, change("INSERT")], false)?.verb).toBe("INSERT");
  expect(confirmationCheck([read], true)).toEqual(read);
});
