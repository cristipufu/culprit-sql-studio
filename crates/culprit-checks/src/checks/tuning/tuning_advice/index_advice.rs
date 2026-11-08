//! Azure SQL automatic tuning index recommendations (`CreateIndex` and `DropIndex`), weighed against the optimizer's
//! missing-index requests, existing indexes, and measured statement runs.

use super::*;

/// Azure SQL's waiting recommendation to create an index on these key columns of a table. The missing-index finding for
/// the same index says so rather than a second finding.
pub(crate) fn recommended_create<'a>(
    snapshot: &'a Snapshot,
    schema: &str,
    table: &str,
    keys: &[String],
) -> Option<&'a TuningRecommendation> {
    snapshot.tuning.as_ref()?.recommendations.iter().find(|r| {
        waiting(r)
            && r.kind.eq_ignore_ascii_case("CreateIndex")
            && r.index.as_ref().is_some_and(|i| {
                i.schema.eq_ignore_ascii_case(schema)
                    && i.table.eq_ignore_ascii_case(table)
                    && !i.columns.is_empty()
                    && lower_set(&i.columns) == lower_set(keys)
            })
    })
}

/// Azure's score below which it expects an index to save little next to what it costs every write.
const LOW_SCORE: f64 = 10.0;

/// An average run under this is fast: no slow read for an index to speed up (the bar from which a plan does real work,
/// `culprit_core::insights`' ten milliseconds).
const SLOW_RUN_MS: f64 = 10.0;

/// Statements listed under a recommendation, the heaviest first; the rest are counted.
const LISTED: usize = 5;

/// Azure SQL's recommendation to create an index, weighed against the indexes there and the statements behind it.
pub struct CreateIndex<'s> {
    r: &'s TuningRecommendation,
    ix: &'s RecommendedIndex,
    /// The statements the scan captured that compare every key with a value, the heaviest first.
    filtering: Vec<&'s QueryStat>,
    rows: Option<f64>,
    /// The columns to include: the recommendation's, without those that can hold large values.
    include: Vec<String>,
    /// The included columns left out for their large values, and the indexes area's sentence saying so.
    large: Vec<String>,
    large_note: Option<String>,
    /// A heavy statement on exactly these keys, reading for nothing: evidence of its own that the index would pay.
    heavy: Option<HeavyReader<'s>>,
    /// A heavy statement whose plan reads the table through another index, picked on an estimate.
    picked: Option<PickedElsewhere<'s>>,
    /// Azure's score, where it says the index is hardly worth having.
    low: Option<f64>,
    advice: Advice<'s>,
    severity: Severity,
}

