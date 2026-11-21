import type { ProviderAcquisitionReport } from "../gen/ProviderAcquisitionReport";
import type { Report } from "../gen/Report";
import type { NativeIdentity } from "../gen/NativeIdentity";
import { errorMessage, type Api, type ApiError } from "./api";
import { isActionable } from "./findings";
import { LatestRequest } from "./requests";
import { t } from "./i18n";
import { reportMatchesSelection, samePopulation, type ReportSelection } from "./report-selection";

export type ReportState =
  | { status: "none" }
  | { status: "loading" }
  | { status: "ready"; report: Report; scanning: boolean; scanError: ApiError | null; scanAcquisition?: ProviderAcquisitionReport | null }
  | { status: "error"; error: ApiError; acquisition?: ProviderAcquisitionReport | null };

type Context = { active: string | null; windowMinutes: number; workloadPopulation: NativeIdentity | null; report: ReportState; finding: string | null };
type Update = { report: ReportState; finding?: string | null; selection?: ReportSelection };
type Client = Pick<Api, "latestReport" | "reportAt" | "scan">;
type Request = ReportSelection & { current: () => boolean; acquisition?: ProviderAcquisitionReport };
type ReadIntent = "open" | "refresh" | "reread";

/** Saved observations keep their original alias on disk; UI actions address the connection's current name. */
export function withConnectionName(report: Report, name: string): Report {
  return report.snapshot.connection === name ? report : { ...report, snapshot: { ...report.snapshot, connection: name } };
}

/** Own the complete cache → scan lifecycle. Target identity alone cannot distinguish A → B → A or two scans of A. */
export class ReportRequests {
  private readonly owner = new LatestRequest();

  constructor(private readonly options: {
    client: () => Promise<Client>;
    read: () => Context;
    publish: (update: Update) => void;
    collected: () => Promise<unknown>;
    language: () => string;
  }) {}

  invalidate(): void {
    this.owner.invalidate();
  }

  private begin(selection: ReportSelection): Request | null {
    const target = { ...selection, population: selection.population === null ? null : structuredClone(selection.population) };
    const code = this.options.language();
    const matches = () => {
      const current = this.options.read();
      return current.active === target.name && current.windowMinutes === target.windowMinutes
        && samePopulation(current.workloadPopulation, target.population) && this.options.language() === code;
    };
    if (!matches()) return null;
    const owns = this.owner.start();
    return { ...target, current: () => owns() && matches() };
  }

  async open(selection: ReportSelection, intent: ReadIntent): Promise<void> {
    const request = this.begin(selection);
    if (!request) return;
    try {
      const client = await this.options.client();
      if (!request.current()) return;
      const saved = await client.latestReport(request.name, request.windowMinutes, request.population);
      if (!request.current()) return;
      const cached = saved && reportMatchesSelection(saved, request) ? saved : null;
      if (cached) this.accept(cached, intent === "refresh", request.name);
      if (intent === "refresh" || (!cached && intent === "open")) await this.collect(client, request);
      else if (!cached) this.failed(request, { code: "not_found", message: t("ui.population_picker.cached_report_missing"), hint: null });
    } catch (error) {
      this.failed(request, error);
    }
  }

  async scan(selection: ReportSelection): Promise<void> {
    const request = this.begin(selection);
    if (!request) return;
    this.scanning();
    try {
      const client = await this.options.client();
      if (request.current()) await this.collect(client, request);
    } catch (error) {
      this.failed(request, error);
    }
  }

  /** Verification names a sealed report, not whichever report became latest while the result was open. */
  async showVerified(selection: ReportSelection, beforeId: string, afterId: string): Promise<void> {
    const displayed = () => {
      const current = this.options.read().report;
      return current.status === "ready" && current.report.snapshot.id === beforeId;
    };
    if (!displayed()) return;
    const request = this.begin(selection);
    if (!request) return;
    const before = this.options.read().report;
    if (before.status !== "ready") return;
    const expectedPopulation = before.report.snapshot.workload?.population ?? selection.population;
    try {
      const client = await this.options.client();
      if (!request.current() || !displayed()) return;
      const report = await client.reportAt(selection.name, afterId);
      if (!request.current() || !displayed()) return;
      const target: ReportSelection = {
        name: selection.name, windowMinutes: report.snapshot.options?.window_minutes ?? 0,
        population: report.snapshot.options?.workload_population ?? null,
      };
      if (report.snapshot.id !== afterId || target.windowMinutes !== selection.windowMinutes
        || !samePopulation(target.population, expectedPopulation) || !reportMatchesSelection(report, target)) {
        this.failed(request, { code: "invalid_input", message: t("ui.population_picker.report_scope_mismatch"), hint: null });
        return;
      }
      const selected = this.options.read().finding;
      const finding = selected && report.findings.some((value) => value.id === selected)
        ? selected : report.findings.find(isActionable)?.id ?? null;
      this.options.publish({ selection: target, report: { status: "ready", report: withConnectionName(report, target.name), scanning: false, scanError: null }, finding });
    } catch (error) {
      if (displayed()) this.failed(request, error);
    }
  }

  private scanning(): void {
    const current = this.options.read().report;
    if (current.status === "loading" || (current.status === "ready" && current.scanning && !current.scanError)) return;
    this.options.publish({ report: current.status === "ready"
      ? { ...current, scanning: true, scanError: null, scanAcquisition: null } : { status: "loading" } });
  }

  private accept(report: Report, scanning: boolean, name: string): void {
    const selected = this.options.read().finding;
    const finding = selected && report.findings.some((f) => f.id === selected)
      ? selected : report.findings.find(isActionable)?.id ?? null;
    this.options.publish({ report: { status: "ready", report: withConnectionName(report, name), scanning, scanError: null }, finding });
  }

  private async collect(client: Client, request: Request): Promise<void> {
    if (!request.current()) return;
    this.scanning();
    if (!request.current()) return;
    const report = await client.scan(request.name, request.windowMinutes, request.population, (acquisition) => {
      if (request.current()) request.acquisition = acquisition;
    });
    if (!request.current()) return;
    if (!reportMatchesSelection(report, request)) {
      this.failed(request, { code: "invalid_input", message: t("ui.population_picker.report_scope_mismatch"), hint: null });
      return;
    }
    this.accept(report, false, request.name);
    // Sidebar metadata has its own request owner; a refresh failure cannot invalidate a successful scan.
    void Promise.resolve().then(() => this.options.collected()).catch(() => {});
  }

  private failed(request: Request, error: unknown): void {
    if (!request.current()) return;
    const previous = this.options.read().report;
    const report: ReportState = previous.status === "ready"
      ? { ...previous, scanning: false, scanError: errorMessage(error), scanAcquisition: request.acquisition ?? null }
      : { status: "error", error: errorMessage(error), acquisition: request.acquisition ?? null };
    this.options.publish({ report });
  }
}
