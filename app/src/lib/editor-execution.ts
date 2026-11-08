import type { ExplainMode } from "../gen/ExplainMode";
import type { Plan } from "../gen/Plan";
import { type Api, type ApiError, errorMessage } from "./api";
import { type Batch, type PendingRun, type RunApi, type StatementResult, runBatch } from "./run-batch";

/** What a tab's last action left below the editor. */
export type Output =
  | { kind: "none" }
  | { kind: "busy"; label: string }
  | { kind: "plan"; plan: Plan }
  | { kind: "rows"; results: StatementResult[] }
  | { kind: "error"; error: ApiError };

/** What the editor's actions need from the API. */
export type ExecutionApi = RunApi & Pick<Api, "explain">;

/**
 * What became of asking a tab to act: refused because it is busy, shown (rows, a plan or an error), asked about first
 * (a production change waiting for a yes), or stopped because the tab was retired before it finished.
 */
export type Acted = "busy" | "rows" | "plan" | "error" | "asked" | "stopped";

/** A tab's current operation; while it waits for a yes, the batch it asked about. */
type Owner = { asked: { pending: PendingRun; batch: Batch } | null };

/**
 * The editor's actions, one per tab at a time. A tab's run, explain or pending confirmation owns that tab's output until
 * it ends, and a tab that is busy refuses another. Retiring a tab (closing it, leaving the connection) ends its owner:
 * nothing more is shown for it, a batch sends no further statement, and a question it asked is withdrawn. A statement
 * the database is already running can't be called back: it finishes there.
 */
export class EditorExecution {
  private readonly owners = new Map<string, Owner>();

  constructor(
    private readonly client: () => Promise<ExecutionApi>,
    private readonly show: (tab: string, output: Output) => void,
    /** A tab's question before a production change, or `null` when it no longer stands. */
    private readonly ask: (tab: string, pending: PendingRun | null) => void,
  ) {}

  /** Whether `tab` has an operation running or a question waiting. */
  busy(tab: string): boolean {
    return this.owners.has(tab);
  }

  /** Run `batch` in `tab`. */
  run(tab: string, batch: Batch): Promise<Acted> {
    const owner = this.claim(tab);
    return owner ? this.execute(tab, owner, batch) : Promise.resolve("busy");
  }

  /** Say yes to the change `tab` asked about: its batch runs again from the start, confirmed. */
  confirm(tab: string): Promise<Acted> {
    const owner = this.owners.get(tab);
    const asked = owner?.asked;
    if (!owner || !asked) return Promise.resolve("stopped");
    owner.asked = null;
    this.ask(tab, null);
    return this.execute(tab, owner, { ...asked.batch, confirmed: true });
  }

  /** Say no to the change `tab` asked about: nothing runs, and the tab is free again. */
  cancel(tab: string): void {
    if (!this.owners.get(tab)?.asked) return;
    this.owners.delete(tab);
    this.ask(tab, null);
  }

  /** Plan `sql` in `tab`. No consent travels with a plan request: the engine measures only provable reads on production. */
  async explain(tab: string, connection: string, sql: string, mode: ExplainMode): Promise<Acted> {
    const owner = this.claim(tab);
    if (!owner) return "busy";
    const live = () => this.owners.get(tab) === owner;
    this.show(tab, { kind: "busy", label: mode === "analyze" ? "Running and measuring…" : "Planning…" });
    try {
      const plan = await (await this.client()).explain(connection, sql, mode, false);
      if (!live()) return "stopped";
      this.owners.delete(tab);
      this.show(tab, { kind: "plan", plan });
      return "plan";
    } catch (e) {
      if (!live()) return "stopped";
      this.owners.delete(tab);
      this.show(tab, { kind: "error", error: errorMessage(e) });
      return "error";
    }
  }

  /**
   * End `tab`'s operation: nothing more is shown for it and its batch sends no further statement. Whether a statement
   * may still be running on the database for it (one was sent and hasn't come back).
   */
  retire(tab: string): boolean {
    const owner = this.owners.get(tab);
    if (!owner) return false;
    this.owners.delete(tab);
    if (owner.asked) {
      this.ask(tab, null);
      return false;
    }
    return true;
  }

  /** Retire every tab (the editor is leaving its connection). How many may still have a statement running. */
  retireAll(): number {
    return [...this.owners.keys()].filter((tab) => this.retire(tab)).length;
  }

  private claim(tab: string): Owner | null {
    if (this.owners.has(tab)) return null;
    const owner: Owner = { asked: null };
    this.owners.set(tab, owner);
    return owner;
  }

  private async execute(tab: string, owner: Owner, batch: Batch): Promise<Acted> {
    const live = () => this.owners.get(tab) === owner;
    let api: ExecutionApi;
    try {
      api = await this.client();
    } catch (e) {
      if (!live()) return "stopped";
      this.owners.delete(tab);
      this.show(tab, { kind: "error", error: errorMessage(e) });
      return "error";
    }
    let started = false;
    const progress = (label: string) => {
      if (!live()) return;
      started = true;
      this.show(tab, { kind: "busy", label });
    };
    const outcome = await runBatch(api, batch, progress, live);
    if (!live() || outcome.kind === "stopped") return "stopped";
    if (outcome.kind === "confirm") {
      if (started) this.show(tab, { kind: "none" });
      owner.asked = { pending: outcome.pending, batch };
      this.ask(tab, outcome.pending);
      return "asked";
    }
    this.owners.delete(tab);
    if (outcome.kind === "error") {
      this.show(tab, { kind: "error", error: outcome.error });
      return "error";
    }
    this.show(tab, { kind: "rows", results: outcome.results });
    return "rows";
  }
}
