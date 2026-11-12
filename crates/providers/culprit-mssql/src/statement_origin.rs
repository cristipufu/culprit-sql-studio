//! One owner for SQL tagging, template identity and provenance decoding, shared by every native source.
use crate::{read::integer, value::Rec};
use culprit_core::{Engine, telemetry::StatementOrigin};
pub(crate) const PROVENANCE_PREFIX_CHARACTERS: usize = 2048;
pub(crate) const OWN_QUERY_MARKER: &str = "/* culprit */";
pub(crate) const MONITORING_QUERY_MARKER: &str = "/* culprit:monitor */";

/// Marks each recorded DML statement. Other monitoring commands carry the same provenance on the batch.
pub(crate) fn monitoring_batch(sql: &str) -> String {
    let template = template_id(sql);
    tag_native_batch(sql, &format!("{OWN_QUERY_MARKER} {MONITORING_QUERY_MARKER} /* culprit:template:{template} */"))
}
fn tag_native_batch(sql: &str, marker: &str) -> String {
    let marked = culprit_core::sql::mark_mssql_statements(sql, marker);
    // Control and metadata commands such as SET, KILL and DBCC need a batch marker even when Query Store does
    // not retain them as DML statements. User-requested actions keep generic provenance, without a template ID.
    if marked == sql { format!("{marker}\n{sql}") } else { marked }
}
pub(crate) fn template_id(sql: &str) -> String {
    culprit_core::telemetry::digest(sql.as_bytes())
}

/// A requested command has generic Culprit ownership, without a monitoring template identity.
pub(crate) fn requested_batch(sql: &str) -> String {
    tag_native_batch(sql, OWN_QUERY_MARKER)
}

/// A native pushdown may exclude a proven marker, but never calls retained/ambiguous text application-owned.
/// Only a first marker before any quote, quoted name or comment opener proves an actual outer block comment.
/// The bounded prefix makes this a conservative candidate filter; full provenance remains the decoder's job.
pub(crate) fn application_candidates_sql(column: &str) -> String {
    let text = format!("LEFT(({column}) COLLATE Latin1_General_100_BIN2,{PROVENANCE_PREFIX_CHARACTERS})");
    let position = format!("CHARINDEX(N'{OWN_QUERY_MARKER}',{text})");
    let before = format!("LEFT({text},CASE WHEN {position}>0 THEN {position}-1 ELSE 0 END)");
    let ambiguous = ["NCHAR(39)", "NCHAR(34)", "N'['", "N'--'", "N'/*'"]
        .into_iter()
        .map(|delimiter| format!("CHARINDEX({delimiter},{before})>0"))
        .collect::<Vec<_>>()
        .join(" OR ");
    // Outside this small ASCII prefix subset, leave eligibility to the full decoder rather than infer ownership.
    let unusual = format!("PATINDEX(N'%[^'+NCHAR(9)+NCHAR(10)+NCHAR(13)+N' -~]%',COALESCE({before},N''))>0");
    format!("({column} IS NULL OR {position}=0 OR {ambiguous} OR {unusual})")
}

