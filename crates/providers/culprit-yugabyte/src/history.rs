//! QPM's independent, evictable plan cache. These are not pg_stat_statements lifetime totals or measured operators.
use crate::read::{NoticeRead, query_bounded};
use crate::{probe::Probe, sources};
use culprit_core::{Coverage, Error, Plan, PlanHistory, PlanHistoryKind, PlanKind, PlanVariant, Result, Workload};
use std::collections::BTreeMap;

fn relation(probe: &Probe) -> String {
    let native = if probe.global { "pg_catalog.\"gv$yb_pg_stat_plans\"" } else { "pg_catalog.yb_pg_stat_plans" };
    guarded_relation(native)
}
pub(crate) fn guarded_relation(native: &str) -> String {
    // Keep the permission check in the same command as the materializing native view. OFFSET 0 preserves
    // the lateral dependency: a revoked role must fail the one-time filter before the view is evaluated.
    format!(
        "(SELECT safe.* FROM (SELECT {} AS allowed OFFSET 0) permission CROSS JOIN LATERAL
        (SELECT native.* FROM {native} native WHERE permission.allowed OFFSET 0) safe) guarded_plans",
        crate::probe::ALL_STATS
    )
}
pub(crate) fn authorize(probe: &Probe) -> Result<()> {
    // The view materializes all entries before SQL filters apply. On verified 2026.1.2, its
    // foreign-role redaction path segfaults even for a count(*) filtered to the caller's userid.
    // pg_read_all_stats avoids that path without requiring a superuser or changing server settings.
    if probe.statement_role.is_some() {
        return Err(Error::new(
            culprit_core::ErrorCode::PermissionDenied,
            culprit_core::message!("provider.yugabyte.error.plan_history_privilege"),
        )
        .with_hint(culprit_core::message!("provider.yugabyte.error.plan_history_privilege_hint")));
    }
    Ok(())
}

pub(crate) async fn collect(
    session: &mut impl NoticeRead,
    probe: &Probe,
    workload: &mut Workload,
    coverage: &mut Coverage,
) {
    let source_message = culprit_core::message!(
        "model.observation.source.ysql",
        relation = "yb_pg_stat_plans",
        scope = if probe.global {
            "cluster"
        } else if probe.node.is_some() {
            "node"
        } else {
            "unknown"
        },
        node = probe.node.as_deref().unwrap_or_default(),
        limited = "no",
        role = ""
    );
    let source = source_message.render();
    let note_message = culprit_core::message!("provider.yugabyte.plan_history.note");
    for q in &mut workload.queries {
        q.plans.clear();
        q.plan_history = Some(PlanHistory {
            source_message: Some(source_message.clone()),
            note_message: Some(note_message.clone()),

            source: source.clone(),
            kind: PlanHistoryKind::Cache,
            captured_plan_id: None,
            captured_plan: None,
            note: note_message.render(),
        });
    }
    let ids: Vec<_> = workload.queries.iter().filter_map(|q| q.id.parse::<i64>().ok()).collect();
    if ids.is_empty() {
        return;
    }
    let ids = ids.iter().map(ToString::to_string).collect::<Vec<_>>().join(",");
    // Project no execution parameters, hints or plan bodies. Global-view filters apply before aggregation.
    let sql = format!(
        "SELECT queryid::text AS query,planid::text AS id,sum(calls::float8) AS calls,
        sum(calls::float8*avg_exec_time) AS ms,
        sum(calls::float8*avg_exec_time)/NULLIF(sum(calls::float8),0) AS mean_ms,
        max(max_exec_time)::float8 AS max_ms,{} AS first,{} AS last
        FROM {} WHERE dbid={} AND queryid IN ({ids}) AND calls>0 AND {}
        GROUP BY queryid,planid ORDER BY ms DESC,queryid,planid LIMIT 1001",
        crate::probe::timestamp("min(first_used)"),
        crate::probe::timestamp("max(last_used)"),
        relation(probe),
        probe.oid,
        crate::probe::ALL_STATS
    );
    let before = session.notices();
    let result = match authorize(probe) {
        Ok(()) => {
            async {
                let rows = query_bounded(session, &sql, 1000).await?;
                crate::probe::require_all_stats_from(session).await?;
                Ok(rows)
            }
            .await
        }
        Err(error) => Err(error),
    };
    let truncated = result.as_ref().is_ok_and(|(_, truncated)| *truncated);
    let Some(rows) = coverage.read_rows(&sources::PLAN_HISTORY, result.map(|(rows, _)| rows)) else {
        for q in &mut workload.queries {
            if let Some(history) = &mut q.plan_history {
                let message = culprit_core::message!("provider.yugabyte.plan_history.unreadable");
                history.set_note(message);
            }
        }
        return;
    };
    let mut partial = truncated || session.notices() != before;
    let mut plans: BTreeMap<String, Vec<PlanVariant>> = BTreeMap::new();
    for row in rows {
        let (Some(query), Some(id), Some(calls), Some(ms), Some(mean)) = (
            row.text("query"),
            row.text("id"),
            row.number("calls").filter(|n| *n > 0.0),
            row.number("ms").filter(|n| *n >= 0.0),
            row.number("mean_ms").filter(|n| *n >= 0.0),
        ) else {
            partial = true;
            continue;
        };
        if query.parse::<i64>().is_err() || id.parse::<i64>().is_err() {
            partial = true;
            continue;
        }
        plans.entry(query).or_default().push(PlanVariant {
            id,
            executions: calls,
            total_time_ms: ms,
            mean_time_ms: mean,
            first_seen: row.text("first"),
            last_seen: row.text("last"),
            is_forced: false,
            min_time_ms: None,
            max_time_ms: row.number("max_ms").filter(|n| *n >= 0.0),
            stddev_time_ms: None,
            compiled: None,
            cpu_time_ms: None,
            logical_reads: None,
        });
    }
    for q in &mut workload.queries {
        q.plans = plans.remove(&q.id).unwrap_or_default();
    }
    if partial {
        coverage
            .partial(&sources::PLAN_HISTORY, culprit_core::message!("provider.yugabyte.coverage.plan_history_partial"));
        let partial = culprit_core::message!("provider.yugabyte.plan_history.partial");
        for q in &mut workload.queries {
            if let Some(history) = &mut q.plan_history {
                history.append_note(partial.clone());
            }
        }
    }
}

