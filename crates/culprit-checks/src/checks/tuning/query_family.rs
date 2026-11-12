//! The `query-family` check: The same statement sent many times with different literal values (or value-list lengths), so the database compiles and tracks each copy separately; and plan caches filled with single-use plans.

use super::*;
use crate::evidence::{QueryMeasure, Requirement as Evidence};
use culprit_core::t;

pub fn query_family_info() -> CheckInfo {
    CheckInfo {
        id: "query-family".into(),
        title: t!("check.query_family.info.title"),
        category: Category::Queries,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Yugabyte],
        requires: vec![],
        description: t!("check.query_family.info.description"),
    }
}

check!(
    QueryFamilyCheck: query_family_info,
    decide = query_family_cases,
    render = query_family_finding,
    case<'s> = FamilyCase<'s>,
    evidence = &[Evidence::Query(QueryMeasure::Work)],
);

/// What the workload's statement families and the plan cache establish.
pub enum FamilyCase<'s> {
    /// One statement sent as many copies, each compiled and tracked on its own.
    Copies(Copies<'s>),
    /// Plans compiled for statements that ran once fill much of the plan cache (SQL Server).
    SingleUsePlans(SingleUsePlans<'s>),
}

/// A statement family, and what its copies differ in.
pub struct Copies<'s> {
    family: &'s culprit_core::QueryFamily,
    differ: Differ,
    /// What the copies differ in is inferred from the one text Culprit has.
    inferred: bool,
    /// Its share of database time; `None` where the workload has no time.
    share: Option<f64>,
    severity: Severity,
}

/// What a family's copies differ in, which decides its fix.
enum Differ {
    /// Their parameters' declarations (one or two examples): the driver sizes them from each value.
    Declarations(Vec<String>),
    /// Literal values (SQL Server): what parameterizing fixes.
    Literals,
    /// The lengths of their value lists (PostgreSQL and the like).
    ValueLists,
}

/// Single-use plans filling the plan cache, and where the setting that keeps a stub instead is.
pub struct SingleUsePlans<'s> {
    cache: &'s culprit_core::PlanCacheUse,
    share: f64,
    /// 'Optimize for ad hoc workloads' is already on.
    ad_hoc_on: bool,
    severity: Severity,
}

fn query_family_cases(snapshot: &Snapshot) -> Vec<FamilyCase<'_>> {
    let engine = snapshot.server.engine;
    let mut out = Vec::new();
    let threshold = if engine == Engine::Mssql { 20.0 } else { 5.0 };
    let total_ms = snapshot.workload.as_ref().map(|w| w.total_time_ms).unwrap_or(0.0);
    for family in snapshot.query_families.iter().filter(|f| f.variants >= threshold) {
        // What the copies differ in decides the fix; copies whose text is the same are split by something no
        // parameterization changes (the role that ran them, the schema a name resolved in, the module they're in).
        let Some(split) = family_split(snapshot, family) else { continue };
        let inferred =
            matches!(&split, FamilySplit::Unknown) || matches!(&split, FamilySplit::Declarations(e) if e.len() < 2);
        let differ = match (split, engine) {
            (
                FamilySplit::Declarations(examples),
                Engine::Mssql | Engine::Postgres | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite,
            ) => Differ::Declarations(examples),
            (FamilySplit::Values | FamilySplit::Unknown, Engine::Mssql) => Differ::Literals,
            (
                FamilySplit::Values | FamilySplit::Unknown,
                Engine::Postgres | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite,
            ) => Differ::ValueLists,
        };
        let share = if total_ms > 0.0 { family.total_time_ms / total_ms } else { 0.0 };
        // Thousands of copies cost compiles and plan cache whatever their time; a share needs an amount behind it.
        let timed = share >= 0.1 && severity_for_time(snapshot, family.total_time_ms, share) >= Severity::Medium;
        out.push(FamilyCase::Copies(Copies {
            family,
            differ,
            inferred,
            share: (total_ms > 0.0).then_some(share),
            severity: if family.variants >= 1000.0 || timed { Severity::Medium } else { Severity::Low },
        }));
    }

    if let Some(cache) = &snapshot.plan_cache
        && cache.total_bytes > 0.0
    {
        let share = cache.single_use_bytes / cache.total_bytes;
        if cache.single_use_bytes >= 256.0 * 1024.0 * 1024.0 && share >= 0.25 {
            out.push(FamilyCase::SingleUsePlans(SingleUsePlans {
                cache,
                share,
                ad_hoc_on: snapshot.setting("optimize for ad hoc workloads").is_some_and(|s| s.value.trim() == "1"),
                severity: if share >= 0.5 { Severity::Medium } else { Severity::Low },
            }));
        }
    }
    out
}

