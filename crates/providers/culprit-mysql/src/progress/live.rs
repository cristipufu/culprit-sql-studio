//! Existing interactive display and optional native plan inspection; no application statement is executed.
use super::*;
use crate::{plan_json, plan_maria};
use culprit_core::format;
use culprit_core::{Plan, PlanKind, t};

/// What MySQL can say about a running statement, where it says no more.
fn no_operator_progress() -> String {
    t!("provider.mysql.progress.no_operator_progress")
}

/// [`no_operator_progress`], when the plan it is executing couldn't be read either (the server's `error`).
fn plan_unavailable(error: &str) -> String {
    [no_operator_progress(), t!("provider.mysql.progress.plan_unavailable", error = error)].join(" ")
}

fn mariadb_no_analyze() -> String {
    t!("provider.mysql.progress.no_show_analyze")
}

#[cfg(test)]
fn stage_progress_basis() -> String {
    t!("provider.mysql.progress.basis.stages")
}

#[cfg(test)]
fn mariadb_progress_basis() -> String {
    t!("provider.mysql.progress.basis.mariadb")
}

/// The phase a MariaDB statement that reports its progress is in: its `state` and its stage of `stages`.
pub(super) fn mariadb_phase(state: Option<&str>, stage: u64, stages: u64) -> String {
    let staged = stages > 1;
    let (stage, stages) = (format::digits(stage.to_string()), format::digits(stages.to_string()));
    match (state, staged) {
        (Some(state), true) => t!("provider.mysql.progress.state_stage", state = state, stage = stage, stages = stages),
        (Some(state), false) => state.to_string(),
        (None, _) => t!("provider.mysql.progress.stage", stage = stage, stages = stages),
    }
}

pub(super) async fn plan<R: QueryRead>(
    reader: &mut R,
    flavor: Flavor,
    version: &str,
    session: u64,
    point: &native::StatementPoint,
    with_plan: bool,
    max_plan_bytes: usize,
) -> Result<PlanEvidence> {
    if flavor == Flavor::Mysql && !with_plan {
        return Ok(PlanEvidence::default());
    }
    if flavor == Flavor::Mariadb
        && !culprit_core::version_at_least(version.strip_prefix("5.5.5-").unwrap_or(version), [10, 9, 0])
    {
        return Ok(PlanEvidence { note: Some(mariadb_no_analyze()), ..Default::default() });
    }
    let sql = match flavor {
        Flavor::Mysql => format!("EXPLAIN FORMAT=JSON FOR CONNECTION {session}"),
        Flavor::Mariadb => format!("SHOW ANALYZE FORMAT=JSON FOR {session}"),
    };
    let record = match reader.explain(&sql).await {
        Ok(record) => record,
        Err(error) if optional(&error) => {
            return Ok(PlanEvidence {
                note: Some(match flavor {
                    Flavor::Mysql => plan_unavailable(&error.message),
                    Flavor::Mariadb => mariadb_no_analyze(),
                }),
                ..Default::default()
            });
        }
        Err(error) => return Err(error),
    };
    let Some(json) = record.and_then(|record| record.first_text()) else {
        return Ok(PlanEvidence::default());
    };
    if json.len() > max_plan_bytes {
        return Err(CollectionStop::ByteLimit.error());
    }
    let kind = if flavor == Flavor::Mariadb { PlanKind::Actual } else { PlanKind::Estimated };
    let mut plan = match plan_json::parse(&json, kind, point.sql.as_deref(), Some(&point.observation.database)) {
        Ok(plan) => plan,
        Err(error) => return Ok(PlanEvidence { note: Some(plan_unavailable(&error.message)), ..Default::default() }),
    };
    plan.raw = None;
    Ok(PlanEvidence { plan: Some(Box::new(plan)), note: None })
}

#[derive(Default)]
pub(super) struct PlanEvidence {
    pub(super) plan: Option<Box<Plan>>,
    note: Option<String>,
}

