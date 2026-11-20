//! One same-original request selector for every task-space presentation. Context is never allocation ownership.
use culprit_core::{
    TaskSpaceCurrentRequest,
    telemetry::{DefinitionPayload, NativeActivityIdentity, TaskRequestContext, TaskSpaceObservation},
};
use std::collections::BTreeSet;

pub(super) fn qualified(
    row: &TaskSpaceObservation,
    refused: &BTreeSet<NativeActivityIdentity>,
) -> Option<TaskSpaceCurrentRequest> {
    if row.request_context() != TaskRequestContext::CheckedAtBounds {
        return None;
    }
    let request = row.request.as_ref()?;
    if !request.has_session_generation_authority() || refused.contains(&request.key.session) {
        return None;
    }
    let statement = match &request.statement {
        DefinitionPayload::Available { text, .. } => {
            // The native TaskSpace contract is SQL Server's guarded task/request projection. Both original
            // and checked locators must agree before this available statement can become shared context.
            let original = request.statement_identity.as_ref()?;
            if row.statement_identity.as_ref() != Some(original) {
                return None;
            }
            DefinitionPayload::sql(text, culprit_core::Engine::Mssql)
        }
        unavailable @ DefinitionPayload::Unavailable { .. } => unavailable.clone(),
    };
    Some(TaskSpaceCurrentRequest {
        request: request.key.clone(),
        observed_from: row.before_check.as_ref()?.observed_at.clone(),
        observed_until: row.after_check.as_ref()?.observed_at.clone(),
        statement,
        client: request.client.clone(),
    })
}
