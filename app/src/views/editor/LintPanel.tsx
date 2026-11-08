import { TriangleAlert, WandSparkles } from "lucide-react";
import type { Finding } from "../../gen/Finding";
import { t } from "../../lib/i18n";

/** The live DDL linter's findings above the editor, each with its quick fix, and one button for all of them. */
export function LintPanel({ findings, onFix, onFixAll }: { findings: Finding[]; onFix: (f: Finding) => void; onFixAll: () => void }) {
  if (findings.length === 0) return null;
  const fixable = findings.filter((f) => Boolean(f.fix?.sql)).length;
  return (
    <div className="editor-lint" role="status" aria-live="polite">
      {fixable >= 2 && (
        <div className="editor-lint-row">
          <WandSparkles size={13} className="editor-lint-icon" />
          <div className="editor-lint-copy">
            <strong>{t("ui.editor.lint.fixable", { count: fixable })}</strong>
            <span>{t("ui.editor.lint.fixable_detail")}</span>
          </div>
          <button type="button" className="btn ghost sm" onClick={onFixAll} title={t("ui.editor.lint.fix_all_title")}>
            <WandSparkles size={12} /> {t("ui.editor.lint.fix_all")}
          </button>
        </div>
      )}
      {findings.map((f, idx) => (
        <div key={`${f.id}:${idx}`} className="editor-lint-row">
          <TriangleAlert size={13} className="editor-lint-icon" />
          <div className="editor-lint-copy">
            <strong>{f.title}</strong>
            <span>{f.fix?.summary ?? f.summary}</span>
          </div>
          {f.fix?.sql && (
            <button type="button" className="btn ghost sm" onClick={() => onFix(f)} title={t("ui.editor.lint.fix_title")}>
              <WandSparkles size={12} /> {t("ui.editor.lint.fix")}
            </button>
          )}
        </div>
      ))}
    </div>
  );
}
