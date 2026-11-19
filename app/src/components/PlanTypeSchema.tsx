import { ChevronDown, TableProperties } from "lucide-react";
import { useId, type ReactNode } from "react";
import type { MssqlCurrentTableType } from "../gen/MssqlCurrentTableType";
import type { Plan } from "../gen/Plan";
import { digits } from "../lib/format";
import { t } from "../lib/i18n";
import "./plan-type-schema.css";

const MAX_TYPES = 8;
const MAX_PARAMETERS = 16;
const MAX_COLUMNS = 64;
const MAX_INDEXES = 8;
const MAX_INDEX_COLUMNS = 64;
const MAX_LIMITATIONS = 4;
const MAX_NATIVE_TEXT = 256;

/** The backend owns association and wording; these are separately timed current catalog fields. */
export function PlanTypeSchema({ plan }: { plan: Plan }) {
  const id = useId();
  const schema = plan.facts.current_type_schema;
  const view = plan.view?.current_type_schema;
  if (!schema || !view) return null;
  const types = schema.types.slice(0, MAX_TYPES);
  const parameters = schema.parameters.slice(0, MAX_PARAMETERS);
  const available = schema.status === "available";
  const limited = schema.types.length > MAX_TYPES || schema.parameters.length > MAX_PARAMETERS
    || types.some((type) => type.columns.length > MAX_COLUMNS || type.indexes.length > MAX_INDEXES
      || type.indexes.reduce((count, index) => count + index.columns.length, 0) > MAX_INDEX_COLUMNS);
  return <details className="ins-context plan-type-schema" key={`${schema.observed_at}:${schema.status}:${schema.module?.identity.object_id}`}>
    <summary><TableProperties size={13} />{view.title}<ChevronDown size={13} className="ins-context-chevron" /></summary>
    <div className="plan-type-schema-body">
      <p>{view.summary}</p>
      {view.limitations.slice(0, MAX_LIMITATIONS).map((limitation, index) => <p className="muted" key={index}>{limitation}</p>)}
      {available && <>
        {schema.module && <dl><dt>{t("ui.plan_type_schema.module")}</dt><dd><code>{qualified(schema.module)}</code></dd></dl>}
        {parameters.length > 0 && <TableRegion label={t("ui.plan_type_schema.parameters")}>
          <table><caption>{t("ui.plan_type_schema.parameters")}</caption><thead><tr>
            <th scope="col">{t("ui.plan_type_schema.parameter")}</th><th scope="col">{t("ui.plan_type_schema.table_type")}</th>
          </tr></thead><tbody>{parameters.map((parameter, position) => {
            const matches = types.map((type, index) => ({ type, index })).filter(({ type }) => type.user_type_id === parameter.user_type_id);
            const match = matches.length === 1 ? matches[0] : null;
            return <tr key={`${parameter.parameter_id}:${position}`}><td><code>{nativeText(parameter.name)}</code></td><td>
              {match ? <a href={`#${id}-type-${match.index}`}>{qualified(match.type)}</a> : t("ui.common.unknown")}
            </td></tr>;
          })}</tbody></table>
        </TableRegion>}
        {types.map((type, index) => <TypeSchema key={`${type.user_type_id}:${index}`} type={type} id={`${id}-type-${index}`} />)}
      </>}
      {limited && <p className="muted">{t("ui.plan_type_schema.limited")}</p>}
    </div>
  </details>;
}