/// What the recommendation comes to, given the indexes there and the statements the scan measured.
enum Advice<'s> {
    /// Every statement behind it averages under [`SLOW_RUN_MS`] a run (the fastest and slowest averages), and whether
    /// its keys are all bit columns: nothing for it to speed up.
    Fast { fastest: f64, slowest: f64, bits: bool },
    /// An index already finds the rows by the keys; the recommendation would only copy large columns into a second one.
    AlreadyThere(&'s IndexStat),
    /// An index leads with the keys but lacks these included columns: widen it.
    Widen { existing: &'s IndexStat, lacking: Vec<String> },
    /// Azure scores it low, and the heavy statement reads past an index there is: the plan's choice, not this index.
    PickedElsewhere,
    /// Create it; an index there may lead with some of its keys.
    Create(Option<Leading<'s>>),
}

/// An index on the table that leads with some of the recommended keys.
struct Leading<'s> {
    index: &'s IndexStat,
    keys: Vec<String>,
    /// The heavy statement's plan reads the table through other indexes: (its query id, those indexes).
    instead: Option<(&'s str, Vec<String>)>,
}

pub(super) fn create_index_advice<'s>(
    snapshot: &'s Snapshot,
    r: &'s TuningRecommendation,
    ix: &'s RecommendedIndex,
) -> Option<CreateIndex<'s>> {
    if ix.columns.is_empty() {
        return None;
    }
    // The optimizer asks for the same index: the missing-index finding says Azure recommends it too.
    if merge_candidates(snapshot).iter().any(|c| {
        c.schema.eq_ignore_ascii_case(&ix.schema)
            && c.table.eq_ignore_ascii_case(&ix.table)
            && lower_set(&c.keys()) == lower_set(&ix.columns)
    }) {
        return None;
    }
    // The statements behind it, where the scan captured them: those that compare every key with a value. One heavy
    // enough, and measured reading pages for nothing, is evidence of its own that the index would pay.
    let filtering = statements_filtering_on(snapshot, ix);
    let rows = snapshot.table(&ix.schema, &ix.table).and_then(|t| t.rows);
    // Large values stay out of INCLUDE, as in the optimizer's requests: copying them into an index costs more than the
    // lookups back to the table it spares.
    let mut include = ix.include.clone();
    let large_note = crate::indexes::drop_large_includes(snapshot, &ix.schema, &ix.table, &mut include);
    let large: Vec<String> = ix.include.iter().filter(|c| !include.contains(c)).cloned().collect();
    // An index that leads with the keys may hold the included columns too: then the recommendation predates it (a
    // migration added it since), and it expires on its own; the clustered index holds every column. One that lacks
    // them can be widened instead.
    let same_keys = leading_index(snapshot, &ix.schema, &ix.table, &ix.columns, &ix.columns);
    let lacking: Vec<String> = match same_keys {
        Some(existing) => {
            let held: Vec<&str> =
                existing.key_names().into_iter().chain(existing.include.iter().map(String::as_str)).collect();
            include.iter().filter(|c| !held.iter().any(|h| h.eq_ignore_ascii_case(c))).cloned().collect()
        }
        None => Vec::new(),
    };
    if same_keys.is_some_and(|e| e.is_clustered) || (same_keys.is_some() && lacking.is_empty() && large.is_empty()) {
        return None;
    }
    // Where an index with these keys exists, a statement reading for nothing isn't missing this one.
    let heavy = heavy_reader(snapshot, &filtering).filter(|_| same_keys.is_none());
    // Nor is one whose plan reads the table through another index while one that leads with keys it compares with a
    // value is there: what it reads comes from the plan's choice (the heavy-query finding's to fix), so it is no
    // evidence for creating this one.
    let picked = heavy.as_ref().and_then(|h| picked_elsewhere(snapshot, ix, h));
    let heavy = heavy.filter(|_| picked.is_none());
    // Azure's score weighs what it expects the index to save against its upkeep: a low one is its own word that the
    // index is hardly worth having, unless a heavy statement the scan measured on exactly these keys says otherwise.
    let low = r.score.filter(|s| *s < LOW_SCORE);
    // Low on Azure's word alone: the advice is conditional, and so is checking it.
    let conditional = low.is_some() && heavy.is_none();
    // The condition a low score leaves ("if the queries are slow all the same") is one the scan can answer: where every
    // statement behind the recommendation runs fast on average, nothing is slow for the index to speed up.
    let fast = (conditional && same_keys.is_none() && picked.is_none() && !filtering.is_empty())
        .then(|| {
            filtering.iter().map(|q| q.mean_time_ms).fold((f64::MAX, 0.0f64), |(lo, hi), t| (lo.min(t), hi.max(t)))
        })
        .filter(|(_, slowest)| *slowest < SLOW_RUN_MS);
    let advice = match (same_keys, picked.as_ref().filter(|_| low.is_some())) {
        (None, None) if let Some((fastest, slowest)) = fast => {
            // A key of two values (a bit, a boolean) splits the table in two at most: it narrows little.
            let bits = ix.columns.iter().all(|c| {
                snapshot.columns.iter().any(|col| {
                    col.schema.eq_ignore_ascii_case(&ix.schema)
                        && col.table.eq_ignore_ascii_case(&ix.table)
                        && col.name.eq_ignore_ascii_case(c)
                        && matches!(col.data_type.to_ascii_lowercase().as_str(), "bit" | "boolean" | "bool")
                })
            });
            Advice::Fast { fastest, slowest, bits }
        }
        (Some(existing), _) if lacking.is_empty() => Advice::AlreadyThere(existing),
        (Some(existing), _) => Advice::Widen { existing, lacking },
        // Azure's low score, and the one statement that would say otherwise reads past an index there is: nothing to
        // create, and the heavy-query finding has the fix.
        (None, Some(_)) => Advice::PickedElsewhere,
        (None, None) => Advice::Create(leading_with_some(snapshot, ix).map(|(index, keys)| {
            // The heavy statement's plan says which index it reads the table through: the engine's own choice.
            let instead = heavy.as_ref().and_then(|h| {
                let used = indexes_read_through(h.query, &ix.schema, &ix.table);
                (!used.is_empty() && !used.iter().any(|u| u.eq_ignore_ascii_case(&index.name)))
                    .then_some((h.query.id.as_str(), used))
            });
            Leading { index, keys, instead }
        })),
    };
    Some(CreateIndex {
        r,
        ix,
        filtering,
        rows,
        include,
        large,
        large_note,
        heavy,
        picked,
        low,
        advice,
        severity: if r.score.is_some_and(|s| s >= 50.0) { Severity::Medium } else { Severity::Low },
    })
}

