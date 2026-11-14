use super::*;
use crate::{
    query_store::tuning::{
        Kind, Layout,
        native::tests::{forced_row, layout_row, recommendation_row},
    },
    resources::test_support::{FakeRead, replace},
    value::Val,
};
use culprit_core::QueryStoreScope;

#[tokio::test]
async fn native_tuning_points_keep_unknown_plan_scopes_partial_including_empty_reads() {
    for partitioned in [false, true] {
        let mut metadata = FakeRead::new(vec![Ok(vec![replace(
            &layout_row(),
            &[("runtime_replica", Val::Int(i64::from(partitioned)))],
        )])]);
        let layout = Ok(Layout::read(&mut metadata).await.unwrap());
        assert_eq!(metadata.calls.len(), 1);
        for (kind, row) in [(Kind::Recommendations, recommendation_row()), (Kind::ForcedPlans, forced_row())] {
            for rows in [vec![row], Vec::new()] {
                let count = rows.len();
                let mut reader = FakeRead::new(vec![Ok(rows)]);
                let ReadOutcome::Observed(records) =
                    Family::Tuning(kind).read(&mut reader, 10, Some(&layout)).await.unwrap()
                else {
                    panic!("supported native source")
                };
                assert_eq!(records.values.len(), count);
                assert_eq!(
                    records.complete_scope, !partitioned,
                    "an empty unqualified read does not prove attribution absence"
                );
                for record in records.values {
                    let TelemetryRecord::Tuning(observation) = record else { panic!("tuning fact") };
                    let scope = match &observation.fact {
                        TuningFact::Recommendation(value) => &value.plan_scope,
                        TuningFact::ForcedPlan(value) => &value.plan_scope,
                        _ => panic!("native plan fact"),
                    };
                    assert_eq!(*scope, if partitioned { QueryStoreScope::Unknown } else { QueryStoreScope::Local });
                    assert_eq!(observation.fact.attribution_is_complete(), !partitioned);
                    assert!(observation.is_valid(), "positive facts stay usable as independent native context");
                }
                assert_eq!(reader.calls.len(), 1, "scope qualification never adds native queries");
            }
        }
    }
}
