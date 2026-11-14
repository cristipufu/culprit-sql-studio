//! Parameter compilation context and native variant relationships retain their original query/plan lifetimes.
use std::collections::HashMap;

use culprit_core::{Coverage, QueryStat, Result, telemetry::NativePlanIdentity};

use crate::{
    plan_xml,
    query_store::{
        Reader,
        definitions::{Definition, Kind},
    },
    read::NativeRead,
};

/// Supplementary plans are bounded independently of the number of runtime rows in the selected workload.
const MAX_QUERIES: usize = 20;
const MAX_PLANS: usize = 60;
const MAX_PLAN_BYTES: usize = 2 * 1024 * 1024;

pub(crate) async fn attach<R: NativeRead>(
    reader: &mut Reader<R>,
    queries: &mut [QueryStat],
    plan_identities: &HashMap<String, NativePlanIdentity>,
    coverage: &mut Coverage,
) -> Result<()> {
    attach_primary_parameters(queries);
    attach_other_parameters(reader, queries, plan_identities, coverage).await
}

fn attach_primary_parameters(queries: &mut [QueryStat]) {
    for query in queries {
        let compiled = query.plan.as_ref().and_then(|plan| plan.facts().compiled_parameters.clone());
        if let Some(plan) = query.plans.iter_mut().find(|plan| query.plan_id.as_ref() == Some(&plan.id)) {
            plan.compiled = compiled;
        }
    }
}

async fn attach_other_parameters<R: NativeRead>(
    reader: &mut Reader<R>,
    queries: &mut [QueryStat],
    identities: &HashMap<String, NativePlanIdentity>,
    coverage: &mut Coverage,
) -> Result<()> {
    let wanted = queries
        .iter()
        .filter(|query| query.plans.len() >= 2)
        .take(MAX_QUERIES)
        .flat_map(|query| {
            query
                .plans
                .iter()
                .filter(|plan| plan.compiled.is_none() && query.plan_id.as_ref() != Some(&plan.id))
                .map(|plan| plan.id.clone())
        })
        .take(MAX_PLANS)
        .collect::<Vec<_>>();
    if wanted.is_empty() {
        return Ok(());
    }
    let definitions = match reader.definitions(&wanted, Kind::Plan, MAX_PLAN_BYTES).await {
        Ok(definitions) => definitions,
        Err(error) if crate::read::stops_remaining_reads(&error) => return Err(error),
        Err(error) => {
            coverage.partial(&crate::sources::QUERY_STORE, error.message_source());
            return Ok(());
        }
    };
    if definitions.iter().any(|definition| !same_plan(definition, identities)) {
        return Err(reader.source_changed());
    }
    let compiled = definitions
        .into_iter()
        .filter_map(|definition| {
            let plan = plan_xml::parse(definition.payload.text()?).ok()?;
            Some((definition.id, plan.facts().compiled_parameters.clone()?))
        })
        .collect::<HashMap<_, _>>();
    for plan in queries.iter_mut().flat_map(|query| &mut query.plans) {
        if plan.compiled.is_none() {
            plan.compiled = compiled.get(&plan.id).cloned();
        }
    }
    Ok(())
}

fn same_plan(definition: &Definition, identities: &HashMap<String, NativePlanIdentity>) -> bool {
    identities.get(&definition.id).is_some_and(|identity| {
        definition.query.as_ref() == Some(&identity.query) && definition.plan.as_ref() == Some(identity)
    })
}