/// "nothing" or "less than a row": what a statement's runs return, as a message's `returns`.
fn returns(rows: f64) -> &'static str {
    if rows <= 0.0 { "nothing" } else { "less_than_a_row" }
}

pub(super) fn create_index_finding(snapshot: &Snapshot, c: CreateIndex<'_>) -> Finding {
    let CreateIndex { r, ix, filtering, include, large, heavy, picked, low, advice, .. } = c;
    let table_name = format!("{}.{}", ix.schema, ix.table);
    let columns = format::list(&ix.columns);
    let mut f = finding(
        "tuning-advice",
        &format!("create-index:{table_name}({})", ix.columns.join(",")),
        Category::Indexes,
        subject(SubjectKind::Table, &table_name),
    );
    f.title = t!("check.tuning_advice.create.title", table = &ix.table, columns = &columns);
    let keys = if ix.columns.len() == 1 {
        columns.clone()
    } else {
        t!("check.tuning_advice.create.all_of", columns = &columns)
    };
    let behind = match filtering.len() {
        0 => t!("check.tuning_advice.create.summary.behind_none", keys = &keys),
        1 => t!("check.tuning_advice.create.summary.behind_one", statement = query_handle(filtering[0]), keys = &keys),
        n if n <= LISTED => {
            t!("check.tuning_advice.create.summary.behind_listed", count = n.to_string(), keys = &keys)
        }
        n => t!(
            "check.tuning_advice.create.summary.behind_heaviest",
            count = n.to_string(),
            keys = &keys,
            listed = LISTED.to_string()
        ),
    };
    f.summary = [
        t!(
            "check.tuning_advice.create.summary.recommends",
            table = &table_name,
            columns = &columns,
            included = if ix.include.is_empty() { "no" } else { "yes" },
            include = format::list(&ix.include),
            dated = if r.valid_since.is_some() { "yes" } else { "no" },
            since = r.valid_since.as_deref().map(|s| format::friendly_date(s, &snapshot.taken_at)).unwrap_or_default(),
            scored = if r.score.is_some() { "yes" } else { "no" },
            score = r.score.map(|s| format!("{s:.0}")).unwrap_or_default()
        ),
        t!("check.tuning_advice.create.summary.weighs", table = &ix.table),
        behind,
    ]
    .join(" ");
    f.queries = query_refs(&filtering.iter().take(LISTED).copied().collect::<Vec<_>>());
    f.severity = c.severity;
    f.impact.score = base_score(f.severity) + r.score.unwrap_or(0.0) / 10.0;
    f.impact.set_label(t!("check.tuning_advice.azure_recommended"));
    f.confidence = Confidence::Medium;
    if let Some(score) = r.score {
        f.metrics.push(Metric::said(culprit_core::key!("check.tuning_advice.metric.azure_score"), score, Unit::Count));
    }
    if let Some(rows) = c.rows {
        f.metrics.push(Metric::said(culprit_core::key!("check.tuning_advice.metric.rows_in_table"), rows, Unit::Rows));
    }
    // Azure's word on its score, said before what to do.
    let rated = match (low, &heavy) {
        (Some(s), Some(h)) => Some(t!(
            "check.tuning_advice.create.rated_but_heavy",
            score = format!("{s:.0}"),
            statement = query_handle(h.query),
            query = &h.query.id,
            share = format::percent(h.share),
            pages = format::count(h.pages),
            returns = returns(h.rows)
        )),
        (Some(s), None) => Some(t!("check.tuning_advice.create.rated_low", score = format!("{s:.0}"))),
        (None, _) => None,
    };
    let with_rated = |summary: String| rated.iter().cloned().chain([summary]).collect::<Vec<_>>().join(" ");
    let conditional = low.is_some() && heavy.is_none();
    let target = qualified(Engine::Mssql, &ix.schema, &ix.table);
    let name = ix.name.clone().unwrap_or_else(|| format!("ix_{}_{}", ix.table, ix.columns.join("_")));
    let create = |why: Option<String>| Fix {
        summary: why
            .into_iter()
            .chain(rated.iter().cloned())
            .chain([if conditional {
                t!("check.tuning_advice.create.fix.create_if_slow", table = &ix.table)
            } else if low.is_some() {
                t!("check.tuning_advice.create.fix.create")
            } else {
                t!("check.tuning_advice.create.fix.create_or_let_azure")
            }])
            .collect::<Vec<_>>()
            .join(" "),
        // Azure's own script, unless it includes a column left out above.
        sql: Some(r.script.clone().filter(|_| large.is_empty()).unwrap_or_else(|| {
            format!(
                "CREATE NONCLUSTERED INDEX {} ON {target} ({}){} WITH (ONLINE = ON);",
                quote(Engine::Mssql, &name),
                ix.columns.iter().map(|c| quote(Engine::Mssql, c)).collect::<Vec<_>>().join(", "),
                if include.is_empty() {
                    String::new()
                } else {
                    format!(
                        " INCLUDE ({})",
                        include.iter().map(|c| quote(Engine::Mssql, c)).collect::<Vec<_>>().join(", ")
                    )
                }
            )
        })),
        cost: None,
    };
    let fix = match &advice {
        Advice::Fast { fastest, slowest, bits } => {
            let average = if format::duration(*fastest) == format::duration(*slowest) {
                format::duration(*slowest)
            } else {
                t!(
                    "check.tuning_advice.create.average_range",
                    fastest = format::duration(*fastest),
                    slowest = format::duration(*slowest)
                )
            };
            Fix {
                summary: with_rated(t!(
                    "check.tuning_advice.create.fix.fast",
                    n = filtering.len(),
                    count = filtering.len().to_string(),
                    keys = &keys,
                    average = average,
                    bits = match (bits, ix.columns.len()) {
                        (false, _) => "none",
                        (true, 1) => "one",
                        (true, _) => "many",
                    },
                    columns = &columns,
                    copies = if ix.include.is_empty() { "no" } else { "yes" },
                    included = ix.include.len().to_string()
                )),
                sql: None,
                cost: None,
            }
        }
        // The fix says what the existing index has and what the recommendation would add: no caveat repeats it.
        Advice::AlreadyThere(existing) => Fix {
            summary: with_rated(t!(
                "check.tuning_advice.create.fix.already_there",
                index = &existing.name,
                keys = leads(existing, ix),
                large = format::list(&large)
            )),
            sql: None,
            cost: None,
        },
        Advice::Widen { existing, lacking } => {
            f.caveats.push(t!(
                "check.tuning_advice.create.caveat.lacking",
                index = &existing.name,
                keys = leads(existing, ix),
                lacking = format::list(lacking),
                n = lacking.len()
            ));
            match crate::indexes::widen_index(snapshot, existing, lacking) {
                crate::indexes::Widening::Sql(fix) => Fix { summary: with_rated(fix.summary), ..fix },
                crate::indexes::Widening::Refused(why) => create(Some(why)),
            }
        }
        Advice::PickedElsewhere => {
            let p = picked.as_ref().expect("a plan that picked another index");
            Fix {
                summary: with_rated(t!(
                    "check.tuning_advice.create.fix.picked",
                    index = &p.other.name,
                    keys = format::list(&p.keys),
                    query = &p.query.id,
                    used = &p.used,
                    expected = rows_expected(p.estimate),
                    pages = format::count(p.pages),
                    returns = returns(p.rows)
                )),
                sql: None,
                cost: None,
            }
        }
        Advice::Create(leading) => {
            if let Some(Leading { index, keys: leading_keys, instead }) = leading {
                let (filtered, filter) = match index.filter.as_deref() {
                    Some(filter) => ("yes", crate::indexes::readable_filter(filter)),
                    None => ("no", String::new()),
                };
                let (keys, count, total) =
                    (format::list(leading_keys), leading_keys.len().to_string(), ix.columns.len().to_string());
                f.caveats.push(match (instead, &picked) {
                    (Some((id, used)), _) => t!(
                        "check.tuning_advice.create.caveat.leading_not_serving",
                        index = &index.name,
                        keys = keys,
                        filtered = filtered,
                        filter = filter,
                        count = count,
                        total = total,
                        query = *id,
                        table = &ix.table,
                        used = format::list(used)
                    ),
                    (None, Some(p)) => t!(
                        "check.tuning_advice.create.caveat.leading_picked",
                        index = &index.name,
                        keys = keys,
                        filtered = filtered,
                        filter = filter,
                        count = count,
                        total = total,
                        query = &p.query.id,
                        table = &ix.table,
                        used = &p.used,
                        expected = rows_expected(p.estimate)
                    ),
                    (None, None) => t!(
                        "check.tuning_advice.create.caveat.leading_check",
                        index = &index.name,
                        keys = keys,
                        filtered = filtered,
                        filter = filter,
                        count = count,
                        total = total,
                        table = &ix.table
                    ),
                });
            }
            create(None)
        }
    };
    // Columns left out of INCLUDE matter only for an index worth creating.
    if let Some(note) = c.large_note.filter(|_| fix.sql.is_some()) {
        f.caveats.push(note);
    }
    let same_keys = matches!(advice, Advice::AlreadyThere(_) | Advice::Widen { .. });
    let nothing_to_create =
        fix.sql.is_none() && (matches!(advice, Advice::AlreadyThere(_)) || matches!(advice, Advice::Fast { .. }));
    let picked_nothing = fix.sql.is_none() && !same_keys;
    if fix.sql.is_none() {
        f.informational();
    }
    f.fix = Some(fix);
    f.verify = vec![if nothing_to_create {
        t!("check.tuning_advice.create.verify.nothing_to_change")
    } else if let Some(p) = picked.as_ref().filter(|_| picked_nothing) {
        t!("check.tuning_advice.create.verify.picked", query = &p.query.id)
    } else if conditional {
        t!("check.tuning_advice.create.verify.if_applied", table = &ix.table)
    } else {
        match &heavy {
            Some(h) => {
                t!("check.tuning_advice.create.verify.fewer_pages", query = &h.query.id, pages = format::count(h.pages))
            }
            None => t!("check.tuning_advice.create.verify.fewer_reads", table = &ix.table),
        }
    }];
    f
}

