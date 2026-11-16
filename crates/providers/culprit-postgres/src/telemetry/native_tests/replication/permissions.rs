use super::support::*;
use culprit_core::{ReplicaCondition, ReplicaRole, SourceState, telemetry::*};
use futures_util::FutureExt;
use std::collections::BTreeMap;

#[tokio::test]
#[ignore = "requires the explicit nonce-owned PostgreSQL primary/standby topology"]
async fn hidden_receiver_details_keep_the_real_pid_and_do_not_become_a_stopped_or_empty_source() {
    run_case(|fixture| {
        async move {
            let limited = fixture.create_role("hidden", false).await;
            let (snapshot, replicas, visible) = fixture.scan(&limited).await;
            assert!(!visible);
            assert!(
                snapshot.sources.iter().all(|source| source.state == SourceState::Partial && !source.complete_scope)
            );
            let receiver = point(&snapshot.observations, "pg_recovery");
            assert_eq!(receiver.measurements.properties["full_visibility"], "false");
            assert_eq!(receiver.measurements.properties["streaming_configured"], "true");
            assert!(!receiver.measurements.properties.contains_key("receiver"));
            let pid =
                receiver.measurements.properties.get("active_pid").expect("native masking retains a live receiver PID");
            assert_eq!(receiver.unresolved_session_id.as_ref(), Some(pid));
            assert!(receiver.session.is_none());
            let replica = replicas
                .iter()
                .find(|replica| replica.role == ReplicaRole::ThisServer)
                .expect("native standby projection");
            assert_eq!(replica.condition, ReplicaCondition::Reported);
            assert!(!replica.condition.is_stopped());
            assert_eq!(replica.lag_seconds, None);
            fixture.denied_capture(&limited).await;
        }
        .boxed_local()
    })
    .await;
}

#[tokio::test]
#[ignore = "requires the explicit nonce-owned PostgreSQL primary/standby topology"]
async fn denied_optional_pause_functions_preserve_receiver_facts_and_explicit_unknown_values() {
    run_case(|fixture| {
        async move {
            let limited = fixture.create_role("pause", true).await;
            fixture.deny_pause_functions().await;
            let (snapshot, _, visible) = fixture.scan(&limited).await;
            assert!(visible);
            let receiver = point(&snapshot.observations, "pg_recovery");
            assert_eq!(receiver.measurements.properties["receiver"], "streaming");
            let access: BTreeMap<String, bool> =
                serde_json::from_str(&receiver.measurements.properties["replay_pause_access"]).unwrap();
            assert_eq!(access, BTreeMap::from([("requested".into(), false), ("state".into(), false)]));
            for field in ["replay_pause_requested", "replay_pause_state", "replay_paused"] {
                assert!(!receiver.measurements.properties.contains_key(field), "denied is not false: {field}");
            }
            let availability: BTreeMap<String, bool> =
                serde_json::from_str(&receiver.measurements.properties["column_availability"]).unwrap();
            assert!(availability["replay_pause_state"], "supported and denied are separate native facts");
            let (manifest, records) = fixture.capture(&limited).await;
            let captured =
                ReplicationSnapshot::from_source(manifest.description.as_ref().unwrap(), rows(&records)).unwrap();
            let receiver = point(&captured.observations, "pg_recovery");
            assert_eq!(receiver.measurements.properties["receiver"], "streaming");
            let captured_access: BTreeMap<String, bool> =
                serde_json::from_str(&receiver.measurements.properties["replay_pause_access"]).unwrap();
            assert_eq!(captured_access, access);
            assert!(
                captured
                    .sources
                    .iter()
                    .all(|source| matches!(source.state, SourceState::Collected | SourceState::Empty))
            );
        }
        .boxed_local()
    })
    .await;
}