fn query_family_finding(snapshot: &Snapshot, case: FamilyCase<'_>) -> Finding {
    match case {
        FamilyCase::Copies(copies) => copies_finding(snapshot, copies),
        FamilyCase::SingleUsePlans(plans) => single_use_finding(snapshot, plans),
    }
}

fn copies_finding(snapshot: &Snapshot, copies: Copies<'_>) -> Finding {
    let Copies { family, differ, inferred, share, severity } = copies;
    let engine = snapshot.server.engine;
    let mut f = finding(
        "query-family",
        &family.key.chars().take(40).collect::<String>(),
        Category::Queries,
        subject(SubjectKind::Query, family.key.clone()),
    );
    f.facts.workload_dependency = Some(culprit_core::WorkloadDependency::Independent);
    let handle = statement_handle(without_declaration(&family.sample));
    let variants = format::count(family.variants);
    f.title = match &differ {
        Differ::Declarations(_) => {
            t!("check.query_family.title.declarations", count = &variants, statement = &handle)
        }
        Differ::Literals => t!("check.query_family.title.literals", count = &variants, statement = &handle),
        Differ::ValueLists => {
            t!("check.query_family.title.value_lists", statement = capitalize(&handle), count = &variants)
        }
    };
    let executions = format::count(family.executions);
    let timed = if share.is_some() { "yes" } else { "no" };
    let share_said = share.map(|s| format::percent(s.clamp(0.0, 1.0))).unwrap_or_default();
    let cache = family.cached_bytes.filter(|b| *b >= 1024.0 * 1024.0);
    let cached = if cache.is_some() { "yes" } else { "no" };
    let cache = cache.map(format::bytes).unwrap_or_default();
    let ran = match &differ {
        Differ::Declarations(examples) => t!(
            "check.query_family.summary.declarations",
            variants = &variants,
            executions = &executions,
            timed = timed,
            share = &share_said,
            declarations = match examples.as_slice() {
                [one] => t!("check.query_family.summary.one_declaration", declaration = one),
                _ => format::list(&examples.iter().map(|e| format!("`{e}`")).collect::<Vec<_>>()),
            },
            cached = cached,
            cache = &cache
        ),
        Differ::Literals => t!(
            "check.query_family.summary.literals",
            variants = &variants,
            executions = &executions,
            timed = timed,
            share = &share_said,
            cached = cached,
            cache = &cache
        ),
        Differ::ValueLists => t!(
            "check.query_family.summary.value_lists",
            variants = &variants,
            executions = &executions,
            timed = timed,
            share = &share_said,
            cached = cached,
            cache = &cache
        ),
    };
    f.summary = [ran, t!("check.query_family.summary.statistics_split")].join(" ");
    let share = share.unwrap_or(0.0);
    f.severity = severity;
    f.impact.score = base_score(f.severity) + family.variants.log10().max(0.0) * 3.0 + share * 20.0;
    f.impact.set_label(t!("check.query_family.impact", count = &variants));
    f.impact.db_time_share = (share > 0.0).then_some(share);
    f.confidence = Confidence::High;
    f.metrics.push(Metric::said(
        culprit_core::key!("check.query_family.metric.variants"),
        family.variants,
        Unit::Count,
    ));
    f.metrics.push(Metric::said(culprit_core::key!("common.metric.executions"), family.executions, Unit::Count));
    if let Some(b) = family.cached_bytes {
        f.metrics.push(Metric::said(culprit_core::key!("check.query_family.metric.plan_cache"), b, Unit::Bytes));
    }
    f.queries = family
        .query_ids
        .iter()
        .filter(|id| snapshot.workload.as_ref().is_some_and(|w| w.query(id).is_some()))
        .map(|id| QueryRef { id: id.clone(), plan_node: None, note: None })
        .collect();
    // The change is in the code that sends the statement: the caveats quote one copy of it, as recorded.
    let quoted = !family.sample.is_empty();
    f.facts.advice.statement_quote = quoted.then_some(culprit_core::StatementQuote::Caveat);
    if matches!(differ, Differ::Literals) {
        f.facts.advice.application = culprit_core::ApplicationAdvice::Preferred;
    }
    f.fix = Some(match differ {
        Differ::Declarations(_) => Fix {
            summary: if quoted {
                t!("check.query_family.fix.declarations_quoted")
            } else {
                t!("check.query_family.fix.declarations")
            },
            sql: None,
            cost: None,
        },
        Differ::Literals => Fix {
            summary: if quoted {
                t!("check.query_family.fix.literals_quoted")
            } else {
                t!("check.query_family.fix.literals")
            },
            sql: Some(format!(
                "-- Prefer fixing the app. As a database-wide fallback:\nALTER DATABASE {} SET PARAMETERIZATION FORCED;",
                quote(engine, &snapshot.server.database)
            )),
            cost: Some(t!("check.query_family.fix_cost.literals")),
        },
        Differ::ValueLists => Fix {
            summary: if quoted {
                t!("check.query_family.fix.value_lists_quoted")
            } else {
                t!("check.query_family.fix.value_lists")
            },
            sql: None,
            cost: None,
        },
    });
    if inferred {
        f.confidence = Confidence::Medium;
        f.caveats.push(t!("check.query_family.caveat.inferred"));
    }
    // pg_stat_statements keeps an entry for each copy until it is evicted or reset: a deploy stops new ones, and
    // the old ones stay counted.
    f.verify = vec![if engine.sql_syntax() == culprit_core::SqlSyntax::Postgres {
        t!("check.query_family.verify.entries_kept", count = &variants)
    } else {
        t!("check.query_family.verify.shrinks")
    }];
    if quoted {
        f.caveats.push(t!("check.query_family.caveat.sample", sample = family.sample.trim()));
    }
    f
}