/// What an index really leads with, as far as the recommended keys go: its own key order, which needn't be the
/// recommendation's.
fn leads(existing: &IndexStat, ix: &RecommendedIndex) -> String {
    format::list(&existing.key_names()[..ix.columns.len()].iter().map(|k| k.to_string()).collect::<Vec<_>>())
}

/// The heavy-query check's bar: a statement that takes this share of database time is one of the heaviest.
const HEAVY_SHARE: f64 = 0.05;

/// Pages a run from which a read that returns nothing is reading for nothing (the heavy-query check's own bar).
const NOTHING_READS: f64 = 100.0;

/// The statements the scan captured that compare every key of a recommended index with a value (`tenant_id = @p0 AND
/// is_deleted = 0`): those the index would find rows for by its keys. Not monitoring or session statements.
fn statements_filtering_on<'a>(snapshot: &'a Snapshot, ix: &RecommendedIndex) -> Vec<&'a QueryStat> {
    let value = |v: &str| (v.len() > 1 && v.starts_with('@')) || crate::indexes::is_literal(v) || v.starts_with("n'");
    let mut found: Vec<&QueryStat> = queries_touching(snapshot, &ix.schema, &ix.table)
        .into_iter()
        .filter(|q| !is_monitoring_query(q) && !culprit_core::sql::is_session_statement(&q.text))
        .filter(|q| ix.columns.iter().all(|c| crate::indexes::compared_to(&q.text, &ix.table, c, value)))
        .collect();
    found.sort_by(|a, b| b.total_time_ms.total_cmp(&a.total_time_ms));
    found
}

