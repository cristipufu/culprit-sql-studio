import { test } from "bun:test";
import type { Report } from "../gen/Report";
import type { NativeIdentity } from "../gen/NativeIdentity";
import type { Api } from "./api";
import { ReportRequests, type ReportState } from "./report-requests";
import { LatestRequest, lazyResource } from "./requests";
import { partial } from "../test/fixtures";

function equal(actual: unknown, expected: unknown, why: string): void {
  if (JSON.stringify(actual) !== JSON.stringify(expected)) throw new Error(`${why}: ${JSON.stringify(actual)}`);
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}
const tick = () => new Promise<void>((resolve) => setTimeout(resolve, 0));
const report = (id: string, windowMinutes = 60, population: NativeIdentity | null = null) => partial<Report>({ snapshot: {
  id, connection: "a", options: { window_minutes: windowMinutes, workload_population: population ?? undefined },
  workload: population ? { population } : null,
}, findings: [
  { id: "info", actionability: "information" }, { id: "issue", actionability: "needs-attention" },
] });

type Client = Pick<Api, "latestReport" | "scan"> & Partial<Pick<Api, "reportAt">>;
function harness(client: () => Promise<Client>, collected: () => Promise<unknown> = async () => {}) {
  let code = "en";
  let state: { active: string | null; windowMinutes: number; workloadPopulation: NativeIdentity | null; report: ReportState; finding: string | null } = {
    active: "a", windowMinutes: 60, workloadPopulation: null, report: { status: "loading" }, finding: null,
  };
  const requests = new ReportRequests({ client: async () => {
    const supplied = await client();
    return { ...supplied, reportAt: supplied.reportAt ?? (async () => { throw new Error("unexpected exact report read"); }) };
  }, read: () => state,
    publish: (update) => {
      const { selection, ...next } = update;
      state = { ...state, ...next, ...(selection ? { active: selection.name, windowMinutes: selection.windowMinutes, workloadPopulation: selection.population } : {}) };
    }, collected, language: () => code });
  return { requests, read: () => state,
    target(name: string, minutes = 60, population: NativeIdentity | null = null) {
      requests.invalidate();
      state = { ...state, active: name, windowMinutes: minutes, workloadPopulation: population, report: { status: "loading" }, finding: null };
    },
    language(next: string) { requests.invalidate(); code = next; },
    seed(value: Report) { state.report = { status: "ready", report: value, scanning: false, scanError: null }; },
    select(id: string) { state.finding = id; },
    visible() {
      const value = state.report;
      return value.status === "ready" ? [value.report.snapshot.id, value.scanning, value.scanError?.message ?? null]
        : [value.status, value.status === "error" ? value.error.message : null];
    },
  };
}

