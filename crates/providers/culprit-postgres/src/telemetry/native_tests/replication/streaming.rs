use super::support::*;
use culprit_core::{ReplicaRole, telemetry::*};
use futures_util::FutureExt;
use std::{collections::BTreeMap, time::Duration};

fn counter(sample: &NativeReplicationSample, key: &str) -> f64 {
    match &sample.measurements.counters.iter().find(|counter| counter.key == key).unwrap().value {
        MeasurementValue::Real(value) => *value,
        MeasurementValue::Integer(value) => value.parse().unwrap(),
    }
}

fn sender_for<'a>(snapshot: &'a ReplicationSnapshot, application: &str) -> &'a NativeReplicationSample {
    let mut matches = snapshot
        .observations
        .iter()
        .filter(|row| row.source == "pg_stat_replication")
        .map(native_sample)
        .filter(|sample| sample.measurements.properties.get("name").map(String::as_str) == Some(application));
    let sample = matches.next().expect("the owned streaming sender must be visible");
    assert!(matches.next().is_none(), "one exact owned standby application");
    sample
}

#[tokio::test]
#[ignore = "requires the explicit nonce-owned PostgreSQL primary/standby topology"]
async fn real_streaming_sender_receiver_and_slot_share_native_identity_across_scan_and_capture() {
    run_case(|fixture| {
        async move {
            let _barrier = fixture.create_role("streaming", true).await;
            let primary_spec = fixture.primary_spec.clone();
            let standby_spec = fixture.standby_spec.clone();
            let (primary, _, visible) = fixture.scan(&primary_spec).await;
            assert!(visible);
            let sender = sender_for(&primary, &fixture.application);
            assert_eq!(sender.measurements.properties["state"], "streaming");
            assert!(sender.session.is_some());
            for position in ["current_lsn", "sent_lsn", "write_lsn", "flush_lsn", "replay_lsn"] {
                assert!(sender.measurements.properties.contains_key(position), "{position}");
            }
            let slot_name = sender.slot_name.as_ref().expect("the owned standby must use its physical slot");
            let slot = primary
                .observations
                .iter()
                .filter(|row| row.source == "pg_replication_slots")
                .map(native_sample)
                .find(|sample| sample.slot_name.as_ref() == Some(slot_name))
                .unwrap();
            assert_eq!(slot.measurements.properties["slot_type"], "physical");
            assert_eq!(slot.measurements.properties["active"], "true");
            assert_eq!(slot.session, sender.session, "link only the actually observed PID plus backend generation");
            assert!(
                !slot.measurements.properties.contains_key("confirmed_flush_lsn"),
                "physical slots do not invent logical confirmation"
            );
            let columns = fixture.slot_columns().await;
            let availability: BTreeMap<String, bool> =
                serde_json::from_str(&slot.measurements.properties["column_availability"]).unwrap();
            for (column, available) in &availability {
                assert_eq!(*available, columns.contains(column), "native column {column}");
                if !available {
                    assert!(!slot.measurements.properties.contains_key(column));
                    assert!(slot.measurements.counters.iter().all(|counter| counter.key != *column));
                }
            }
            assert_eq!(availability["two_phase_at"], fixture.version >= 180_000);
            assert_eq!(availability["synced"], fixture.version >= 170_000);

            let (standby, _, visible) = fixture.scan(&standby_spec).await;
            assert!(visible);
            let receiver = point(&standby.observations, "pg_recovery");
            assert_eq!(receiver.measurements.properties["receiver"], "streaming");
            assert_eq!(receiver.slot_name.as_ref(), Some(slot_name));
            assert!(receiver.session.is_none());
            assert!(
                receiver.unresolved_session_id.is_some(),
                "the receiver view supplies a PID without backend generation"
            );
            for field in [
                "receive_lsn",
                "replay_lsn",
                "written_lsn",
                "flushed_lsn",
                "last_msg_send_time",
                "last_msg_receipt_time",
                "latest_end_lsn",
                "latest_end_time",
            ] {
                assert!(receiver.measurements.properties.contains_key(field), "real receiver field {field}");
            }
            assert!(
                !receiver.measurements.properties.contains_key("receiver_received_lsn"),
                "the modern view does not fabricate the older combined column"
            );

            let (manifest, records) = fixture.capture(&primary_spec).await;
            let captured =
                ReplicationSnapshot::from_source(manifest.description.as_ref().unwrap(), rows(&records)).unwrap();
            let captured_sender = sender_for(&captured, &fixture.application);
            assert_eq!(captured_sender.key, sender.key);
            assert_eq!(captured_sender.session, sender.session);
            assert!(captured.observations.iter().any(|row| row.fact.key() == slot.key));
            let (manifest, records) = fixture.capture(&standby_spec).await;
            let captured =
                ReplicationSnapshot::from_source(manifest.description.as_ref().unwrap(), rows(&records)).unwrap();
            assert_eq!(point(&captured.observations, "pg_recovery").key, receiver.key);
            for sample in captured.observations.iter().map(native_sample) {
                assert!(
                    sample
                        .measurements
                        .counters
                        .iter()
                        .all(|counter| counter.reset == culprit_core::ResetMarker::Unknown)
                );
            }
            assert!(crate::telemetry_recipes().iter().any(
                |recipe| recipe.id == "postgres.replication" && recipe.cost == RecipeCostQualification::Unqualified
            ));
        }
        .boxed_local()
    })
    .await;
}

#[tokio::test]
#[ignore = "requires the explicit nonce-owned PostgreSQL primary/standby topology"]
async fn an_idle_replayed_commit_gets_older_without_becoming_a_replay_latency_measurement() {
    run_case(|fixture| {
        async move {
            let _barrier = fixture.create_role("idle", true).await;
            let standby = fixture.standby_spec.clone();
            let (before, replicas, _) = fixture.scan(&standby).await;
            let first = point(&before.observations, "pg_recovery");
            let commit = first.measurements.properties["last_replay_commit_at"].clone();
            let age = counter(first, "since_replay");
            assert_eq!(
                replicas
                    .iter()
                    .find(|replica| replica.role == ReplicaRole::ThisServer)
                    .expect("native standby projection")
                    .lag_seconds,
                None
            );
            tokio::time::sleep(Duration::from_millis(1_200)).await;
            let (after, replicas, _) = fixture.scan(&standby).await;
            let second = point(&after.observations, "pg_recovery");
            assert_eq!(second.measurements.properties["last_replay_commit_at"], commit);
            assert!(
                counter(second, "since_replay") > age + 0.5,
                "a real idle commit age must advance independently of replay latency"
            );
            assert_eq!(
                replicas
                    .iter()
                    .find(|replica| replica.role == ReplicaRole::ThisServer)
                    .expect("native standby projection")
                    .lag_seconds,
                None
            );
        }
        .boxed_local()
    })
    .await;
}
