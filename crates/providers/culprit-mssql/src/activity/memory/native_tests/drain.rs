//! The oracle is a positive observation of the unchanged waiter's grant, never a disappeared row or accepted drain.
use super::{
    FIXTURE_ROWS,
    fixture::Fixture,
    reservations::{self, HolderRequirement},
};
use crate::{read::invalid_data, test_support::save_cost_receipt};
use culprit_core::{Result, telemetry::*};

pub(super) async fn prove(fixture: &mut Fixture) -> Result<()> {
    let pair = reservations::establish(fixture, HolderRequirement::ClientOutputWait).await?;
    if !fixture.can_observe() {
        return Err(fixture.observation_stop());
    }
    save_cost_receipt(
        &fixture.directory,
        "drain-before",
        &serde_json::json!({
            "point": pair.point.snapshot(),
            "original_holder": pair.holder,
            "original_waiter": pair.waiter,
            "waiter_actor_index": pair.waiter_index,
            "intervention": "only the original holder's unchanged result stream will be drained",
        }),
    )?;
    let holder_rows = fixture.drain_original_holder().await?;
    if holder_rows != FIXTURE_ROWS {
        return Err(invalid_data());
    }
    while fixture.can_observe() {
        fixture.pause().await?;
        let point = fixture.observe().await?;
        if !point.context.matches_earlier_scope(&pair.point.context) {
            return Err(CollectionStop::SourceChanged.error());
        }
        let Some(waiter) = point.actor(fixture.session(pair.waiter_index)?)? else { continue };
        if !same_original_waiter(&pair.waiter, waiter) {
            return Err(CollectionStop::SourceChanged.error());
        }
        if waiter.state != ExecutionGrantState::Granted {
            continue;
        }
        if !waiter.granted_kibibytes.as_deref().is_some_and(|value| value.parse::<u64>().is_ok_and(|amount| amount > 0))
            || waiter.granted_without_offset.is_none()
        {
            return Err(invalid_data());
        }
        return save_cost_receipt(
            &fixture.directory,
            "drain-acceptance",
            &serde_json::json!({
                "original_point": pair.point.snapshot(),
                "later_point": point.snapshot(),
                "original_holder": pair.holder,
                "original_waiter": pair.waiter,
                "same_waiter_granted": waiter,
                "holder_rows_drained_and_native_stream_completed": holder_rows,
                "intervention_count": 1,
                "waiter_sql_or_input_replaced": false,
                "disappearance_used_as_recovery": false,
                "causal_scope": "owned comparable-load control; production observations alone do not establish an exclusive cause",
                "native_cost_approval": false,
            }),
        );
    }
    Err(fixture.observation_stop())
}

fn same_original_waiter(before: &ExecutionGrantObservation, after: &ExecutionGrantObservation) -> bool {
    let (Some(original), Some(current)) = (&before.request, &after.request) else { return false };
    before.is_valid()
        && after.is_valid()
        && before.source == after.source
        && before.identity == after.identity
        && before.requested_without_offset == after.requested_without_offset
        && before.resource.is_some()
        && before.resource == after.resource
        && before.scope == after.scope
        && before.request_association == RequestGrantAssociation::GenerationChecked
        && after.request_association == RequestGrantAssociation::GenerationChecked
        && original.key == current.key
        && original.statement_identity.is_some()
        && original.statement_identity == current.statement_identity
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn original() -> ExecutionGrantObservation {
        let mut reader = crate::activity::memory::tests::successful();
        let captured = reader.capture_memory().await.unwrap();
        assert_eq!(reader.close().await, SourceShutdown::Quiescent);
        captured.observations.into_iter().next().unwrap()
    }

    #[tokio::test]
    async fn a_positive_transition_keeps_original_native_request_and_grant_time_instead_of_a_reused_session_number() {
        let granted = original().await;
        let mut waiting = granted.clone();
        waiting.state = ExecutionGrantState::Waiting;
        waiting.granted_without_offset = None;
        waiting.granted_kibibytes = Some("0".into());
        assert!(waiting.is_valid() && granted.is_valid());
        assert!(same_original_waiter(&waiting, &granted));
        #[derive(Debug)]
        enum Change {
            SessionGeneration,
            RequestGeneration,
            GrantTime,
            Resource,
            Statement,
        }
        for change in [
            Change::SessionGeneration,
            Change::RequestGeneration,
            Change::GrantTime,
            Change::Resource,
            Change::Statement,
        ] {
            let mut changed = granted.clone();
            match change {
                Change::SessionGeneration => {
                    changed.request.as_mut().unwrap().key.session.generation = "2020-01-01T07:57:01".into()
                }
                Change::RequestGeneration => {
                    changed.request.as_mut().unwrap().key.request.as_mut().unwrap().generation =
                        "2020-01-01T07:58:00.003".into()
                }
                Change::GrantTime => {
                    changed.requested_without_offset = "2020-01-01T07:58:01.001".into();
                    changed
                        .identity
                        .qualifiers
                        .insert("requested_without_offset".into(), changed.requested_without_offset.clone());
                }
                Change::Resource => changed.resource.as_mut().unwrap().id = "3".into(),
                Change::Statement => {
                    changed.request.as_mut().unwrap().statement_identity.as_mut().unwrap().id.push('1')
                }
            }
            assert!(changed.is_valid(), "{change:?} remains independently valid native-shaped evidence");
            assert!(!same_original_waiter(&waiting, &changed), "{change:?}");
        }
        let mut missing = granted;
        missing.request = None;
        assert!(!same_original_waiter(&waiting, &missing), "a missing request is not a positive transition");
    }
}
