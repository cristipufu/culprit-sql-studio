//! What the statement checks' shared helpers say, word for word, in the shapes no check's own test reaches: their
//! words come from locales/en.json, and moving them there must not change a word of English.

use super::*;

#[test]
fn many_rows_in_one_statement_are_said_in_each_engines_terms() {
    let said: Vec<String> = Engine::ALL
        .iter()
        .map(|&e| format!("{e}: {} | {} | {}", many_keys(e, "UPDATE"), many_keys(e, "DELETE"), bulk_insert(e)))
        .collect();
    assert_eq!(
        said,
        [
            "SQL Server: an UPDATE over a list of keys, or joined to a table-valued parameter | an DELETE over a list of keys, or joined to a table-valued parameter | a multi-row VALUES, a table-valued parameter, or a bulk insert",
            "PostgreSQL: an UPDATE over an array of keys (`= ANY($1)`), or joined to a VALUES list | an DELETE over an array of keys (`= ANY($1)`), or joined to a VALUES list | a multi-row VALUES, arrays unnested into rows, or COPY",
            "YugabyteDB: an UPDATE over an array of keys (`= ANY($1)`), or joined to a VALUES list | an DELETE over an array of keys (`= ANY($1)`), or joined to a VALUES list | a multi-row VALUES, arrays unnested into rows, or COPY",
            "MySQL: an UPDATE over a list of keys (`IN (…)`), or joined to a temporary table of the changes | an DELETE over a list of keys (`IN (…)`), or joined to a temporary table of the changes | a multi-row VALUES, or LOAD DATA",
            "SQLite: an UPDATE over a list of keys (`IN (…)`), or joined to a temporary table of the changes | an DELETE over a list of keys (`IN (…)`), or joined to a temporary table of the changes | a multi-row VALUES",
        ]
    );
}

#[test]
fn a_statements_average_without_a_workload_is_its_average() {
    let mut s = crate::waits::snapshot_with(Engine::Postgres);
    let q = s.workload.as_ref().unwrap().queries[0].clone();
    s.workload = None;
    let labels: Vec<String> = average_metrics(&s, &q).into_iter().map(|m| m.label).collect();
    assert_eq!(labels, ["Average duration"]);
}

#[test]
fn when_a_change_came_reads_as_people_write_it() {
    use super::regression::{short_when, when_phrase};
    let said: Vec<String> = ["2026-10-09T14:00:00Z", "2026-12-25T00:05:00+02:00", "2026-03-01", "soon"]
        .iter()
        .map(|at| format!("{} | {}", short_when(at), when_phrase(at)))
        .collect();
    assert_eq!(
        said,
        [
            "Oct 9, 14:00 UTC | on Oct 9 at 14:00 UTC",
            "Dec 25, 00:05 UTC | on Dec 25 at 00:05 UTC",
            "2026-03-01 | at 2026-03-01",
            "soon | at soon",
        ]
    );
}