fn single_use_finding(snapshot: &Snapshot, plans: SingleUsePlans<'_>) -> Finding {
    let SingleUsePlans { cache, share, ad_hoc_on, severity } = plans;
    let mut f =
        finding("query-family", "plan-cache", Category::Configuration, subject(SubjectKind::Setting, "plan cache"));
    f.facts.workload_dependency = Some(culprit_core::WorkloadDependency::Independent);
    f.facts.cause = Some(culprit_core::FindingCause::PlanCache);
    let bytes = format::bytes(cache.single_use_bytes);
    f.title = t!("check.query_family.plan_cache.title", share = format::percent(share), size = &bytes);
    let mut summary = vec![t!("check.query_family.plan_cache.summary", plans = format::count(cache.single_use_plans))];
    if !ad_hoc_on {
        summary.push(t!("check.query_family.plan_cache.summary_ad_hoc_off"));
    }
    f.summary = summary.join(" ");
    f.severity = severity;
    f.impact.score = base_score(f.severity) + share * 10.0;
    f.impact.set_label(t!("check.query_family.plan_cache.impact", size = &bytes));
    f.confidence = Confidence::High;
    f.metrics.push(Metric::said(
        culprit_core::key!("check.query_family.plan_cache.metric.single_use_plans"),
        cache.single_use_plans,
        Unit::Count,
    ));
    f.metrics.push(Metric::said(
        culprit_core::key!("check.query_family.plan_cache.metric.their_memory"),
        cache.single_use_bytes,
        Unit::Bytes,
    ));
    f.metrics.push(Metric::said(culprit_core::key!("check.query_family.plan_cache.metric.share"), share, Unit::Ratio));
    // Where the setting lives: per database on Azure SQL Database, a server option elsewhere (the DB parameter
    // group on RDS, which refuses sp_configure).
    f.fix = Some(if ad_hoc_on {
        Fix { summary: t!("check.query_family.plan_cache.fix.parameterize"), sql: None, cost: None }
    } else if snapshot.server.on(culprit_core::Platform::AzureSqlDatabase) {
        Fix {
            summary: t!("check.query_family.plan_cache.fix.stub"),
            sql: Some("ALTER DATABASE SCOPED CONFIGURATION SET OPTIMIZE_FOR_AD_HOC_WORKLOADS = ON;".to_string()),
            cost: None,
        }
    } else {
        let change = crate::config::server_option(&snapshot.server, "optimize for ad hoc workloads", "1");
        let mut summary = vec![t!("check.query_family.plan_cache.fix.stub")];
        if change.sql.is_none() {
            summary.push(change.how);
        }
        Fix { summary: summary.join(" "), sql: change.sql, cost: None }
    });
    f.verify = vec![t!("check.query_family.plan_cache.verify")];
    f
}

/// What the copies of a statement family differ in, as far as their texts show.
#[derive(Debug, PartialEq)]
pub(super) enum FamilySplit {
    /// Literal values (SQL Server) or value-list lengths (PostgreSQL): what parameterizing fixes.
    Values,
    /// The same parameterized statement with its parameters declared differently (sizes taken from each value).
    Declarations(Vec<String>),
    /// Inferred from the one text Culprit has.
    Unknown,
}