pub(crate) async fn capture(
    session: &mut impl NoticeRead,
    probe: &Probe,
    workload: &mut Workload,
    requested: usize,
    budget: &mut culprit_core::limits::ResultBudget,
    coverage: &mut Coverage,
) {
    if let Err(error) = authorize(probe) {
        coverage.read::<()>(&sources::PLAN_HISTORY, Err(error));
        return;
    }
    let wanted: Vec<_> = workload
        .queries
        .iter()
        .take(requested)
        .enumerate()
        .filter_map(|(order, q)| Some((q.id.parse::<i64>().ok()?, q.plans.first()?.id.parse::<i64>().ok()?, order)))
        .collect();
    if wanted.is_empty() {
        return;
    }
    let pairs = wanted.iter().map(|(q, p, _)| format!("({q}::bigint,{p}::bigint)")).collect::<Vec<_>>().join(",");
    let priority = wanted.iter().map(|(q, _, order)| format!("WHEN {q} THEN {order}")).collect::<Vec<_>>().join(" ");
    // One scan of the native cache, rather than one full get_all_entries() scan for each requested plan.
    let sql = format!(
        "WITH entries AS (SELECT DISTINCT ON (queryid) queryid,planid,plan
        FROM {} WHERE dbid={} AND (queryid,planid) IN ({pairs}) AND {}
        ORDER BY queryid,last_used DESC)
        SELECT queryid::text AS query,planid::text AS id,plan FROM entries
        ORDER BY CASE queryid {priority} END",
        relation(probe),
        probe.oid,
        crate::probe::ALL_STATS
    );
    let result: Result<_> = async {
        let rows = query_bounded(session, &sql, requested).await?;
        crate::probe::require_all_stats_from(session).await?;
        Ok(rows)
    }
    .await;
    let Ok((rows, truncated)) = result else {
        coverage.partial(
            &sources::PLAN_HISTORY,
            culprit_core::message!("provider.yugabyte.coverage.plan_bodies_unfetched"),
        );
        return;
    };
    if truncated {
        coverage
            .partial(&sources::PLAN_HISTORY, culprit_core::message!("provider.yugabyte.coverage.plan_bodies_budget"));
    }
    if rows.len() < wanted.len() {
        coverage
            .partial(&sources::PLAN_HISTORY, culprit_core::message!("provider.yugabyte.coverage.plan_entries_missing"));
    }
    let mut bodies = BTreeMap::new();
    for row in rows {
        let Some(raw) = row.text("plan") else {
            coverage
                .partial(&sources::PLAN_HISTORY, culprit_core::message!("provider.yugabyte.coverage.plan_body_hidden"));
            continue;
        };
        if !budget.retain(raw.len()) {
            coverage.partial(
                &sources::PLAN_HISTORY,
                culprit_core::message!("provider.yugabyte.coverage.plan_bodies_shared_budget"),
            );
            break;
        }
        match decode_stored(&raw) {
            Ok(plan) => {
                bodies.insert((row.string("query"), row.string("id")), plan);
            }
            Err(_) => coverage.partial(
                &sources::PLAN_HISTORY,
                culprit_core::message!("provider.yugabyte.coverage.plan_body_truncated"),
            ),
        }
    }
    for q in workload.queries.iter_mut().take(requested) {
        if let Some(id) = q.plans.first().map(|p| p.id.clone())
            && let (Some(history), Some(plan)) = (&mut q.plan_history, bodies.remove(&(q.id.clone(), id.clone())))
        {
            history.captured_plan_id = Some(id);
            history.captured_plan = Some(plan);
        }
    }
}