pub(crate) struct Provenance {
    pub origin: StatementOrigin,
    pub template_id: Option<String>,
}
pub(crate) fn read(row: &Rec, text: Option<&str>) -> Provenance {
    let Some(text) = text else { return Provenance { origin: internal(row), template_id: None } };
    let Ok(Some(comments)) = culprit_core::sql::block_comments(text, Engine::Mssql) else {
        return Provenance { origin: StatementOrigin::Unknown, template_id: None };
    };
    from_comments(row, &comments)
}
/// A truncated prefix can prove a completed Culprit marker, but cannot prove application ownership.
pub(crate) fn read_prefix(row: &Rec, text: Option<&str>, complete: bool) -> Provenance {
    if complete {
        return read(row, text);
    }
    let Some(text) = text else { return Provenance { origin: internal(row), template_id: None } };
    let Ok(Some(comments)) = culprit_core::sql::block_comments_in_prefix(text, Engine::Mssql) else {
        return Provenance { origin: StatementOrigin::Unknown, template_id: None };
    };
    let mut provenance = from_comments(row, &comments);
    if !provenance.origin.is_culprit() && provenance.origin != StatementOrigin::EngineInternal {
        provenance.origin = StatementOrigin::Unknown;
    }
    provenance
}
fn from_comments(row: &Rec, comments: &[&str]) -> Provenance {
    let origin = if comments.contains(&MONITORING_QUERY_MARKER) {
        StatementOrigin::CulpritMonitoring
    } else if comments.contains(&OWN_QUERY_MARKER) {
        StatementOrigin::Culprit
    } else {
        match (integer(row, "engine_internal"), integer(row, "cloud_internal")) {
            (Ok(1), _) | (_, Ok(1)) => StatementOrigin::EngineInternal,
            (Ok(0), Ok(0)) => StatementOrigin::Application,
            _ => StatementOrigin::Unknown,
        }
    };
    let template_id = (origin == StatementOrigin::CulpritMonitoring).then(|| template(comments)).flatten();
    Provenance { origin, template_id }
}
fn internal(row: &Rec) -> StatementOrigin {
    if matches!(integer(row, "engine_internal"), Ok(1)) || matches!(integer(row, "cloud_internal"), Ok(1)) {
        StatementOrigin::EngineInternal
    } else {
        StatementOrigin::Unknown
    }
}
fn template(comments: &[&str]) -> Option<String> {
    let mut found = None;
    for comment in comments {
        if let Some(id) = comment.strip_prefix("/* culprit:template:").and_then(|value| value.strip_suffix(" */")) {
            if id.len() != 64 || !id.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) {
                return None;
            }
            if found.is_some_and(|before| before != id) {
                return None;
            }
            found = Some(id);
        }
    }
    found.map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Val;
    #[test]
    fn generated_batches_round_trip_monitoring_and_requested_command_ownership_separately() {
        let row = Rec::of(&[("engine_internal", Val::Int(0)), ("cloud_internal", Val::Int(0))]);
        for sql in [
            "SELECT N'/* culprit */' AS label",
            "WITH x AS(SELECT 1 AS n) SELECT n FROM x",
            "SET LOCK_TIMEOUT 2000",
            "DBCC SHOW_STATISTICS(N't',N'i') WITH HISTOGRAM",
        ] {
            let monitoring = monitoring_batch(sql);
            let classified = read(&row, Some(&monitoring));
            assert_eq!(classified.origin, StatementOrigin::CulpritMonitoring);
            assert_eq!(classified.template_id, Some(template_id(sql)));
            let requested = requested_batch(sql);
            let classified = read(&row, Some(&requested));
            assert_eq!(classified.origin, StatementOrigin::Culprit);
            assert!(classified.template_id.is_none(), "requested commands are not collector templates");
        }
    }

    #[test]
    fn provenance_uses_real_comments_and_keeps_unknowns_explicit() {
        let user = Rec::of(&[("engine_internal", Val::Int(0)), ("cloud_internal", Val::Int(0))]);
        assert_eq!(
            read(&user, Some("SELECT /* culprit */ /* culprit:monitor */ 1")).origin,
            StatementOrigin::CulpritMonitoring
        );
        assert_eq!(read(&user, Some("SELECT /* culprit */ 1")).origin, StatementOrigin::Culprit);
        for sql in [
            "SELECT N'/* culprit */'",
            "SELECT [/* culprit */] FROM t",
            "SELECT 1 -- /* culprit */",
            "SELECT /* outer /* culprit */ */ 1",
        ] {
            assert_eq!(read(&user, Some(sql)).origin, StatementOrigin::Application);
        }
        assert_eq!(read(&user, None).origin, StatementOrigin::Unknown);
        assert_eq!(read(&user, Some("SELECT /* unfinished")).origin, StatementOrigin::Unknown);
        let internal = Rec::of(&[("engine_internal", Val::Int(1))]);
        assert_eq!(read(&internal, None).origin, StatementOrigin::EngineInternal);
    }
}

#[cfg(test)]
mod native_tests;