function TypeSchema({ type, id }: { type: MssqlCurrentTableType; id: string }) {
  const columns = type.columns.slice(0, MAX_COLUMNS);
  let remaining = MAX_INDEX_COLUMNS;
  const indexes = type.indexes.slice(0, MAX_INDEXES).map((index) => {
    const links = index.columns.slice(0, remaining);
    remaining -= links.length;
    return { index, links };
  });
  return <section className="plan-type-schema-type" id={id}>
    <h4><code>{qualified(type)}</code></h4>
    <dl><dt>{t("ui.plan_type_schema.memory_optimized")}</dt><dd>{yesNo(type.memory_optimized)}</dd></dl>
    <TableRegion label={t("ui.plan_type_schema.columns")}>
      <table className="plan-type-schema-columns"><caption>{t("ui.plan_type_schema.columns")}</caption><thead><tr>
        <th scope="col">{t("ui.plan_type_schema.column")}</th><th scope="col">{t("ui.plan_type_schema.data_type")}</th>
        <th scope="col">{t("ui.plan_type_schema.length_bytes")}</th><th scope="col">{t("ui.plan_type_schema.precision")}</th>
        <th scope="col">{t("ui.plan_type_schema.scale")}</th><th scope="col">{t("ui.plan_type_schema.nullable")}</th>
        <th scope="col">{t("ui.plan_type_schema.collation")}</th>
      </tr></thead><tbody>{columns.map((column, index) => <tr key={`${column.column_id}:${index}`}>
        <td><code>{nativeText(column.name)}</code></td><td><code>{qualified({ schema: column.type_schema, name: column.type_name })}</code></td>
        <td>{digits(String(column.max_length))}</td><td>{digits(String(column.precision))}</td><td>{digits(String(column.scale))}</td>
        <td>{yesNo(column.nullable)}</td><td>{column.collation == null ? t("ui.common.unknown") : nativeText(column.collation)}</td>
      </tr>)}</tbody></table>
    </TableRegion>
    {indexes.length > 0 && <TableRegion label={t("ui.plan_type_schema.indexes")}>
      <table className="plan-type-schema-indexes"><caption>{t("ui.plan_type_schema.indexes")}</caption><thead><tr>
        <th scope="col">{t("ui.plan_type_schema.index")}</th><th scope="col">{t("ui.plan_type_schema.native_kind")}</th><th scope="col">{t("ui.plan_type_schema.unique")}</th>
        <th scope="col">{t("ui.plan_type_schema.column")}</th><th scope="col">{t("ui.plan_type_schema.key_ordinal")}</th>
        <th scope="col">{t("ui.plan_type_schema.descending")}</th><th scope="col">{t("ui.plan_type_schema.included")}</th>
      </tr></thead><tbody>{indexes.flatMap(({ index, links }, position) => (links.length ? links : [null]).map((column, ordinal) => <tr key={`${index.index_id}:${position}:${ordinal}`}>
        {ordinal === 0 && <><td rowSpan={Math.max(links.length, 1)}><code>{nativeText(index.name)}</code></td>
          <td rowSpan={Math.max(links.length, 1)}>{nativeText(index.native_type)}</td><td rowSpan={Math.max(links.length, 1)}>{yesNo(index.unique)}</td></>}
        {column ? <><td><code>{nativeText(column.name)}</code></td><td>{digits(String(column.key_ordinal))}</td>
          <td>{yesNo(column.descending)}</td><td>{yesNo(column.included)}</td></> : <td colSpan={4}>{t("ui.common.unknown")}</td>}
      </tr>))}</tbody></table>
    </TableRegion>}
  </section>;
}

function TableRegion({ label, children }: { label: string; children: ReactNode }) {
  return (
    // biome-ignore lint/a11y/noNoninteractiveTabindex: a scrolling table region takes focus so the keyboard can scroll it
    <div className="plan-type-schema-scroll" role="region" aria-label={label} tabIndex={0}>{children}</div>
  );
}

function yesNo(value: boolean): string {
  return t(value ? "ui.common.yes" : "ui.common.no");
}

function qualified(value: { schema: string; name: string }): string {
  return `${nativeText(value.schema)}.${nativeText(value.name)}`;
}

function nativeText(value: string): string {
  let text = "";
  let characters = 0;
  for (const character of value) {
    if (characters === MAX_NATIVE_TEXT) return `${text}…`;
    text += character;
    characters++;
  }
  return text;
}