test("Report requests: navigation, windows, overlapping scans, cache races and failures retain the current owner", async () => {
  // Re-entering the same connection must not revive its previous request, including stale failures.
  for (const failOld of [false, true]) {
    const old = deferred<Report | null>(), other = deferred<Report | null>(), current = deferred<Report | null>();
    const queue = [old, other, current];
    const h = harness(async () => ({ latestReport: () => queue.shift()!.promise,
      scan: async () => { throw new Error("unexpected scan"); } }));
    const a = h.requests.open({ name: "a", windowMinutes: 60, population: null }, "open"); await tick();
    h.target("b"); const b = h.requests.open({ name: "b", windowMinutes: 60, population: null }, "open"); await tick();
    h.target("a"); const again = h.requests.open({ name: "a", windowMinutes: 60, population: null }, "open"); await tick();
    current.resolve(report("current")); await again;
    if (failOld) old.reject(new Error("old failure")); else old.resolve(report("old"));
    other.resolve(report("other")); await Promise.all([a, b]);
    equal(h.visible(), ["current", false, null], "A → B → A revived an old request");
  }

  // Window identity applies to errors as well as successful responses.
  {
    const old = deferred<Report | null>();
    const h = harness(async () => ({ latestReport: async (_name, minutes) => minutes === 60 ? old.promise : report("6h", 360),
      scan: async () => report("unused") }));
    const pending = h.requests.open({ name: "a", windowMinutes: 60, population: null }, "open"); await tick();
    h.target("a", 360); await h.requests.open({ name: "a", windowMinutes: 360, population: null }, "open");
    old.reject(new Error("old window failed")); await pending;
    equal(h.visible(), ["6h", false, null], "A stale window error replaced the selected report");
  }

  // Two scans of the same target finish independently; only the latest request owns the result and spinner.
  for (const failOld of [false, true]) {
    const old = deferred<Report>(), current = deferred<Report>();
    const queue = [old, current];
    const h = harness(async () => ({ latestReport: async () => null, scan: () => queue.shift()!.promise }));
    h.seed(report("cached"));
    const first = h.requests.scan({ name: "a", windowMinutes: 60, population: null }); await tick();
    const second = h.requests.scan({ name: "a", windowMinutes: 60, population: null }); await tick();
    if (failOld) old.reject(new Error("old scan failed")); else old.resolve(report("old scan"));
    await first;
    equal(h.visible(), ["cached", true, null], "An old scan changed a newer scan's state");
    current.resolve(report("fresh")); await second;
    equal(h.visible(), ["fresh", false, null], "The current scan did not finish");
  }

  // A late cache cannot replace a manual refresh or launch another automatic scan.
  {
    const cache = deferred<Report | null>(); let scans = 0;
    const h = harness(async () => ({ latestReport: () => cache.promise,
      scan: async () => { scans++; return report("manual"); } }));
    const opening = h.requests.open({ name: "a", windowMinutes: 60, population: null }, "refresh"); await tick();
    await h.requests.scan({ name: "a", windowMinutes: 60, population: null });
    cache.resolve(report("cache")); await opening;
    equal(h.visible(), ["manual", false, null], "A late cache replaced a completed scan");
    equal(scans, 1, "A superseded cache launched another scan");
  }

  // A failed refresh retains usable cached data and the current selection; API bootstrap failures are handled too.
  {
    const scan = deferred<Report>();
    const h = harness(async () => ({ latestReport: async () => report("cache"), scan: () => scan.promise }));
    const opening = h.requests.open({ name: "a", windowMinutes: 60, population: null }, "refresh"); await tick();
    equal(h.visible(), ["cache", true, null], "Cache was not shown during refresh");
    equal(h.read().finding, "issue", "An informational finding became the default selection");
    scan.reject(new Error("offline")); await opening;
    equal(h.visible(), ["cache", false, "offline"], "A refresh failure discarded usable cached data");
    const broken = harness(async () => { throw new Error("API bootstrap failed"); });
    await broken.requests.open({ name: "a", windowMinutes: 60, population: null }, "open");
    equal(broken.visible(), ["error", "API bootstrap failed"], "API bootstrap rejection escaped the lifecycle");
  }

  // Ancillary refresh failures do not turn successful scans into failures, and stale views cannot initiate work.
  {
    let scans = 0, refreshed = 0;
    const cache = deferred<Report | null>();
    const h = harness(async () => ({ latestReport: () => cache.promise,
      scan: async () => { scans++; return report("fresh"); } }), async () => { refreshed++; throw new Error("sidebar failed"); });
    const pending = h.requests.open({ name: "a", windowMinutes: 60, population: null }, "open"); await tick();
    await h.requests.scan({ name: "b", windowMinutes: 60, population: null });
    cache.resolve(report("cache")); await pending;
    equal(h.visible(), ["cache", false, null], "An inactive view superseded the current request");
    equal(scans, 0, "An inactive view launched a scan");
    await h.requests.scan({ name: "a", windowMinutes: 60, population: null }); await tick();
    equal(h.visible(), ["fresh", false, null], "A sidebar failure invalidated a successful scan");
    equal(refreshed, 1, "Accepted scans did not refresh connection metadata");
  }

  {
    const owner = new LatestRequest(); const first = owner.start(), second = owner.start();
    equal([first(), second()], [false, true], "List request ownership is ambiguous");
    owner.invalidate(); equal(second(), false, "Invalidation revived an old owner");
  }
});

test("API initialization retries after failure; retired cache continuations launch no scan", async () => {
  // API initialization is shared while pending, retryable after rejection, and cached after success.
  {
    const initial = deferred<number>(); let attempts = 0;
    const resource = lazyResource(() => ++attempts === 1 ? initial.promise : Promise.resolve(42));
    const first = resource(), same = resource();
    if (first !== same) throw new Error("Concurrent API initialization was duplicated");
    initial.reject(new Error("temporary failure"));
    await Promise.allSettled([first, same]);
    equal(await resource(), 42, "API initialization could not recover after failure");
    equal(await resource(), 42, "Successful API initialization was not cached");
    equal(attempts, 2, "Resource initialization ran again after success");
  }

  // Navigation triggered by a state subscriber must retire the cache-to-scan continuation too.
  {
    let scans = 0;
    let state: { active: string; windowMinutes: number; workloadPopulation: NativeIdentity | null; report: ReportState; finding: string | null } = {
      active: "a", windowMinutes: 60, workloadPopulation: null, report: { status: "loading" }, finding: null,
    };
    const requests = new ReportRequests({ read: () => state,
      client: async () => ({ latestReport: async () => report("cache"), scan: async () => { scans++; return report("fresh"); },
        reportAt: async () => { throw new Error("unexpected exact report read"); } }),
      publish: (update) => { state = { ...state, ...update }; requests.invalidate(); state.active = "b"; },
      collected: async () => {},
      language: () => "en",
    });
    await requests.open({ name: "a", windowMinutes: 60, population: null }, "refresh");
    equal(scans, 0, "A retired cache continuation started another scan");
  }
});