/// A statement behind a recommendation measured heavy and reading for nothing.
struct HeavyReader<'a> {
    query: &'a QueryStat,
    share: f64,
    pages: f64,
    rows: f64,
}

/// Of the statements behind a recommendation, the heaviest that takes a heavy query's share of database time and
/// reads at least [`NOTHING_READS`] pages a run to return less than a row: its filter keeps nothing of what it reads, so
/// an index on its keys finds its rows directly (the share kept is far under `INDEX_KEEPS_AT_MOST`). What the runs
/// measured outweighs Azure's estimate.
fn heavy_reader<'a>(snapshot: &Snapshot, statements: &[&'a QueryStat]) -> Option<HeavyReader<'a>> {
    let workload = snapshot.workload.as_ref()?;
    statements.iter().find_map(|&q| {
        // A heavy query's share as every surface states it (`Workload::statement_share`).
        let share = workload.statement_share(q);
        let (pages, rows) = (q.reads_per_execution()?, q.rows_per_execution()?);
        (!q.writes_rows() && share >= HEAVY_SHARE && pages >= NOTHING_READS && rows < 1.0).then_some(HeavyReader {
            query: q,
            share,
            pages,
            rows,
        })
    })
}

/// A heavy statement behind a recommendation whose plan reads the table through `used`, picked on an estimate of
/// `estimate` rows, while `other` already leads with `keys`, columns it compares with a value.
struct PickedElsewhere<'a> {
    query: &'a QueryStat,
    other: &'a IndexStat,
    keys: Vec<String>,
    used: String,
    estimate: f64,
    pages: f64,
    rows: f64,
}

