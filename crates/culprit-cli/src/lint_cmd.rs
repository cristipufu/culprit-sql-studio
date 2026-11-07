use std::path::PathBuf;

use clap::Args;
use culprit_core::{Error, Severity};
use culprit_engine::Workspace;

use crate::output::Out;
use crate::{SeverityArg, fail_code, load, report_file};

#[derive(Args)]
pub(crate) struct LintSqlArgs {
    /// A connection name or a saved report/snapshot .json file.
    pub(crate) connection: String,
    /// Inline SQL or migration script to check.
    #[arg(long, conflicts_with = "files")]
    pub(crate) sql: Option<String>,
    /// Path to one or more .sql migration files or directories of .sql files to check.
    #[arg(long = "file", value_name = "PATH")]
    pub(crate) files: Vec<PathBuf>,
    /// Rewrite fixable DDL statements in place on the specified --file path(s).
    #[arg(long)]
    pub(crate) write: bool,
    /// Saved report or snapshot file to lint against (instead of the connection's latest scan).
    #[arg(long)]
    pub(crate) report: Option<PathBuf>,
    /// Exit with code 3 if any finding is at least this severe (for CI).
    #[arg(long, value_enum)]
    pub(crate) fail_on: Option<SeverityArg>,
}

pub(crate) fn run(ws: &Workspace, args: LintSqlArgs, out: &Out) -> culprit_core::Result<u8> {
    let LintSqlArgs { connection, sql, files, write, report, fail_on } = args;
    if write && files.is_empty() {
        return Err(Error::invalid("--write requires --file <PATH>"));
    }
    let report_path = report.or_else(|| report_file(&connection));
    let mut base_report = load(ws, &connection, report_path.as_ref())?;
    let mut batches: Vec<(Option<String>, String, Vec<culprit_core::Finding>)> = Vec::new();
    let mut all_findings = Vec::new();

    if let Some(s) = sql {
        let findings = culprit_engine::lint_sql(&base_report.snapshot, &s);
        all_findings.extend(findings.clone());
        batches.push((None, s, findings));
    } else if !files.is_empty() {
        let resolved = collect_sql_files(&files)?;
        let multi = resolved.len() > 1;
        for path in resolved {
            let uri = path.to_string_lossy().into_owned();
            let raw = std::fs::read_to_string(&path).map_err(|e| Error::invalid(format!("{}: {e}", path.display())))?;
            let sql_text = if write {
                let (rewritten, applied) = culprit_engine::apply_sql_fixes(&base_report.snapshot, &raw);
                if applied > 0 && rewritten != raw {
                    std::fs::write(&path, &rewritten)
                        .map_err(|e| Error::invalid(format!("{}: {e}", path.display())))?;
                }
                rewritten
            } else {
                raw
            };
            let findings = culprit_engine::lint_sql(&base_report.snapshot, &sql_text);
            for mut f in findings.clone() {
                if multi {
                    f.title = format!("[{uri}] {}", f.title);
                }
                all_findings.push(f);
            }
            batches.push((Some(uri), sql_text, findings));
        }
    } else {
        return Err(Error::invalid("Pass --sql <SQL> or --file <PATH>"));
    }

    let opportunities = all_findings.iter().filter(|f| f.is_actionable()).count() as u32;
    let mut by_category = Vec::new();
    for cat in [
        culprit_core::Category::Indexes,
        culprit_core::Category::Queries,
        culprit_core::Category::Maintenance,
        culprit_core::Category::Configuration,
    ] {
        let count = all_findings.iter().filter(|f| f.is_actionable() && f.category == cat).count() as u32;
        if count > 0 {
            by_category.push(culprit_core::CategoryCount { category: cat, count });
        }
    }
    let mut by_severity = Vec::new();
    for sev in [Severity::High, Severity::Medium, Severity::Low] {
        let count = all_findings.iter().filter(|f| f.is_actionable() && f.severity == sev).count() as u32;
        if count > 0 {
            by_severity.push(culprit_core::SeverityCount { severity: sev, count });
        }
    }
    let headline = if all_findings.is_empty() {
        "No migration or DDL issues found.".to_string()
    } else {
        format!(
            "{} migration DDL {} found.",
            all_findings.len(),
            if all_findings.len() == 1 { "issue" } else { "issues" }
        )
    };
    base_report.summary = culprit_core::Summary {
        opportunities,
        by_category,
        by_severity,
        top_share: None,
        headline,
        no_workload: None,
        runs_said: Vec::new(),
        over_scans: Vec::new(),
        index_notes: Vec::new(),
        index_counting: String::new(),
        foreign_key_indexes: Vec::new(),
    };
    base_report.findings = all_findings;
    base_report.locked.clear();
    base_report.assessments.clear();
    out.lint_sql(&base_report, &batches);
    Ok(fail_code(&base_report, fail_on))
}

fn collect_sql_files(inputs: &[PathBuf]) -> culprit_core::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for path in inputs {
        if path.is_file() {
            push_unique_file(path, &mut seen, &mut out);
        } else if path.is_dir() {
            collect_sql_dir(path, 0, &mut seen, &mut out)?;
        } else {
            return Err(Error::invalid(format!("{}: no such file or directory", path.display())));
        }
    }
    if out.is_empty() {
        return Err(Error::invalid("No .sql files found in the specified --file path(s)"));
    }
    Ok(out)
}

fn push_unique_file(path: &std::path::Path, seen: &mut std::collections::HashSet<PathBuf>, out: &mut Vec<PathBuf>) {
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if seen.insert(key) {
        out.push(path.to_path_buf());
    }
}

fn collect_sql_dir(
    dir: &std::path::Path,
    depth: usize,
    seen: &mut std::collections::HashSet<PathBuf>,
    out: &mut Vec<PathBuf>,
) -> culprit_core::Result<()> {
    if depth > 16 {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).map_err(|e| Error::invalid(format!("{}: {e}", dir.display())))?;
    let mut items: Vec<(PathBuf, std::fs::FileType)> = entries
        .filter_map(|e| {
            let e = e.ok()?;
            let ft = e.file_type().ok()?;
            Some((e.path(), ft))
        })
        .collect();
    items.sort_by(|a, b| a.0.cmp(&b.0));
    for (p, ft) in items {
        if ft.is_symlink() {
            continue;
        }
        if ft.is_dir() {
            let is_hidden = p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
            if !is_hidden {
                collect_sql_dir(&p, depth + 1, seen, out)?;
            }
        } else if ft.is_file() && p.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("sql")) {
            push_unique_file(&p, seen, out);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_sql_files_traverses_directories_skips_hidden_and_deduplicates() {
        let dir = std::env::temp_dir().join(format!("culprit-lint-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        let a = dir.join("001_init.sql");
        let b = dir.join("sub").join("002_index.SQL");
        let hidden = dir.join(".hidden").join("003_secret.sql");
        let txt = dir.join("README.txt");
        for f in [&a, &b, &hidden, &txt] {
            std::fs::write(f, "SELECT 1;").unwrap();
        }
        let dot_a = dir.join(".").join("001_init.sql");
        let found = collect_sql_files(&[dir.clone(), a.clone(), dot_a]).unwrap();
        assert_eq!(found, vec![a, b]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
