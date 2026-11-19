import examples from "../../../fixtures/plan-view.sample.json" with { type: "json" };
import english from "../../../locales/en.json" with { type: "json" };
import type { MssqlCurrentTypeSchema } from "../gen/MssqlCurrentTypeSchema";
import type { Plan } from "../gen/Plan";
import { fromRust } from "./fixtures";

/** Supplied current-catalog display data over a Rust-produced tree, not collection or compilation proof. */
export function currentTypeSchema(): MssqlCurrentTypeSchema {
  return {
    observed_at: "2026-11-19T12:00:00.1234567Z", status: "available",
    module: {
      identity: { database_id: 7, object_id: 1001, created_without_offset: "2026-11-14T12:00:00.003" },
      schema: "dbo", name: "consume", modified_without_offset: "2026-11-19T11:00:00.007",
    },
    parameters: [{ parameter_id: 1, name: "@Rows", user_type_id: 257 }, { parameter_id: 2, name: "@MoreRows", user_type_id: 257 }],
    types: [{
      schema_id: 1, user_type_id: 257, schema: "dbo", name: "RowsType",
      backing_object: { database_id: 7, object_id: 2001, created_without_offset: "2026-11-19T11:00:00.003" },
      modified_without_offset: "2026-11-19T11:00:00.007", memory_optimized: true,
      columns: [
        { column_id: 1, name: "München", user_type_id: 231, system_type_id: 231, type_schema: "sys", type_name: "nvarchar", max_length: 128, precision: 0, scale: 0, nullable: false, identity: false, computed: false, collation: "Latin1_General_100_BIN2" },
        { column_id: 2, name: "payload", user_type_id: 165, system_type_id: 165, type_schema: "sys", type_name: "varbinary", max_length: -1, precision: 0, scale: 0, nullable: true, identity: false, computed: false, collation: null },
        { column_id: 3, name: "amount", user_type_id: 106, system_type_id: 106, type_schema: "sys", type_name: "decimal", max_length: 9, precision: 18, scale: 3, nullable: false, identity: false, computed: false, collation: null },
      ],
      indexes: [
        { index_id: 1, name: "ix_rows", type_code: 2, native_type: "NONCLUSTERED", unique: true, primary_key: true, unique_constraint: false, disabled: false, hypothetical: false,
          columns: [
            { index_column_id: 1, column_id: 1, name: "München", key_ordinal: 1, descending: true, included: false },
            { index_column_id: 2, column_id: 2, name: "payload", key_ordinal: 0, descending: false, included: true },
          ] },
        { index_id: 2, name: "ix_hash", type_code: 7, native_type: "NONCLUSTERED HASH", unique: false, primary_key: false, unique_constraint: false, disabled: false, hypothetical: false,
          columns: [{ index_column_id: 1, column_id: 1, name: "München", key_ordinal: 0, descending: false, included: false }] },
      ],
    }],
  };
}

export function typeSchemaPlan(schema: MssqlCurrentTypeSchema | null = currentTypeSchema()): Plan {
  const sample = examples.find((value) => value.name === "retained-old");
  if (!sample) throw new Error("Missing Rust retained plan fixture");
  const plan = fromRust<{ plan: Plan }>(sample).plan;
  if (!plan.view) throw new Error("Missing Rust plan display");
  plan.facts.current_type_schema = schema ?? undefined;
  plan.view.current_type_schema = schema ? {
    title: english["view.plan_type_schema.title"],
    summary: `Metadata for 1 table type was read at ${schema.observed_at}.`,
    limitations: [english["view.plan_type_schema.limit.current"]],
  } : undefined;
  return plan;
}