/// A heavy statement with many recorded pages beside a seek estimated at few rows, while another index leads with a
/// key it compares with a value and the seek doesn't seek on. That index can serve the predicate (unfiltered, or its
/// filter as the statement writes it); the page count proves no row misestimate or reason for the choice. The
/// heavy-query finding asks for actual measurements. `None` without a plan or a seek on the table.
fn picked_elsewhere<'a>(
    snapshot: &'a Snapshot,
    ix: &RecommendedIndex,
    h: &HeavyReader<'a>,
) -> Option<PickedElsewhere<'a>> {
    let seek = first_seek(h.query, &ix.schema, &ix.table)?;
    let used = seek.index.as_deref()?.trim_matches(['[', ']', '"']).to_string();
    let estimate = seek.est_rows_total();
    if estimate * culprit_core::MISESTIMATE_FACTOR > h.pages {
        return None;
    }
    let sought = predicate_columns(seek.seek_predicate.as_deref().unwrap_or_default());
    let is_sought = |c: &str| sought.equality.iter().chain(&sought.range).any(|s| s.eq_ignore_ascii_case(c));
    let recommended = |c: &str| ix.columns.iter().any(|k| k.eq_ignore_ascii_case(c));
    snapshot
        .indexes
        .iter()
        .filter(|i| i.schema.eq_ignore_ascii_case(&ix.schema) && i.table.eq_ignore_ascii_case(&ix.table))
        .filter(|i| !i.is_disabled && !i.name.eq_ignore_ascii_case(&used))
        .filter(|i| crate::indexes::fits_index_filter(i, &[h.query]))
        .filter(|i| i.key_names().first().is_some_and(|k| recommended(k) && !is_sought(k)))
        .map(|i| {
            let keys: Vec<String> =
                i.key_names().into_iter().take_while(|k| recommended(k)).map(str::to_string).collect();
            (i, keys)
        })
        .max_by_key(|(i, keys)| (keys.len(), i.filter.is_none(), std::cmp::Reverse(i.columns.len())))
        .map(|(other, keys)| PickedElsewhere {
            query: h.query,
            other,
            keys,
            used,
            estimate,
            pages: h.pages,
            rows: h.rows,
        })
}

