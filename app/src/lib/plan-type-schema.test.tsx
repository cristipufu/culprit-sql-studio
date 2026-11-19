import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import english from "../../../locales/en.json";
import { PlanTypeSchema } from "../components/PlanTypeSchema";
import type { Plan } from "../gen/Plan";
import { currentTypeSchema, typeSchemaPlan } from "../test/plan-type-schema";

function render(plan: Plan): string {
  return renderToStaticMarkup(<PlanTypeSchema plan={plan} />);
}

function table(html: string, name: string): string {
  return html.match(new RegExp(`<table class="plan-type-schema-${name}"[^>]*>[\\s\\S]*?<\\/table>`))?.[0] ?? "";
}

test("current catalog context starts collapsed and keeps backend scope, native bytes and parameter links", () => {
  const plan = typeSchemaPlan();
  plan.statement = "SELECT hidden_full_sql_canary FROM dbo.secret";
  const html = render(plan);
  expect(html).toContain('<details class="ins-context plan-type-schema">');
  expect(html).not.toContain(" open=");
  expect(html).toContain(english["view.plan_type_schema.title"]);
  expect(html).toContain(plan.view!.current_type_schema!.summary);
  expect(html).toContain(english["view.plan_type_schema.limit.current"]);
  expect(html).toContain("@Rows");
  expect(html).toContain("@MoreRows");
  expect(html.match(/href="#[^"]+-type-0"/g)).toHaveLength(2);
  const columns = table(html, "columns");
  expect(columns).toContain("sys.nvarchar");
  expect(columns).toContain("<td>128</td>");
  expect(columns).toContain("<td>-1</td>");
  expect(columns).toContain("<td>18</td><td>3</td>");
  expect(columns).toContain("Latin1_General_100_BIN2");
  expect(html).not.toContain("nvarchar(64)");
  expect(html).not.toContain("hidden_full_sql_canary");
  expect(html).not.toContain("2026-11-14T12:00:00.003");
});

test("index metadata preserves native kind, zero key ordinals, descending and included flags independently", () => {
  const html = render(typeSchemaPlan());
  const indexes = table(html, "indexes");
  expect(indexes).toContain("NONCLUSTERED HASH");
  expect(indexes).toContain("<td>1</td><td>Yes</td><td>No</td>");
  expect(indexes).toContain("<td>0</td><td>No</td><td>Yes</td>");
  expect(indexes).toContain("<td>0</td><td>No</td><td>No</td>");
  expect(html).toContain(`${english["ui.plan_type_schema.memory_optimized"]}</dt><dd>Yes</dd>`);
});

test("unavailable statuses retain backend words and missing view or facts cannot borrow another schema", () => {
  for (const status of ["restricted", "not-collected", "oversized", "changed"] as const) {
    const schema = currentTypeSchema();
    schema.status = status;
    schema.module = null;
    schema.parameters = [];
    schema.types = [];
    const plan = typeSchemaPlan(schema);
    plan.view!.current_type_schema!.summary = `Backend context: ${status}`;
    const html = render(plan);
    expect(html).toContain(`Backend context: ${status}`);
    expect(html).toContain(english["view.plan_type_schema.limit.current"]);
    expect(html).not.toContain("<table");
  }
  const missingView = typeSchemaPlan();
  missingView.view!.current_type_schema = undefined;
  expect(render(missingView)).toBe("");
  const missingFacts = typeSchemaPlan();
  missingFacts.facts.current_type_schema = undefined;
  expect(render(missingFacts)).toBe("");
  expect(render(typeSchemaPlan(null))).toBe("");
});

test("malformed extra metadata stays bounded by type, parameter, column, index and total link limits", () => {
  const schema = currentTypeSchema();
  const first = schema.types[0]!;
  first.columns = Array.from({ length: 65 }, (_, index) => ({ ...first.columns[0]!, column_id: index + 1, name: index === 64 ? "extra-column-canary" : `column_${index}` }));
  first.indexes = Array.from({ length: 9 }, (_, index) => ({ ...first.indexes[0]!, index_id: index + 1, name: index === 8 ? "extra-index-canary" : `index_${index}`,
    columns: Array.from({ length: 65 }, (_, ordinal) => ({ ...first.indexes[0]!.columns[0]!, index_column_id: ordinal + 1, name: ordinal === 64 ? "extra-link-canary" : `link_${ordinal}` })),
  }));
  schema.types = Array.from({ length: 9 }, (_, index) => ({ ...first, user_type_id: index + 257, name: index === 8 ? "extra-type-canary" : `type_${index}` }));
  schema.parameters = Array.from({ length: 17 }, (_, index) => ({ parameter_id: index + 1, name: index === 16 ? "extra-parameter-canary" : `@Rows${index}`, user_type_id: 257 }));
  const html = render(typeSchemaPlan(schema));
  for (const canary of ["extra-column-canary", "extra-index-canary", "extra-link-canary", "extra-type-canary", "extra-parameter-canary"]) expect(html).not.toContain(canary);
  expect(html.match(/class="plan-type-schema-type"/g)).toHaveLength(8);
  expect(table(html, "columns").match(/<tr/g)).toHaveLength(65);
  expect(table(html, "indexes").match(/<tr/g)).toHaveLength(72);
  expect(html).toContain(english["ui.plan_type_schema.limited"]);
});

test("native names and backend text remain escaped text without copying full plan SQL or payload properties", () => {
  const schema = currentTypeSchema();
  schema.types[0]!.name = '<img src=x onerror="window.schemaXss=true">';
  schema.types[0]!.columns[0]!.name = '<script>window.schemaXss=true</script>';
  schema.types[0]!.indexes[0]!.native_type = '<iframe src="javascript:alert(1)">';
  const plan = typeSchemaPlan(schema);
  plan.view!.current_type_schema!.title = "<script>backend_context_xss</script>";
  plan.statement = "SELECT hidden_full_sql_canary";
  const html = render(plan);
  expect(html).toContain("&lt;script&gt;");
  expect(html).not.toContain("<script>");
  expect(html).not.toContain("<img");
  expect(html).not.toContain("<iframe");
  expect(html).not.toContain("hidden_full_sql_canary");
});