pub(super) fn project(session: &str, captured: Captured, with_plan: bool) -> QueryProgress {
    let Captured { snapshot, point, plan, flavor, running, session: independent } = captured;
    if let Some(independent) = independent {
        return independent.project(session, snapshot.context);
    }
    let mut progress = QueryProgress::unobserved(session, Engine::Mysql, snapshot.context.observed_until.clone());
    progress.running = running;
    progress.current_work = Some(Box::new(snapshot));
    let Some(point) = point else {
        if progress.running.is_none() {
            progress.note = Some(t!("provider.mysql.progress.unavailable"));
        }
        return progress;
    };
    progress.taken_at = point.observation.observed_at.clone();
    progress.statement = point.sql.clone();
    progress.state = Some(point.state.clone());
    progress.wait = crate::native::activity::waiting_on_lock(Some(&point.state), false).map(|_| point.state.clone());
    progress.elapsed_ms = point
        .observation
        .native
        .timer_wait_ps
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .map(|ps| ps as f64 / 1_000_000_000.0);
    progress.rows_examined = point
        .observation
        .native
        .rows_examined
        .as_deref()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|rows| *rows > 0)
        .map(|rows| rows as f64);
    if let MysqlStageObservation::Observed { event } = &point.observation.native.stage {
        progress.phase = Some(event.event_name.trim_start_matches("stage/").replace('/', ": "));
        if let (Some(done), Some(total)) = (event.work_completed.as_deref(), event.work_estimated.as_deref())
            && let (Ok(done), Ok(total)) = (done.parse::<u64>(), total.parse::<u64>())
            && total > 0
            && done <= total
        {
            progress.phase_percent = Some(done as f64 / total as f64);
        }
    }
    progress.note = plan.note;
    if let Some(plan) = plan.plan {
        if flavor == Flavor::Mariadb {
            progress.nodes = plan_maria::progress(&plan);
        }
        if with_plan {
            progress.plan = Some(*plan);
        }
    }
    // A stage's native work estimate is never the completion percentage of the whole statement.
    progress.percent = None;
    if progress.note.is_none() && progress.phase_percent.is_none() {
        progress.note = Some(match flavor {
            Flavor::Mysql => no_operator_progress(),
            Flavor::Mariadb => mariadb_no_analyze(),
        });
    }
    progress
}

#[cfg(test)]
mod tests {
    use super::*;
    // What a running statement's progress says, word for word: its messages come from locales/en.json.
    #[test]
    fn progress_says_what_it_can_in_english() {
        let mysql = "MySQL doesn't report how far a running query has got, operator by operator. Culprit shows the plan \
                     MySQL is executing, how long it has run and, for plans MySQL counts as it goes, how many rows it \
                     has examined so far. ALTER TABLE and other long statements report stage progress when \
                     performance_schema stage events are on.";
        assert_eq!(no_operator_progress(), mysql);
        assert_eq!(
            plan_unavailable("Unknown thread id: 42"),
            format!("{mysql} Its plan couldn't be shown: Unknown thread id: 42")
        );
        assert_eq!(
            mariadb_no_analyze(),
            "MariaDB reports a running statement's operators through SHOW ANALYZE, which needs MariaDB 10.9 or newer. \
             Culprit shows how long it has run and, for plans MariaDB counts as it goes, how many rows it has examined \
             so far."
        );
        assert_eq!(stage_progress_basis(), "MySQL's own stage progress (WORK_COMPLETED of WORK_ESTIMATED)");
        assert_eq!(mariadb_progress_basis(), "MariaDB's own progress report for this statement");
        assert_eq!(mariadb_phase(Some("altering table"), 2, 3), "altering table (stage 2 of 3)");
        assert_eq!(mariadb_phase(Some("copy to tmp table"), 1, 1), "copy to tmp table");
        assert_eq!(mariadb_phase(None, 1, 2), "stage 1 of 2");
    }
}
