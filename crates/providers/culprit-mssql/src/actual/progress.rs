//! One current request, its selected statement and its operator rows share an owned native reader. No current
//! point establishes the original Live row or a cross-capture member/process identity.
use super::{Availability, Layout, no_profiling, own_estimate};
use crate::{
    activity::participants::Check,
    conn::Pool,
    plan_xml,
    read::{NativeRead, Parameter, ReadError, integer, invalid_data, optional_text, text},
    value::Rec,
};
use culprit_core::{
    Engine, QueryProgress, Result, session_number, t,
    telemetry::{CollectionStop, NativeIdentity, ObservationBounds, canonical_time},
};
use std::sync::Arc;
mod sql;
#[cfg(test)]
mod tests;

struct Scope {
    database: i64,
    name: String,
    reader: i64,
    login: String,
    role: Option<String>,
}
impl Scope {
    fn read(row: &Rec) -> Result<Self> {
        let role = optional_text(row, "database_updateability")?;
        if role.as_deref().is_some_and(|role| !matches!(role, "READ_WRITE" | "READ_ONLY")) {
            return Err(invalid_data());
        }
        let scope = Self {
            database: integer(row, "selected_database_id")?,
            name: text(row, "selected_database_name")?,
            reader: integer(row, "reader_session_id")?,
            login: crate::sql_time::native_time(&text(row, "reader_session_generation")?)?,
            role,
        };
        if scope.database <= 0 || scope.reader <= 0 {
            return Err(invalid_data());
        }
        Ok(scope)
    }
    fn matches(&self, other: &Self) -> bool {
        self.database == other.database
            && self.name == other.name
            && self.reader == other.reader
            && self.login == other.login
            && self.role == other.role
    }
}

struct Point {
    row: Rec,
    check: Check,
    scope: Scope,
}
enum Reading {
    Unknown { at: String, ambiguous: bool },
    Changed { at: String },
    Observed { point: Box<Point>, profiles: Vec<Rec>, verified: bool },
}

pub(crate) async fn query_progress(pool: &Arc<Pool>, session: &str) -> Result<QueryProgress> {
    let id = session_number(session)?;
    if id == 0 || id > i16::MAX as u64 || id.to_string() != session {
        return Err(invalid_data());
    }
    let captured = pool.observe(|reader| Box::pin(read(reader, id))).await?;
    present(session, captured)
}

async fn read<R: NativeRead>(reader: &mut R, id: u64) -> Result<Reading> {
    let layout = Layout::read(reader).await?;
    let generation = reader.connection_generation();
    let mut requests = reader
        .query(&sql::request(layout.live_xml == Availability::Available), vec![Parameter::Integer(id as i64)], 2)
        .await
        .map_err(ReadError::into_error)?;
    if generation != reader.connection_generation() {
        return Err(CollectionStop::SourceChanged.error());
    }
    let row = if requests.len() == 1 { requests.remove(0) } else { return Err(invalid_data()) };
    let count = integer(&row, "request_count")?;
    if count > 2 {
        return Err(invalid_data());
    }
    let at = canonical_time(&text(&row, "observed_at")?)?;
    if count != 1 {
        return Ok(Reading::Unknown { at, ambiguous: count > 1 });
    }
    let check = Check::decode(&row)?;
    let scope = Scope::read(&row)?;
    let Some((key, statement)) = check.selector() else { return Ok(Reading::Changed { at }) };
    if key.session.id != id.to_string()
        || statement.qualifiers.get("database_id").and_then(|id| id.parse::<i64>().ok()) != Some(scope.database)
    {
        return Err(invalid_data());
    }
    let point = Box::new(Point { row, check, scope });
    let Some((_, Some(plan))) = point.check.identities(&point.check) else {
        return Ok(Reading::Observed { point, profiles: vec![], verified: false });
    };
    if layout.profiles != Availability::Available || point.scope.role.is_none() {
        return Ok(Reading::Observed { point, profiles: vec![], verified: false });
    }
    let parameters = parameters(&key, &statement, &plan)?;
    let rows = reader.query(&sql::profiles(), parameters, sql::MAX_NODES + 2).await.map_err(ReadError::into_error)?;
    if generation != reader.connection_generation() {
        return Err(CollectionStop::SourceChanged.error());
    }
    finish(point, rows)
}

