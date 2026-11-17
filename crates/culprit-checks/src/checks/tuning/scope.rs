//! A tuning fact may join runtime statistics only within the same positively identified native population.
use culprit_core::{QueryStoreScope, Snapshot, Workload, WorkloadKind, engines::QueryStoreReadMode};

pub(crate) fn plan_workload(snapshot: &Snapshot) -> Option<&Workload> {
    let scope = &snapshot.tuning.as_ref()?.plan_scope;
    let workload = query_store_workload(snapshot)?;
    let catalog = workload.population_catalog.as_ref()?;
    if !scope.is_valid() {
        return None;
    }
    let matches = match scope {
        QueryStoreScope::Local => !catalog.partitioned && workload.population.is_none(),
        QueryStoreScope::ReplicaGroup { id } => {
            let identity = culprit_core::telemetry::NativeIdentity::new(id);
            catalog.partitioned && workload.population.as_ref() == Some(&identity) && catalog.contains(&identity)
        }
        QueryStoreScope::Unknown => false,
    };
    matches.then_some(workload)
}

fn query_store_workload(snapshot: &Snapshot) -> Option<&Workload> {
    let workload = snapshot.workload.as_ref()?;
    let catalog = workload.population_catalog.as_ref()?;
    (workload.kind() == Some(WorkloadKind::QueryStore)
        && catalog.source == culprit_core::source_id::QUERY_STORE
        && catalog.is_valid())
    .then_some(workload)
}

/// A database-wide index recommendation cannot borrow one replica group's observed query costs.
pub(crate) fn unpartitioned_workload(snapshot: &Snapshot) -> Option<&Workload> {
    let workload = query_store_workload(snapshot)?;
    (!workload.population_catalog.as_ref()?.partitioned && workload.population.is_none()).then_some(workload)
}

fn management_scope(snapshot: &Snapshot) -> Option<&QueryStoreScope> {
    let tuning = snapshot.tuning.as_ref()?;
    let workload = plan_workload(snapshot)?;
    if !workload.population_catalog.as_ref()?.has_complete_membership() {
        return None;
    }
    let mode = snapshot
        .setting("query_store_options.actual_state_desc")
        .and_then(|setting| QueryStoreReadMode::from_native(&setting.value));
    (mode == Some(QueryStoreReadMode::ReadWrite)).then_some(&tuning.plan_scope)
}

pub(crate) fn can_change_plan(snapshot: &Snapshot) -> bool {
    management_scope(snapshot).is_some()
}

#[derive(Clone, Copy)]
pub(super) enum PlanCommand {
    Force,
    Unforce,
}

pub(super) fn plan_command(snapshot: &Snapshot, command: PlanCommand, query: &str, plan: &str) -> Option<String> {
    let scope = management_scope(snapshot)?;
    let valid_id = |id: &str| id.parse::<u64>().is_ok_and(|value| value.to_string() == id);
    if !valid_id(query) || !valid_id(plan) {
        return None;
    }
    let procedure = match command {
        PlanCommand::Force => "sp_query_store_force_plan",
        PlanCommand::Unforce => "sp_query_store_unforce_plan",
    };
    let group = match scope {
        QueryStoreScope::Local => String::new(),
        QueryStoreScope::ReplicaGroup { id } => format!(", @replica_group_id = {id}"),
        QueryStoreScope::Unknown => return None,
    };
    Some(format!("EXEC {procedure} @query_id = {query}, @plan_id = {plan}{group};"))
}

#[cfg(test)]
mod tests;
