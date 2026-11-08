import { expect, test } from "bun:test";
import type { Plan } from "../gen/Plan";
import type { ResultSet } from "../gen/ResultSet";
import type { StatementCheck } from "../gen/StatementCheck";
import { EditorExecution, type ExecutionApi, type Output } from "./editor-execution";
import type { Batch, PendingRun } from "./run-batch";

/** A promise the test settles when it chooses. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

const rows = (sql: string): ResultSet => ({ columns: ["sql"], rows: [[sql]], truncated: false, elapsed_ms: 1, messages: [] });
const read: StatementCheck = { reads_only: true, verb: "SELECT", warning: null, problem: null };
const update: StatementCheck = { reads_only: false, verb: "UPDATE", warning: null, problem: null };

/** An API whose statements and plans each finish when the test says, and which records what was sent. */
function controlledApi() {
  const sent: { sql: string; confirmed: boolean }[] = [];
  const checked: string[] = [];
  const running = new Map<string, ReturnType<typeof deferred<ResultSet>>>();
  const planning = new Map<string, ReturnType<typeof deferred<Plan>>>();
  const api: ExecutionApi = {
    checkStatement: async (_engine, sql) => {
      checked.push(sql);
      return sql.startsWith("UPDATE") ? update : read;
    },
    lintSql: async () => [],
    run: (_connection, sql, _limit, confirmed) => {
      sent.push({ sql, confirmed });
      const d = deferred<ResultSet>();
      running.set(sql, d);
      return d.promise;
    },
    explain: (_connection, sql) => {
      const d = deferred<Plan>();
      planning.set(sql, d);
      return d.promise;
    },
  };
  /** Let the async steps before a statement is sent (checks, the client) happen. */
  const settle = () => new Promise((r) => setTimeout(r, 0));
  return { api, sent, checked, running, planning, settle };
}

function editor(client: () => Promise<ExecutionApi>) {
  const shown: { tab: string; output: Output }[] = [];
  const asked: { tab: string; pending: PendingRun | null }[] = [];
  const execution = new EditorExecution(
    client,
    (tab, output) => shown.push({ tab, output }),
    (tab, pending) => asked.push({ tab, pending }),
  );
  const last = (tab: string) => shown.filter((s) => s.tab === tab).at(-1)?.output.kind;
  return { execution, shown, asked, last };
}

function batch(statements: string[], production = false): Batch {
  return { connection: "orders", engine: "postgres", sql: statements.join(";\n"), statements, limit: 1000, production, confirmed: false };
}

test("A busy tab refuses a second run or a plan; the first one's result is what it shows", async () => {
  const { api, sent, running, settle } = controlledApi();
  const { execution, last } = editor(async () => api);
  const first = execution.run("t", batch(["SELECT 'a'"]));
  await settle();
  expect(await execution.run("t", batch(["SELECT 'b'"]))).toBe("busy");
  expect(await execution.explain("t", "orders", "SELECT 'b'", "estimated")).toBe("busy");
  expect(sent.map((s) => s.sql)).toEqual(["SELECT 'a'"]);
  running.get("SELECT 'a'")!.resolve(rows("a"));
  expect(await first).toBe("rows");
  expect(last("t")).toBe("rows");
  // Free again once it ended.
  expect(execution.busy("t")).toBe(false);
});

test("Tabs act independently: a slow run in one never writes over another tab's result", async () => {
  const { api, running, settle } = controlledApi();
  const { execution, shown } = editor(async () => api);
  const slow = execution.run("one", batch(["SELECT 'slow'"]));
  const fast = execution.run("two", batch(["SELECT 'fast'"]));
  await settle();
  running.get("SELECT 'fast'")!.resolve(rows("fast"));
  await fast;
  running.get("SELECT 'slow'")!.resolve(rows("slow"));
  await slow;
  const final = (tab: string) => shown.filter((s) => s.tab === tab).at(-1)!.output;
  expect(final("one")).toMatchObject({ kind: "rows", results: [{ sql: "SELECT 'slow'" }] });
  expect(final("two")).toMatchObject({ kind: "rows", results: [{ sql: "SELECT 'fast'" }] });
});

test("Closing a tab mid-batch sends nothing more, and its result never shows", async () => {
  const { api, sent, running, settle } = controlledApi();
  const { execution, shown } = editor(async () => api);
  const run = execution.run("t", batch(["UPDATE a SET x = 1", "UPDATE b SET y = 2"]));
  await settle();
  const before = shown.length;
  // The first statement is on the database: it can't be called back, and the editor says it finishes there.
  expect(execution.retire("t")).toBe(true);
  running.get("UPDATE a SET x = 1")!.resolve(rows("a"));
  expect(await run).toBe("stopped");
  expect(sent.map((s) => s.sql)).toEqual(["UPDATE a SET x = 1"]);
  expect(shown.length).toBe(before);
});