fn parameters(
    key: &culprit_core::telemetry::ActivityKey,
    statement: &NativeIdentity,
    plan: &NativeIdentity,
) -> Result<Vec<Parameter>> {
    let request = key.request.as_ref().ok_or_else(invalid_data)?;
    let number = |value: &str| value.parse::<i64>().map(Parameter::Integer).map_err(|_| invalid_data());
    let qualifier =
        |name: &str| statement.qualifiers.get(name).ok_or_else(invalid_data).and_then(|value| number(value));
    Ok(vec![
        number(&key.session.id)?,
        number(&request.id)?,
        Parameter::Text(key.session.generation.clone()),
        Parameter::Text(request.generation.clone()),
        Parameter::Text(statement.id.clone()),
        Parameter::Text(plan.id.clone()),
        qualifier("statement_start_offset")?,
        qualifier("statement_end_offset")?,
        qualifier("database_id")?,
    ])
}

fn finish(point: Box<Point>, rows: Vec<Rec>) -> Result<Reading> {
    let mut guard = None;
    let mut profiles = Vec::new();
    for row in rows {
        match text(&row, "row_kind")?.as_str() {
            "guard" if guard.is_none() => guard = Some(row),
            "profile" => profiles.push(row),
            _ => return Err(invalid_data()),
        }
    }
    if profiles.len() > sql::MAX_NODES {
        return Err(CollectionStop::RowLimit.error());
    }
    let guard = guard.ok_or_else(invalid_data)?;
    let check = Check::decode(&guard)?;
    let scope = Scope::read(&guard)?;
    if integer(&guard, "request_count")? != 1
        || !check.matches_check(&point.check)
        || !scope.matches(&point.scope)
        || ObservationBounds::new(&point.check.at, &check.at).is_none()
    {
        return Ok(Reading::Changed { at: check.at });
    }
    let mut nodes = std::collections::BTreeSet::new();
    for profile in &profiles {
        let node = integer(profile, "node_id")?;
        if node < 0 || node > u32::MAX as i64 || !nodes.insert(node) {
            return Err(invalid_data());
        }
    }
    let verified = point.scope.role.is_some();
    Ok(Reading::Observed { point, profiles, verified })
}

fn present(session: &str, reading: Reading) -> Result<QueryProgress> {
    let (point, profiles, verified) = match reading {
        Reading::Unknown { at, ambiguous } => {
            let mut progress = QueryProgress::unobserved(session, Engine::Mssql, at);
            if ambiguous {
                progress.note = Some(t!("provider.mssql.progress.ambiguous_request"));
            }
            return Ok(progress);
        }
        Reading::Changed { at } => {
            let mut progress = QueryProgress::unobserved(session, Engine::Mssql, at);
            progress.note = Some(t!("provider.mssql.progress.changed_request"));
            return Ok(progress);
        }
        Reading::Observed { point, profiles, verified } => (point, profiles, verified),
    };
    let row = &point.row;
    let mut progress = QueryProgress::unobserved(session, Engine::Mssql, point.check.at.clone());
    progress.running = Some(true);
    progress.state = row.str("status");
    progress.wait = row.str("wait_type");
    progress.wait_ms = row.f64("wait_time").filter(|value| *value > 0.0);
    progress.elapsed_ms = row.f64("elapsed_ms");
    progress.cpu_ms = row.f64("cpu_ms");
    progress.logical_reads = row.f64("logical_reads");
    progress.statement = row.str("statement_text").map(|sql| sql.trim().to_owned());
    if !verified {
        progress.note = Some(t!("provider.mssql.progress.unverified_request"));
        return Ok(progress);
    }
    let hash = point.check.query_hash().map(|hash| format!("0x{hash}"));
    let parse = |field| row.str(field).and_then(|xml| plan_xml::parse_native_statement(&xml, hash.as_deref()).ok());
    progress.plan = parse("live_plan").or_else(|| parse("cached_plan")).map(|mut plan| {
        plan.raw = None;
        plan
    });
    if let Some(plan) = &progress.plan {
        let ids = plan.nodes().into_iter().map(|node| node.id).collect::<std::collections::BTreeSet<_>>();
        if profiles.iter().all(|row| row.f64("node_id").is_some_and(|id| ids.contains(&(id as u32)))) {
            progress.nodes = super::node_progress(&profiles, Some(plan));
        }
    }
    match row.f64("percent_complete").filter(|value| value.is_finite() && *value > 0.0 && *value <= 100.0) {
        Some(value) => {
            progress.percent = Some(value / 100.0);
            progress.percent_basis = Some(own_estimate(row.string("command").trim()));
            let percent = progress.percent;
            let basis = progress.percent_basis.clone();
            progress.summarize_nodes();
            progress.percent = percent;
            progress.percent_basis = basis;
        }
        None => progress.summarize_nodes(),
    }
    if progress.nodes.is_empty() {
        progress.note = Some(if progress.plan.is_none() || !profiles.is_empty() {
            t!("provider.mssql.progress.unverified_request")
        } else {
            no_profiling()
        });
    }
    // Reader connection identity is not native compute/process authority. No cross-poll plan cache key is issued.
    progress.request_key = None;
    Ok(progress)
}
