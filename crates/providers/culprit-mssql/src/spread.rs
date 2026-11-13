//! What parameter sensitivity needs beyond the workload numbers: the values each plan was compiled for, and the
//! variants SQL Server 2022's parameter-sensitive plan optimization keeps for a statement.

use std::collections::HashMap;

use culprit_core::{PlanVariant, QueryStat};

use crate::plan_xml;
use crate::workload::id_list;

/// Queries with several plans get every plan's compiled values; bounded so a scan stays cheap.
const MAX_QUERIES: usize = 20;
const MAX_PLANS: usize = 60;

pub(crate) async fn attach<R: crate::read::NativeRead>(
    reader: &mut crate::query_store::Reader<R>,
    window_minutes: u32,
    queries: &mut [QueryStat],
) -> culprit_core::Result<()> {
    for q in queries.iter_mut() {
        let compiled = q.plan.as_ref().and_then(|p| p.facts().compiled_parameters.clone());
        if let (Some(first), Some(compiled)) = (q.plans.first_mut(), compiled) {
            first.compiled = Some(compiled);
        }
    }
    let wanted: Vec<String> = queries
        .iter()
        .filter(|q| q.plans.len() >= 2)
        .take(MAX_QUERIES)
        .flat_map(|q| q.plans.iter().filter(|p| p.compiled.is_none()).map(|p| p.id.clone()))
        .take(MAX_PLANS)
        .collect();
    let definitions = if wanted.is_empty() {
        vec![]
    } else {
        match reader.definitions(&wanted, crate::query_store::definitions::Kind::Plan, 2 * 1024 * 1024).await {
            Ok(rows) => rows,
            Err(error) if crate::read::stops_remaining_reads(&error) => return Err(error),
            Err(_) => vec![],
        }
    };
    {
        let compiled: HashMap<String, String> = definitions
            .into_iter()
            .filter_map(|row| {
                let plan = plan_xml::parse(row.payload.text()?).ok()?;
                Some((row.id, plan.facts().compiled_parameters.clone()?))
            })
            .collect();
        for p in queries.iter_mut().flat_map(|q| q.plans.iter_mut()) {
            if p.compiled.is_none() {
                p.compiled = compiled.get(&p.id).cloned();
            }
        }
    }
    attach_variants(reader, window_minutes, queries).await
}

use crate::query_store::variants_sql::variants_sql;

/// SQL Server 2022 keeps one "variant" query per range of parameter values (with its own plans) under the statement
/// the application sent. Older versions don't have the view; that is not an error.
async fn attach_variants<R: crate::read::NativeRead>(
    reader: &mut crate::query_store::Reader<R>,
    window_minutes: u32,
    queries: &mut [QueryStat],
) -> culprit_core::Result<()> {
    let ids: Vec<String> = queries.iter().map(|q| q.id.clone()).collect();
    let list = id_list(&ids);
    if list == "NULL" {
        return Ok(());
    }
    let rows = match reader.window_rows(&variants_sql(&list), window_minutes).await {
        Ok(rows) => rows,
        Err(error) if crate::read::stops_remaining_reads(&error) => return Err(error),
        Err(_) => return Ok(()),
    };
    for r in rows {
        let Some(parent) = queries.iter_mut().find(|q| q.id == r.string("parent_query_id")) else { continue };
        let executions = r.f64_or_zero("executions");
        let total = r.f64_or_zero("total_ms");
        parent.plans.push(PlanVariant {
            id: r.string("plan_id"),
            executions,
            total_time_ms: total,
            mean_time_ms: if executions > 0.0 { total / executions } else { 0.0 },
            first_seen: r.str("first_seen"),
            last_seen: r.str("last_seen"),
            is_forced: r.bool("is_forced"),
            min_time_ms: r.f64("min_ms"),
            max_time_ms: r.f64("max_ms"),
            stddev_time_ms: None,
            compiled: None,
            variant_query: Some(r.string("variant_id")),
            cpu_time_ms: None,
            logical_reads: None,
        });
    }
    Ok(())
}