/// "1 row", "12 rows": what a plan expects a read to find.
fn rows_expected(rows: f64) -> String {
    let rows = if rows >= 10.0 { rows.round() } else { (rows * 10.0).round() / 10.0 };
    t!("check.tuning_advice.create.rows_expected", n = rows, count = format::count(rows))
}

/// The first step of a statement's captured plan on a table, when it seeks an index.
fn first_seek<'a>(q: &'a QueryStat, schema: &str, table: &str) -> Option<&'a culprit_core::PlanNode> {
    q.plan
        .as_ref()?
        .nodes()
        .into_iter()
        .find(|n| {
            n.object
                .as_deref()
                .and_then(split_object)
                .is_some_and(|(s, t)| s.eq_ignore_ascii_case(schema) && t.eq_ignore_ascii_case(table))
                && matches!(n.kind, NodeKind::Seek | NodeKind::Scan)
        })
        .filter(|n| n.kind == NodeKind::Seek && n.index.is_some())
}

/// The indexes a statement's captured plan seeks or scans on a table, in plan order (not the lookups back to it): how
/// the optimizer chose to read it. Empty without a plan.
fn indexes_read_through(q: &QueryStat, schema: &str, table: &str) -> Vec<String> {
    let mut used: Vec<String> = Vec::new();
    for n in q.plan.as_ref().map(|p| p.nodes()).unwrap_or_default() {
        let on_table = n
            .object
            .as_deref()
            .and_then(split_object)
            .is_some_and(|(s, t)| s.eq_ignore_ascii_case(schema) && t.eq_ignore_ascii_case(table));
        if let (true, NodeKind::Seek | NodeKind::Scan, Some(index)) = (on_table, n.kind, n.index.as_deref())
            && !used.iter().any(|u| u.eq_ignore_ascii_case(index))
        {
            used.push(index.to_string());
        }
    }
    used
}

/// The enabled index on the recommendation's table that leads with the most of its keys, in any order, when one leads
/// with any: the queries behind the recommendation may already use it. The unfiltered one first, then the narrowest.
fn leading_with_some<'a>(snapshot: &'a Snapshot, ix: &RecommendedIndex) -> Option<(&'a IndexStat, Vec<String>)> {
    snapshot
        .indexes
        .iter()
        .filter(|i| {
            i.schema.eq_ignore_ascii_case(&ix.schema) && i.table.eq_ignore_ascii_case(&ix.table) && !i.is_disabled
        })
        .filter_map(|i| {
            let keys: Vec<String> = i
                .key_names()
                .into_iter()
                .take_while(|k| ix.columns.iter().any(|c| c.eq_ignore_ascii_case(k)))
                .map(str::to_string)
                .collect();
            (!keys.is_empty()).then_some((i, keys))
        })
        .max_by_key(|(i, keys)| (keys.len(), i.filter.is_none(), std::cmp::Reverse(i.columns.len())))
}