/// None when the copies are the same text: the split is by who ran them (PostgreSQL keeps an entry per role), the
/// schema a name resolved in, the module they belong to or the connection's settings, which no change to how values
/// are sent fixes.
pub(super) fn family_split(snapshot: &Snapshot, family: &culprit_core::QueryFamily) -> Option<FamilySplit> {
    let members: Vec<&QueryStat> = snapshot
        .workload
        .as_ref()
        .map(|w| family.query_ids.iter().filter_map(|id| w.query(id)).collect())
        .unwrap_or_default();
    if members.len() >= 2 {
        let first = normalized(&members[0].text);
        if members.iter().any(|q| normalized(&q.text) != first) {
            return Some(FamilySplit::Values);
        }
        let mut declarations: Vec<String> = Vec::new();
        for d in members.iter().filter_map(|q| q.parameters.as_deref()) {
            if !declarations.iter().any(|x| x == d) {
                declarations.push(d.to_string());
            }
        }
        return (declarations.len() >= 2)
            .then(|| FamilySplit::Declarations(declarations.into_iter().take(2).collect()));
    }
    match snapshot.server.engine {
        Engine::Mssql => match split_declaration(&family.sample) {
            // Parameters the application named (`@p0`, `@P1`, `@id`) and no literal left in the text: the copies can't
            // differ in values, so the declarations are what differs.
            (Some(declaration), body) if !auto_parameterized(declaration) && !has_literals(body) => {
                Some(FamilySplit::Declarations(vec![declaration.trim().to_string()]))
            }
            _ => Some(FamilySplit::Unknown),
        },
        // Entries are grouped with their value lists collapsed: without a list, the copies are the same text.
        Engine::Postgres | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite => {
            has_value_list(&family.sample).then_some(FamilySplit::Values)
        }
    }
}

/// `(@p0 nvarchar(7),@p1 int)SELECT …` as (declaration, statement), the way Query Store keeps a parameterized text.
pub(super) fn split_declaration(text: &str) -> (Option<&str>, &str) {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('(') || !trimmed[1..].trim_start().starts_with('@') {
        return (None, text);
    }
    let mut depth = 0usize;
    for (i, c) in trimmed.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return (Some(&trimmed[1..i]), trimmed[i + 1..].trim_start());
                }
            }
            _ => {}
        }
    }
    (None, text)
}

pub(super) fn without_declaration(text: &str) -> &str {
    split_declaration(text).1
}

/// SQL Server's own parameterization names parameters by number (`@1`, `@2`): the application sent literals.
pub(super) fn auto_parameterized(declaration: &str) -> bool {
    declaration
        .split(',')
        .any(|d| d.trim().strip_prefix('@').is_some_and(|n| n.starts_with(|c: char| c.is_ascii_digit())))
}

/// A string or number literal in a statement (outside names, parameters and `TOP (n)`).
pub(super) fn has_literals(body: &str) -> bool {
    let lower = body.to_ascii_lowercase();
    let chars: Vec<char> = lower.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '\'' {
            return true;
        }
        if c == '[' {
            while i < chars.len() && chars[i] != ']' {
                i += 1;
            }
        } else if c.is_ascii_digit() {
            let prev = i.checked_sub(1).map(|p| chars[p]);
            let in_name = prev.is_some_and(|p| p.is_ascii_alphanumeric() || matches!(p, '_' | '@' | '#' | '$'));
            let before: String = chars[..i].iter().collect();
            let top = before.trim_end().trim_end_matches('(').trim_end().ends_with("top");
            if !in_name && !top {
                return true;
            }
            while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.') {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    false
}

/// An IN list, multi-row VALUES or ARRAY of parameters: what PostgreSQL entries of different lengths come from.
pub(super) fn has_value_list(text: &str) -> bool {
    let t = text.to_ascii_lowercase().split_whitespace().collect::<Vec<_>>().join(" ").replace("( ", "(");
    [" in ($", " in($", "values ($", "values($", "array[$", "any(array[$"].iter().any(|m| t.contains(m))
}

/// Statement text without comments and with whitespace collapsed, to compare two copies.
pub(super) fn normalized(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("/*") {
            rest = after.split_once("*/").map_or("", |(_, r)| r);
        } else if let Some(after) = rest.strip_prefix("--") {
            rest = after.split_once('\n').map_or("", |(_, r)| r);
        } else {
            let c = rest.chars().next().expect("non-empty");
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
