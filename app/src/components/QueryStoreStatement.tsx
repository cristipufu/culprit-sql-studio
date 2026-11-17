import type { QueryStoreStatementExplanation } from "../gen/QueryStoreStatementExplanation";
import { t } from "../lib/i18n";

/** Rust owns binding and source meaning. This folded view never resolves an identifier or runs SQL. */
export function QueryStoreStatement({ statement }: { statement: QueryStoreStatementExplanation }) {
  return <details>
    <summary>{t("ui.query_store_statement.inspect")}</summary>
    <p>{statement.explanation}</p>
    {statement.preview !== null && <pre><code>{statement.preview}</code></pre>}
    {statement.preview_truncated && <p className="muted">{t("view.query_store_statement.shortened")}</p>}
    {statement.unavailable_explanation !== null && <p className="muted">{statement.unavailable_explanation}</p>}
    {statement.definition && <p className="muted">{t("ui.query_store_statement.reference", { reference: statement.definition.record_id })}</p>}
  </details>;
}
