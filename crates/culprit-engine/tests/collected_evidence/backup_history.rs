use super::*;
use culprit_collector::analytical::test_support::{
    BackupHistoryCase, assert_backup_history_capture, backup_history_evidence,
};
use culprit_core::SourceState;
#[tokio::test]
async fn original_backup_profiles_keep_native_fields_retention_gaps_and_one_canonical_interpretation_without_recovery()
{
    for case in BackupHistoryCase::ALL {
        let (source, rows) = backup_history_evidence(case);
        let fixture = Fixture::new(source.clone(), rows.clone()).await;
        let opened = fixture.recovery_object_reads();
        let capture =
            fixture.read_families(&[culprit_collector::analytical::evidence::EvidenceFamily::OperationalSamples]).await;
        assert_backup_history_capture(&capture, &source, &rows);
        let analysis = culprit_engine::collected_resources::backup_history(&capture).unwrap();
        let observations = rows
            .iter()
            .map(|row| match row {
                TelemetryRecord::Operational(row) => row,
                _ => unreachable!(),
            })
            .collect::<Vec<_>>();
        let canonical =
            culprit_checks::backup_history::explain(culprit_checks::backup_history::BackupHistoryEvidence {
                source: &source,
                observations: &observations,
                read_complete: true,
            })
            .unwrap();
        assert_eq!(*analysis.report, canonical, "{case:?}");
        assert_eq!(analysis.context.source, source);
        assert_eq!(analysis.context.records.len(), rows.len());
        assert_eq!(fixture.recovery_object_reads(), opened);
        assert!(!analysis.report.source.complete_scope);
        assert_eq!(
            analysis.report.source_state,
            if matches!(
                case,
                BackupHistoryCase::PermissionDenied
                    | BackupHistoryCase::Hyperscale
                    | BackupHistoryCase::UnknownDeployment
            ) {
                SourceState::Unavailable
            } else {
                SourceState::Partial
            }
        );
        assert_eq!(analysis.report.entries.len(), rows.len());
        if case == BackupHistoryCase::DamagedNative {
            assert_eq!(analysis.report.entries[0].explanation, culprit_core::t!("view.backup_history.entry.damaged"));
        }
        if case == BackupHistoryCase::NullableNative {
            let MssqlBackupRecord::Msdb(row) = &analysis.report.entries[0].native else { unreachable!() };
            assert_eq!((row.backup_type, row.time_zone_quarter_hours), (None, None));
            assert_eq!(row.initiator_user_name, None);
        }
    }
}
#[tokio::test]
async fn backups_refuse_coherent_foreign_vendor_or_altered_original_scope_before_export() {
    for change in ["vendor", "dataset", "scope", "member", "bounds"] {
        let (mut source, rows) = backup_history_evidence(BackupHistoryCase::MsdbNative);
        match change {
            "vendor" => {
                let CollectionTarget::Database(database) = &mut source.target else { unreachable!() };
                database.engine = culprit_core::Engine::Postgres;
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    unreachable!()
                };
                member.engine = culprit_core::Engine::Postgres;
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    unreachable!()
                };
                member.native.id = "other-member".into();
            }
            _ => {
                let SourceObservation::BackupHistory { snapshot, scope, .. } = &mut source.observation else {
                    unreachable!()
                };
                match change {
                    "dataset" => snapshot.dataset = SnapshotDataset::Cdc,
                    "scope" => scope.database_id = 8,
                    _ => snapshot.observed_until = "2020-01-01T00:00:02Z".into(),
                }
            }
        }
        assert!(
            rows.iter().all(|row| ExportedRecord::new(
                &source,
                &CaptureIdentity { observation_id: "original".into(), revision: None },
                row.clone()
            )
            .is_err()),
            "{change}"
        );
    }
}