fn decode_stored(raw: &str) -> Result<Plan> {
    let mut plan = crate::plan::parse(raw, PlanKind::Estimated, None).map_err(|_| {
        Error::invalid(culprit_core::message!("provider.yugabyte.error.retained_plan_unreadable"))
            .with_hint(culprit_core::message!("provider.yugabyte.error.retained_plan_unreadable_hint"))
    })?;
    crate::plan::redacted_constants(&mut plan);
    Ok(plan)
}

pub(crate) async fn stored(
    session: &mut impl NoticeRead,
    probe: &Probe,
    query: &str,
    id: &str,
) -> Result<Option<Plan>> {
    authorize(probe)?;
    let query =
        query.parse::<i64>().map_err(|_| Error::invalid(culprit_core::message!("provider.yugabyte.error.query_id")))?;
    let id =
        id.parse::<i64>().map_err(|_| Error::invalid(culprit_core::message!("provider.yugabyte.error.plan_id")))?;
    let sql = format!(
        "SELECT plan FROM {} WHERE dbid={} AND queryid={query} AND planid={id} AND {}
        ORDER BY last_used DESC LIMIT 1",
        relation(probe),
        probe.oid,
        crate::probe::ALL_STATS
    );
    let (rows, truncated) = query_bounded(session, &sql, 1).await?;
    crate::probe::require_all_stats_from(session).await?;
    if truncated {
        return Err(Error::invalid(culprit_core::message!("provider.yugabyte.error.stored_plan_too_big")));
    }
    let Some(raw) = rows.first().and_then(|r| r.text("plan")) else {
        return Ok(None);
    };
    decode_stored(&raw).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Why retained plans can't be read, word for word.
    #[test]
    fn refusals_in_english() {
        let error = decode_stored("{\"Plan\":").unwrap_err();
        assert_eq!(
            (error.message.as_str(), error.hint.as_deref()),
            (
                "The retained YSQL plan is truncated, hidden, or not JSON",
                Some(
                    "QPM must already retain complete JSON plans. Review yb_pg_stat_plans_plan_format; Culprit does \
                     not change it during collection."
                )
            )
        );
        let probe = Probe {
            now: "2026-11-08T10:00:00.000000Z".into(),
            started: None,
            version: "2025.2.0.0".into(),
            compatibility: 150_002,
            database: "shop".into(),
            oid: 16384,
            node: None,
            cluster: false,
            global: false,
            statements: true,
            statement_schema: None,
            metrics: false,
            rpc_metrics: false,
            statement_role: Some(16385),
        };
        let error = authorize(&probe).unwrap_err();
        assert_eq!(
            (error.message.as_str(), error.hint.as_deref()),
            (
                "Retained YSQL plan history requires pg_read_all_stats; the native cache cannot be read safely without it",
                Some(
                    "An administrator can grant pg_read_all_stats to the monitoring login. Culprit skips this source \
                     without that grant."
                )
            )
        );
    }
}
