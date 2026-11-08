import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { WorkloadUnlock } from "../components/WorkloadUnlock";
import type { Report } from "../gen/Report";
import { partial } from "../test/fixtures";

function report(action: boolean): Report {
  return partial<Report>({
    snapshot: { connection: "shop", workload: null, server: { engine: "postgres", missing: [{
      capability: "pg-stat-statements", reason: "La collecte n’est pas disponible.",
      fix: "An instruction mentions CREATE TABLE example; but offers no such action.",
      action: action ? { sql: "CREATE EXTENSION pg_stat_statements;", detail: "Aucune reprise nécessaire." } : undefined,
    }] } },
    summary: { no_workload: "A differently worded summary.", no_workload_capability: "pg-stat-statements" },
  });
}

test("workload unlock follows capability identity and explicit SQL, preserving translated instructions", () => {
  const html = renderToStaticMarkup(<WorkloadUnlock report={report(true)} />);
  expect(html).toContain("CREATE EXTENSION pg_stat_statements;");
  expect(html).toContain("Aucune reprise nécessaire.");
  expect(html).not.toContain("CREATE TABLE example;");
  expect(html).toContain("La collecte n’est pas disponible.");
  const textOnly = renderToStaticMarkup(<WorkloadUnlock report={report(false)} />);
  expect(textOnly).toContain("An instruction mentions CREATE TABLE example;");
  expect(textOnly).not.toContain("unlock-sql");
});