/// Azure SQL's recommendation to drop an index nothing else flags.
pub struct DropIndex<'s> {
    r: &'s TuningRecommendation,
    ix: &'s RecommendedIndex,
    existing: &'s IndexStat,
}

pub(super) fn drop_index_advice<'s>(
    snapshot: &'s Snapshot,
    r: &'s TuningRecommendation,
    ix: &'s RecommendedIndex,
) -> Option<DropIndex<'s>> {
    let name = ix.name.as_deref()?;
    // Already gone, or the unused- and duplicate-index findings already say to drop it.
    let existing = snapshot.indexes.iter().find(|i| {
        i.schema.eq_ignore_ascii_case(&ix.schema)
            && i.table.eq_ignore_ascii_case(&ix.table)
            && i.name.eq_ignore_ascii_case(name)
    })?;
    if is_unused(snapshot, existing) || redundant_indexes(snapshot).iter().any(|(a, _)| std::ptr::eq(*a, existing)) {
        return None;
    }
    Some(DropIndex { r, ix, existing })
}

pub(super) fn drop_index_finding(snapshot: &Snapshot, d: DropIndex<'_>) -> Finding {
    let DropIndex { r, ix, existing } = d;
    let table_name = format!("{}.{}", ix.schema, ix.table);
    let mut f = finding(
        "tuning-advice",
        &format!("drop-index:{table_name}.{}", existing.name),
        Category::Indexes,
        subject(SubjectKind::Index, format!("{table_name}.{}", existing.name)),
    );
    f.title = t!("check.tuning_advice.drop.title", index = &existing.name, table = &ix.table);
    let reason = r.reason.as_deref().filter(|s| !s.is_empty());
    f.summary = t!(
        "check.tuning_advice.drop.summary",
        index = &existing.name,
        reasoned = if reason.is_some() { "yes" } else { "no" },
        reason = reason.unwrap_or_default(),
        dated = if r.valid_since.is_some() { "yes" } else { "no" },
        since = r.valid_since.as_deref().map(|s| format::friendly_date(s, &snapshot.taken_at)).unwrap_or_default(),
        counted = if existing.reads.is_some() { "yes" } else { "no" },
        reads = existing.reads.map(format::count).unwrap_or_default(),
        counted_since = existing.reads.map(|_| since_phrase(snapshot)).unwrap_or_default()
    );
    f.severity = Severity::Low;
    f.impact.score = base_score(f.severity) + r.score.unwrap_or(0.0) / 20.0;
    f.impact.set_label(t!("check.tuning_advice.azure_recommended"));
    f.confidence = Confidence::Medium;
    if let Some(size) = existing.size_bytes {
        f.metrics.push(Metric::said(culprit_core::key!("common.metric.size"), size, Unit::Bytes));
    }
    if let Some(reads) = existing.reads {
        f.metrics.push(Metric::said(culprit_core::key!("common.metric.reads"), reads, Unit::Count));
    }
    let target = qualified(Engine::Mssql, &existing.schema, &existing.table);
    f.fix = Some(Fix {
        summary: t!("check.tuning_advice.drop.fix"),
        sql: Some(format!("ALTER INDEX {} ON {target} DISABLE;", quote(Engine::Mssql, &existing.name))),
        cost: None,
    });
    if let Some(definition) = &existing.definition {
        f.caveats.push(t!("check.tuning_advice.drop.caveat.definition", definition = definition));
    }
    f.verify = vec![t!("check.tuning_advice.drop.verify", table = &ix.table)];
    f
}