test("Verification opens its exact report reference and atomically adopts its actual group without latest-cache fallback", async () => {
  const population: NativeIdentity = { id: "17", qualifiers: {} };
  const before = report("before");
  before.snapshot.workload = report("actual", 60, population).snapshot.workload;
  const exactReads: string[] = [];
  const h = harness(async () => ({
    latestReport: async () => { throw new Error("verification must not use latest"); },
    scan: async () => { throw new Error("showing verification must not scan"); },
    reportAt: async (name, id) => { exactReads.push(`${name}:${id}`); return report(id, 60, population); },
  }));
  h.seed(before);
  await h.requests.showVerified({ name: "a", windowMinutes: 60, population: null }, "before", "verified");
  equal(h.visible(), ["verified", false, null], "The exact verified report was not shown");
  equal([h.read().workloadPopulation, h.read().windowMinutes, exactReads], [population, 60, ["a:verified"]], "The actual verified scope or immutable reference was replaced");
  equal(before.snapshot.options?.workload_population, undefined, "Adoption mutated the original default snapshot");

  for (const wrong of [report("newer latest", 60, population), report("verified", 360, population), report("verified", 60, { id: "18", qualifiers: {} })]) {
    const invalid = harness(async () => ({ latestReport: async () => null, scan: async () => report("wrong"), reportAt: async () => wrong }));
    invalid.seed(before);
    await invalid.requests.showVerified({ name: "a", windowMinutes: 60, population: null }, "before", "verified");
    const current = invalid.read().report;
    if (current.status !== "ready") throw new Error("The prior scoped report was discarded");
    equal([current.report.snapshot.id, current.scanError?.code], ["before", "invalid_input"], "A wrong exact reference/group/window was admitted");
  }
});

test("A delayed verified artifact cannot switch a newly selected group or returning report session", async () => {
  const population: NativeIdentity = { id: "17", qualifiers: {} };
  const other: NativeIdentity = { id: "18", qualifiers: {} };
  const pending = deferred<Report>();
  const h = harness(async () => ({ latestReport: async () => null, scan: async () => report("unused"), reportAt: () => pending.promise }));
  h.target("a", 60, population); h.seed(report("before", 60, population));
  const showing = h.requests.showVerified({ name: "a", windowMinutes: 60, population }, "before", "verified"); await tick();
  h.target("a", 60, other); h.seed(report("other group", 60, other));
  pending.resolve(report("verified", 60, population)); await showing;
  equal([h.read().workloadPopulation, h.visible()], [other, ["other group", false, null]], "An old verified result changed the new group");
});

test("Report actions follow the requested alias without changing saved observations", async () => {
  // Historical reports retain their saved alias; both cache and scan results route actions through the current name.
  {
    const cached = report("before rename"), fresh = report("after rename");
    const h = harness(async () => ({ latestReport: async () => cached, scan: async () => fresh }));
    h.target("renamed");
    await h.requests.open({ name: "renamed", windowMinutes: 60, population: null }, "open");
    const connection = () => {
      const value = h.read().report;
      return value.status === "ready" ? value.report.snapshot.connection : null;
    };
    equal(connection(), "renamed", "Cached report actions still use the previous alias");
    await h.requests.scan({ name: "renamed", windowMinutes: 60, population: null });
    equal(connection(), "renamed", "Scan actions still use the previous alias");
    equal([cached.snapshot.connection, fresh.snapshot.connection], ["a", "a"], "UI alias binding mutated saved observations");
  }
});

