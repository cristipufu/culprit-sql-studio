use super::fixture::Fixture;
use culprit_collector::analytical::{
    evidence::EvidenceFamily,
    test_support::{self, GroupReplicationCase},
};
use culprit_core::telemetry::*;
#[tokio::test]
async fn group_replication_native_member_roles_queues_and_unknown_totals_replay_one_canonical_report() {
    for case in GroupReplicationCase::ALL {
        let (source, records) = test_support::group_replication_evidence(case);
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let before = fixture.recovery_object_reads();
        let capture = fixture.read_families(&[EvidenceFamily::OperationalSamples]).await;
        test_support::assert_operational_capture(&capture, &source, &records);
        let analysis = culprit_engine::collected_resources::group_replication(&capture).unwrap();
        let rows = records
            .iter()
            .map(|record| {
                let TelemetryRecord::Operational(row) = record else { panic!() };
                row
            })
            .collect::<Vec<_>>();
        let expected =
            culprit_checks::group_replication::explain(culprit_checks::group_replication::GroupReplicationEvidence {
                source: &source,
                observations: &rows,
                read_complete: true,
            })
            .unwrap();
        assert_eq!(serde_json::to_value(&analysis.report).unwrap(), serde_json::to_value(expected).unwrap());
        assert_eq!(analysis.context.source, source);
        assert_eq!(fixture.recovery_object_reads(), before);
        assert_eq!(analysis.report.observed_rows, records.len() as u32);
        assert_eq!(analysis.report.original.as_ref().unwrap().context.database_writable, None);
    }
}
#[tokio::test]
async fn group_replication_raw_original_rejects_foreign_group_member_scope_clock_and_async_relabelling() {
    let (source, records) = test_support::group_replication_evidence(GroupReplicationCase::Queued);
    for change in ["group", "member", "clock", "dataset", "generic"] {
        let mut altered = source.clone();
        if change == "member" {
            let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut altered.partition else { panic!() };
            member.native.id = "foreign".into();
        } else {
            let SourceObservation::GroupReplication { metadata, snapshot } = &mut altered.observation else { panic!() };
            match change {
                "group" => metadata.group_id = Some("foreign-group".into()),
                "clock" => snapshot.observed_until = snapshot.observed_from.clone(),
                "dataset" => snapshot.dataset = SnapshotDataset::Replication,
                "generic" => altered.observation = SourceObservation::Snapshot(snapshot.clone()),
                _ => unreachable!(),
            }
        }
        assert!(
            records.iter().all(|row| ExportedRecord::new(
                &altered,
                &CaptureIdentity { observation_id: "original".into(), revision: None },
                row.clone()
            )
            .is_err()),
            "{change}"
        );
    }
}
