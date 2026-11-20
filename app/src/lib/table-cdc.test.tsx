import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { Engine } from "../gen/Engine";
import type { SourceState } from "../gen/SourceState";
import type { TableStat } from "../gen/TableStat";
import { partial } from "../test/fixtures";
import { CdcEnrollment } from "../views/schema-table/CdcEnrollment";
import { t } from "./i18n";

function render(tracked: boolean | null, sourceState: SourceState | null = "collected", engine: Engine = "mssql") {
  return renderToStaticMarkup(<CdcEnrollment engine={engine} sourceState={sourceState} table={partial<TableStat>({ mssql_tracked_by_cdc: tracked })} />);
}

test("captured enrollment keeps enabled, disabled and native NULL separate without a job-health verdict", () => {
  for (const [tracked, text] of [[true, t("ui.common.yes")], [false, t("ui.common.no")], [null, t("ui.indexes.columnstore.native_null")]] as const) {
    const html = render(tracked);
    expect(html).toContain(`<dd>${text}</dd>`);
    expect(html).toContain(t("view.table_cdc.qualification"));
    expect(html).not.toContain(" open=");
    expect(html).not.toContain('class="sev');
  }
});

test("unavailable or foreign catalogs withhold enrollment and partial or unknown sources keep their qualification", () => {
  for (const state of ["unavailable", "empty", "not-applicable"] as const) expect(render(true, state)).toBe("");
  expect(render(true, "collected", "postgres")).toBe("");
  expect(render(true, "partial")).toContain(t("view.table_catalog.partial"));
  expect(render(true, null)).toContain(t("view.table_catalog.unknown_source"));
});