test("A plan that comes back after its tab closed is dropped", async () => {
  const { api, planning, settle } = controlledApi();
  const { execution, shown } = editor(async () => api);
  const plan = execution.explain("t", "orders", "SELECT 1", "estimated");
  await settle();
  execution.retire("t");
  planning.get("SELECT 1")!.resolve({} as Plan);
  expect(await plan).toBe("stopped");
  expect(shown.map((s) => s.output.kind)).toEqual(["busy"]);
});

test("A production change waits for a yes, owned by its tab: refused while waiting, run confirmed on yes", async () => {
  const { api, sent, running, settle } = controlledApi();
  const { execution, asked, last } = editor(async () => api);
  expect(await execution.run("t", batch(["UPDATE a SET x = 1"], true))).toBe("asked");
  expect(asked.at(-1)).toMatchObject({ tab: "t", pending: { check: { verb: "UPDATE" } } });
  expect(sent).toEqual([]);
  // The question holds the tab: nothing else runs there until it's answered.
  expect(await execution.run("t", batch(["SELECT 1"]))).toBe("busy");
  const yes = execution.confirm("t");
  expect(asked.at(-1)).toEqual({ tab: "t", pending: null });
  await settle();
  expect(sent).toEqual([{ sql: "UPDATE a SET x = 1", confirmed: true }]);
  running.get("UPDATE a SET x = 1")!.resolve(rows("a"));
  expect(await yes).toBe("rows");
  expect(last("t")).toBe("rows");
});

test("Saying no runs nothing and frees the tab; closing the tab withdraws its question", async () => {
  const { api, sent } = controlledApi();
  const { execution, asked } = editor(async () => api);
  await execution.run("t", batch(["UPDATE a SET x = 1"], true));
  execution.cancel("t");
  expect(execution.busy("t")).toBe(false);
  expect(sent).toEqual([]);

  await execution.run("u", batch(["UPDATE a SET x = 1"], true));
  // Nothing was sent: no statement is left running for it.
  expect(execution.retire("u")).toBe(false);
  expect(asked.at(-1)).toEqual({ tab: "u", pending: null });
  expect(await execution.confirm("u")).toBe("stopped");
  expect(sent).toEqual([]);
});

test("An API that fails to start is the tab's error, and the tab is free again", async () => {
  const { execution, shown } = editor(() => Promise.reject({ code: "internal", message: "The desktop bridge is unavailable", hint: null }));
  expect(await execution.run("t", batch(["SELECT 1"]))).toBe("error");
  expect(shown.at(-1)).toMatchObject({ tab: "t", output: { kind: "error", error: { message: "The desktop bridge is unavailable" } } });
  expect(execution.busy("t")).toBe(false);
  expect(await execution.explain("t", "orders", "SELECT 1", "estimated")).toBe("error");
});

test("Leaving the connection retires every tab and counts those with a statement still running", async () => {
  const { api, sent, running, settle } = controlledApi();
  const { execution } = editor(async () => api);
  const a = execution.run("a", batch(["SELECT 'a'", "SELECT 'a2'"]));
  await execution.run("b", batch(["UPDATE b SET y = 2"], true));
  await settle();
  expect(execution.retireAll()).toBe(1);
  running.get("SELECT 'a'")!.resolve(rows("a"));
  expect(await a).toBe("stopped");
  expect(sent.map((s) => s.sql)).toEqual(["SELECT 'a'"]);
  expect(execution.busy("a") || execution.busy("b")).toBe(false);
});

test("A tab closed while the API is still coming sends nothing: no plan, no Analyze run, no checks", async () => {
  const { api, sent, checked, planning } = controlledApi();
  const ready = deferred<ExecutionApi>();
  const { execution, shown } = editor(() => ready.promise);
  const analyze = execution.explain("plan", "orders", "SELECT * FROM orders", "analyze");
  const change = execution.run("batch", batch(["UPDATE a SET x = 1"], true));
  // Nothing has been sent yet, so nothing is left running for either.
  expect(execution.retire("plan")).toBe(true);
  expect(execution.retire("batch")).toBe(true);
  ready.resolve(api);
  expect(await analyze).toBe("stopped");
  expect(await change).toBe("stopped");
  expect(planning.size).toBe(0);
  expect(sent).toEqual([]);
  expect(checked).toEqual([]);
  expect(shown.map((s) => s.output.kind)).toEqual(["busy"]);
});