test("Exact execution groups retain separate caches and cannot revive an old group request", async () => {
  const first: NativeIdentity = { id: "17", qualifiers: { context: "first", generation: "9007199254740993" } };
  const second: NativeIdentity = { id: "17", qualifiers: { context: "second", generation: "9007199254740993" } };
  for (const failOld of [false, true]) {
    const retired = deferred<Report | null>(), other = deferred<Report | null>(), selected = deferred<Report | null>();
    const queue = [retired, other, selected];
    let scans = 0;
    const h = harness(async () => ({ latestReport: () => queue.shift()!.promise,
      scan: async () => { scans++; throw new Error("cached groups require no scan"); } }));
    h.target("a", 60, first);
    const a = h.requests.open({ name: "a", windowMinutes: 60, population: first }, "open"); await tick();
    h.target("a", 60, second);
    const b = h.requests.open({ name: "a", windowMinutes: 60, population: second }, "open"); await tick();
    h.target("a", 60, first);
    const again = h.requests.open({ name: "a", windowMinutes: 60, population: first }, "open"); await tick();
    selected.resolve(report("current group", 60, first)); await again;
    if (failOld) retired.reject(new Error("old group failed")); else retired.resolve(report("retired group", 60, first));
    other.resolve(report("other group", 60, second)); await Promise.all([a, b]);
    equal(h.visible(), ["current group", false, null], "Same-id groups or a returning group borrowed a retired response");
    equal(scans, 0, "Switching exact cached groups launched extra native work");
  }
});

test("Default requested reports stay distinct from explicit groups even when their actual native population agrees", async () => {
  const population: NativeIdentity = { id: "17", qualifiers: { group: "native" } };
  const requests: (NativeIdentity | null)[] = [];
  let scans = 0;
  const h = harness(async () => ({
    latestReport: async (_name, minutes, selected) => {
      requests.push(selected);
      const cached = report(selected ? "explicit" : "default", minutes ?? 60, selected);
      if (!selected) cached.snapshot.workload = report("actual", 60, population).snapshot.workload;
      return cached;
    },
    scan: async () => { scans++; throw new Error("unexpected native scan"); },
  }));
  await h.requests.open({ name: "a", windowMinutes: 60, population: null }, "open");
  equal(h.visible(), ["default", false, null], "An actual source group changed the requested default cache");
  h.target("a", 60, population);
  await h.requests.open({ name: "a", windowMinutes: 60, population }, "open");
  equal(h.visible(), ["explicit", false, null], "The explicit cache borrowed the default view");
  h.target("a");
  await h.requests.open({ name: "a", windowMinutes: 60, population: null }, "open");
  equal([requests, scans], [[null, population, null], 0], "Default and exact lookup namespaces changed");
});

test("A missing or mismatched cache never turns a language reread into source work or publishes another group", async () => {
  const population: NativeIdentity = { id: "17", qualifiers: {} };
  const unqualified = report("request intent without actual population", 60, population);
  unqualified.snapshot.workload!.population = undefined;
  const contradicted = report("another actual native population", 60, population);
  contradicted.snapshot.workload!.population = { id: "18", qualifiers: {} };
  for (const wrong of [null, report("wrong group"), report("wrong window", 360, population), unqualified, contradicted]) {
    let scans = 0;
    const h = harness(async () => ({ latestReport: async () => wrong,
      scan: async () => { scans++; return report("wrong live group"); } }));
    h.target("a", 60, population);
    const selection = { name: "a", windowMinutes: 60, population };
    await h.requests.open(selection, "reread");
    equal(scans, 0, "Cache-only wording refresh started native work");
    equal(h.read().report.status, "error", "Unowned cache data was admitted");
    h.seed(report("selected cache", 60, population));
    await h.requests.scan(selection);
    const current = h.read().report;
    if (current.status !== "ready") throw new Error("A failed scan discarded the selected cache");
    equal([current.report.snapshot.id, current.scanError?.code], ["selected cache", "invalid_input"], "Wrong live evidence replaced the exact selected cache");
  }
});

test("Language ownership retires pending cache and native results before backend language application finishes", async () => {
  const old = deferred<Report | null>(); let scans = 0;
  const h = harness(async () => ({ latestReport: () => old.promise,
    scan: async () => { scans++; return report("wrong language"); } }));
  const pending = h.requests.open({ name: "a", windowMinutes: 60, population: null }, "refresh"); await tick();
  h.language("fr");
  old.resolve(report("retired English")); await pending;
  equal([h.read().report.status, scans], ["loading", 0], "A retired language cache published or launched a scan during language setup");

  const first = deferred<Report>(), current = deferred<Report>(); const queue = [first, current];
  const native = harness(async () => ({ latestReport: async () => null, scan: () => queue.shift()!.promise }));
  const english = native.requests.scan({ name: "a", windowMinutes: 60, population: null }); await tick();
  native.language("fr"); native.language("en");
  const newEnglish = native.requests.scan({ name: "a", windowMinutes: 60, population: null }); await tick();
  current.resolve(report("current English")); await newEnglish;
  first.reject(new Error("retired English failure")); await english;
  equal(native.visible(), ["current English", false, null], "A returning language revived a retired native result");
});
