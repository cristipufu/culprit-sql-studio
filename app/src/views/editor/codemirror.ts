// What the SQL editor adds to CodeMirror: lint marks, colors from the app's code tokens, and the dialect's language
// with completion from the connection's catalog.
import type { CompletionSource } from "@codemirror/autocomplete";
import { MariaSQL, MSSQL, MySQL, PostgreSQL, SQLite, keywordCompletionSource, schemaCompletionSource } from "@codemirror/lang-sql";
import { HighlightStyle, LanguageSupport, syntaxHighlighting } from "@codemirror/language";
import { StateEffect, StateField, type Extension } from "@codemirror/state";
import { Decoration, EditorView, type DecorationSet } from "@codemirror/view";
import { tags } from "@lezer/highlight";
import type { Catalog } from "../../gen/Catalog";
import type { Engine } from "../../gen/Engine";
import type { Finding } from "../../gen/Finding";
import type { Report } from "../../gen/Report";
import { completionConfig } from "../../lib/completion";
import { locateFindingSpan } from "../../lib/editor-sql";
import { isMariadb, sqlSyntax } from "../../lib/engines";

/** Replace the lint marks with these findings' spans. */
export const setLintEffect = StateEffect.define<Finding[]>();

/** The live linter's marks: each finding underlines the statement it is about. */
export const lintField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(decorations, tr) {
    decorations = decorations.map(tr.changes);
    for (const effect of tr.effects) {
      if (effect.is(setLintEffect)) {
        const docText = tr.state.doc.toString();
        const marks = effect.value
          .map((f) => {
            const span = locateFindingSpan(docText, f);
            if (!span) return null;
            const cls = f.severity === "high" ? "cm-lint-error" : "cm-lint-warn";
            return Decoration.mark({ class: cls, attributes: { title: f.title } }).range(span[0], span[1]);
          })
          .filter((r): r is NonNullable<typeof r> => r !== null)
          .sort((a, b) => a.from - b.from);
        decorations = Decoration.set(marks, true);
      }
    }
    return decorations;
  },
  provide: (field) => EditorView.decorations.from(field),
});

/** SQL colors from the same tokens as every other code block, so they follow light and dark mode. */
export const sqlColors = syntaxHighlighting(
  HighlightStyle.define([
    { tag: [tags.keyword, tags.operatorKeyword, tags.modifier], color: "var(--code-keyword)" },
    { tag: [tags.string, tags.special(tags.string)], color: "var(--code-string)" },
    { tag: [tags.number, tags.bool, tags.null], color: "var(--code-number)" },
    { tag: [tags.comment, tags.lineComment, tags.blockComment], color: "var(--code-comment)", fontStyle: "italic" },
    { tag: [tags.special(tags.name), tags.variableName], color: "var(--code-param)" },
    { tag: [tags.typeName, tags.standard(tags.name)], color: "var(--code-keyword)" },
  ]),
);

/**
 * The engine's SQL dialect with completion: tables and columns from the catalog (or the report), keywords except
 * after `alias.`, where only that table's columns make sense.
 */
export function sqlLanguage(engine: Engine, product: string | null | undefined, catalog: Catalog | null, report: Report | null): Extension {
  const dialect = { mssql: MSSQL, postgres: PostgreSQL, mysql: isMariadb(engine, product) ? MariaSQL : MySQL, sqlite: SQLite }[sqlSyntax(engine)];
  const keywords = keywordCompletionSource(dialect, true);
  const keywordsOutsideMembers: CompletionSource = (context) => (/[\w"`\]]\.\w*$/.test(context.state.sliceDoc(Math.max(0, context.pos - 120), context.pos)) ? null : keywords(context));
  return new LanguageSupport(dialect.language, [
    dialect.language.data.of({ autocomplete: schemaCompletionSource({ dialect, ...completionConfig(engine, catalog, report) }) }),
    dialect.language.data.of({ autocomplete: keywordsOutsideMembers }),
  ]);
}
