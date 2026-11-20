use super::*;
use culprit_collector::analytical::test_support::{
    CleanupCase, assert_cleanup_capture, cleanup_evidence, cleanup_evidence_cases,
};
use culprit_core::{
    CleanupMechanism, CleanupReport, SourceState, collected_resource_analysis::CollectedCleanupAnalysis, source_id,
};
use culprit_engine::collected_resources;

async fn analyze_case(case: CleanupCase) -> CollectedCleanupAnalysis {
    let (source, records) = cleanup_evidence(case);
    let fixture = Fixture::new(source.clone(), records.clone()).await;
    let capture =
        fixture.read_families(&[culprit_collector::analytical::evidence::EvidenceFamily::OperationalSamples]).await;
    assert_cleanup_capture(&capture, &source, &records);
    let analysis = collected_resources::cleanup(&capture).unwrap();
    assert_eq!(analysis.context.source, source);
    assert_eq!(analysis.context.records.len(), records.len());
    for record in records {
        let expected = ExportedRecord::new(&source, capture.capture(), record).unwrap();
        assert_eq!(analysis.context.records.iter().filter(|row| row.row == expected).count(), 1);
    }
    analysis
}

fn boundary(report: &CleanupReport, horizon: CleanupHorizon) -> &culprit_core::CleanupBoundary {
    report.boundaries.iter().find(|boundary| boundary.horizon == horizon).unwrap()
}

#[tokio::test]
async fn a_read_only_snapshot_is_only_a_retention_candidate_with_zero_unknown_or_positive_version_bytes() {
    for case in [
        CleanupCase::LocalSnapshotZeroVersions,
        CleanupCase::LocalSnapshotUnknownVersions,
        CleanupCase::LocalSnapshotProtector,
    ] {
        let analysis = analyze_case(case).await;
        let [entry] = analysis.report.entries.as_slice() else { panic!("one original snapshot candidate") };
        assert_eq!(entry.mechanisms, vec![CleanupMechanism::ConfirmedDatabaseTransaction]);
        assert!(entry.explanation.contains(&culprit_core::t!("view.cleanup.transaction.context.snapshot_zero_log")));
        assert!(entry.investigation.contains(&culprit_core::t!("view.cleanup.transaction.investigate_snapshot")));
        let source = analysis.context.source.observation.snapshot().unwrap();
        let coverage = source.sources.iter().find(|source| source.source == source_id::VERSION_STORE).unwrap();
        let observed = analysis.context.records.iter().find_map(|row| match &row.row.record {
            TelemetryRecord::Operational(row) if row.source == source_id::VERSION_STORE => match &row.fact {
                OperationalFact::Counter(counter) => Some(counter.value.clone()),
                _ => None,
            },
            _ => None,
        });
        match case {
            CleanupCase::LocalSnapshotZeroVersions => {
                assert_eq!(coverage.state, SourceState::Collected);
                assert_eq!(observed, Some(MeasurementValue::Integer("0".into())));
            }
            CleanupCase::LocalSnapshotUnknownVersions => {
                assert_eq!(coverage.state, SourceState::Unavailable);
                assert_eq!(coverage.error_code, Some(culprit_core::ErrorCode::PermissionDenied));
                assert_eq!(observed, None, "an unavailable native gauge cannot become measured zero");
            }
            CleanupCase::LocalSnapshotProtector => assert_eq!(observed, Some(MeasurementValue::Integer("64".into()))),
            _ => unreachable!(),
        }
        assert!(
            analysis.report.boundaries.is_empty(),
            "a snapshot and database bytes do not establish a native cleanup-distance boundary"
        );
    }
}

#[tokio::test]
async fn local_cleanup_distinguishes_logged_generator_from_zero_log_snapshot_and_keeps_original_bytes_separate() {
    for (case, message) in [
        (CleanupCase::LocalLoggedGenerator, culprit_core::t!("view.cleanup.transaction.context.logged_versioning")),
        (CleanupCase::LocalSnapshotProtector, culprit_core::t!("view.cleanup.transaction.context.snapshot_zero_log")),
    ] {
        let analysis = analyze_case(case).await;
        let entry = analysis
            .report
            .entries
            .iter()
            .find(|entry| entry.reference.source == source_id::OPEN_TRANSACTIONS)
            .unwrap();
        assert!(entry.explanation.contains(&message));
        assert_eq!(analysis.context.source.target.database().unwrap().incarnation, None);
        assert!(analysis.context.source.partition.is_qualified());
        assert!(!analysis.report.entries.iter().any(|entry| entry.reference.source == source_id::VERSION_STORE));
        assert!(analysis.context.records.iter().any(|row| matches!(&row.row.record,
            TelemetryRecord::Operational(row) if row.source == source_id::VERSION_STORE
                && matches!(&row.fact, OperationalFact::Counter(counter)
                    if counter.value == MeasurementValue::Integer("64".into())))));
    }
    let recovered = analyze_case(CleanupCase::LocalRecoveredHorizon).await;
    assert!(recovered.report.entries.iter().all(|entry| entry.mechanisms.is_empty()));
    assert!(
        recovered.context.records.iter().any(|row| matches!(&row.row.record,
        TelemetryRecord::Operational(row) if matches!(&row.fact, OperationalFact::Counter(counter)
            if counter.value == MeasurementValue::Integer("64".into())))),
        "ending the original protection does not prove immediate native byte reclamation"
    );
    let unavailable = analyze_case(CleanupCase::LocalUnavailableHorizon).await;
    assert!(unavailable.report.entries.is_empty());
    assert!(
        unavailable
            .report
            .sources
            .iter()
            .any(|source| source.source == source_id::OPEN_TRANSACTIONS && source.state == SourceState::Unavailable)
    );
}

#[tokio::test]
async fn local_cleanup_intake_refuses_unqualified_members_target_scope_and_unrelated_fact_families() {
    let (original, records) = cleanup_evidence(CleanupCase::LocalLoggedGenerator);
    for partition in [
        SourcePartition::Target,
        SourcePartition::InstanceLocal { member: None, epoch: None },
        match original.partition.clone() {
            SourcePartition::InstanceLocal { member: Some(mut member), epoch } => {
                member.kind = SharedResourceKind::Node;
                SourcePartition::InstanceLocal { member: Some(member), epoch }
            }
            _ => unreachable!(),
        },
    ] {
        let mut source = original.clone();
        source.partition = partition;
        if !source.partition.is_qualified() {
            let SourceObservation::Snapshot(snapshot) = &mut source.observation else { unreachable!() };
            for coverage in &mut snapshot.sources {
                if matches!(coverage.state, SourceState::Collected | SourceState::Empty) {
                    *coverage = StateSourceCoverage::observed(coverage.source.clone(), coverage.records, false);
                }
            }
        }
        let fixture = Fixture::new(source, records.clone()).await;
        let capture =
            fixture.read_families(&[culprit_collector::analytical::evidence::EvidenceFamily::OperationalSamples]).await;
        assert!(collected_resources::cleanup(&capture).is_err());
    }
    for engine in [culprit_core::Engine::Postgres, culprit_core::Engine::Mysql] {
        let mut source = original.clone();
        let CollectionTarget::Database(database) = &mut source.target else { unreachable!() };
        database.engine = engine;
        let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else { unreachable!() };
        member.engine = engine;
        assert!(source.partition_is_consistent());
        assert!(
            NativeReadContext::from_snapshot(&source).unwrap().has_qualified_local_owner(),
            "qualified local ownership is vendor-neutral"
        );
        let fixture = Fixture::new(source, records.clone()).await;
        let capture =
            fixture.read_families(&[culprit_collector::analytical::evidence::EvidenceFamily::OperationalSamples]).await;
        assert!(
            collected_resources::cleanup(&capture).is_err(),
            "{engine:?} cannot supply the SQL Server native cleanup interpretation"
        );
    }
    let mut duplicate_rows = records.clone();
    let mut duplicate = duplicate_rows
        .iter()
        .find(|row| {
            matches!(row,
        TelemetryRecord::Operational(row) if row.source == source_id::OPEN_TRANSACTIONS)
        })
        .unwrap()
        .clone();
    let TelemetryRecord::Operational(row) = &mut duplicate else { unreachable!() };
    row.key = "second-horizon-row".into();
    duplicate_rows.push(duplicate);
    let mut duplicate_source = original.clone();
    let SourceObservation::Snapshot(snapshot) = &mut duplicate_source.observation else { unreachable!() };
    snapshot.records = duplicate_rows.len() as u32;
    snapshot.sources.iter_mut().find(|coverage| coverage.source == source_id::OPEN_TRANSACTIONS).unwrap().records = 2;
    let fixture = Fixture::new(duplicate_source, duplicate_rows).await;
    let capture =
        fixture.read_families(&[culprit_collector::analytical::evidence::EvidenceFamily::OperationalSamples]).await;
    assert!(collected_resources::cleanup(&capture).is_err(), "one local native horizon source is a singleton point");
    for mutation in ["source", "scope", "unit", "native-name"] {
        let mut rows = records.clone();
        let TelemetryRecord::Operational(row) = rows
            .iter_mut()
            .find(|row| {
                matches!(row,
            TelemetryRecord::Operational(row) if row.source == source_id::VERSION_STORE)
            })
            .unwrap()
        else {
            unreachable!()
        };
        let OperationalFact::Counter(counter) = &mut row.fact else { unreachable!() };
        let mut source = original.clone();
        match mutation {
            "source" => {
                row.source = "foreign-cleanup-source".into();
                let SourceObservation::Snapshot(snapshot) = &mut source.observation else { unreachable!() };
                snapshot.sources.iter_mut().find(|source| source.source == source_id::VERSION_STORE).unwrap().source =
                    row.source.clone();
            }
            "scope" => counter.scope = culprit_core::MeasurementScope::Server,
            "unit" => counter.unit = CounterUnit::Bytes,
            "native-name" => counter.native_name = "different_native_counter".into(),
            _ => unreachable!(),
        }
        assert!(rows.iter().all(TelemetryRecord::is_valid));
        let fixture = Fixture::new(source, rows).await;
        let capture =
            fixture.read_families(&[culprit_collector::analytical::evidence::EvidenceFamily::OperationalSamples]).await;
        assert!(collected_resources::cleanup(&capture).is_err(), "{mutation}");
    }
}

#[tokio::test]
async fn every_cleanup_population_uses_original_rows_sources_and_the_existing_pure_view_without_native_work() {
    for (case, _, _) in cleanup_evidence_cases() {
        let analysis = analyze_case(case).await;
        for entry in &analysis.report.entries {
            let row = analysis
                .context
                .records
                .iter()
                .find_map(|row| match &row.row.record {
                    TelemetryRecord::Operational(observation)
                        if observation.source == entry.reference.source
                            && observation.key == entry.reference.observation_key =>
                    {
                        Some(observation)
                    }
                    _ => None,
                })
                .unwrap_or_else(|| panic!("{case:?}: explanation must reference its exact original row"));
            if let Some(identity) = &entry.reference.identity {
                let OperationalFact::Retention(holder) = &row.fact else { panic!("retainer reference") };
                assert_eq!(identity, &holder.identity);
            }
        }
        assert!(!analysis.report.sources.iter().any(|source| source.source == "unrelated-operational"));
        assert!(serde_json::to_value(&analysis.report).unwrap().get("observations").is_none());
    }
}

#[tokio::test]
async fn pvs_precision_opaque_clocks_and_native_skip_reasons_never_turn_into_an_owner_or_wall_clock() {
    let analysis = analyze_case(CleanupCase::PersistentVersions).await;
    let native = analysis
        .context
        .records
        .iter()
        .find_map(|row| match &row.row.record {
            TelemetryRecord::Operational(row) => match &row.fact {
                OperationalFact::MssqlPersistentVersionStore(native) => Some(native),
                _ => None,
            },
            _ => None,
        })
        .unwrap();
    assert_eq!(native.off_row_kibibytes.as_deref(), Some("9007199254740993"));
    assert_eq!(native.minimum_transaction_timestamp.as_deref(), Some("-9223372036854775808"));
    assert_eq!(native.secondary_low_water_mark.as_deref(), Some("9007199254740993"));
    assert_eq!(native.off_row_cleanup.started_without_offset.as_deref(), Some("2020-01-01T13:00:00.1234567"));
    let pvs = analysis
        .report
        .entries
        .iter()
        .find(|entry| entry.reference.source == source_id::PERSISTENT_VERSION_STORE)
        .unwrap();
    assert!(pvs.mechanisms.contains(&CleanupMechanism::PersistentVersionStore));
    assert!(pvs.mechanisms.contains(&CleanupMechanism::OldestActiveTransaction));
    assert!(pvs.mechanisms.contains(&CleanupMechanism::OldestAbortedTransaction));
    assert!(!pvs.mechanisms.contains(&CleanupMechanism::LongSnapshotScan));
    assert!(!pvs.mechanisms.contains(&CleanupMechanism::OnlineIndexRebuild));
    assert!(
        pvs.metrics
            .iter()
            .filter_map(|metric| metric.text.as_deref())
            .any(|text| text.replace(',', "").contains("9007199254740993"))
    );
    assert!(!serde_json::to_string(&analysis.report).unwrap().contains("9007199254740992"));
    // The same native numeric transaction ID appears in the independent horizon. Neither source establishes
    // that this captured participant produced the observed bytes or caused every native cleanup skip.
    assert!(analysis.report.entries.iter().any(|entry| entry.reference.source == source_id::OPEN_TRANSACTIONS));
    assert!(analysis.report.boundaries.is_empty());
}

#[tokio::test]
async fn native_null_missing_columns_and_zero_keep_different_meanings_and_failures_remain_visible() {
    for case in [CleanupCase::NativeNulls, CleanupCase::UnavailableColumns] {
        let analysis = analyze_case(case).await;
        let pvs = analysis
            .report
            .entries
            .iter()
            .find(|entry| entry.reference.source == source_id::PERSISTENT_VERSION_STORE)
            .unwrap();
        assert!(!pvs.mechanisms.iter().any(|mechanism| matches!(
            mechanism,
            CleanupMechanism::OldestActiveTransaction
                | CleanupMechanism::LongSnapshotScan
                | CleanupMechanism::OnlineIndexRebuild
                | CleanupMechanism::OldestAbortedTransaction
        )));
        let native = analysis
            .context
            .records
            .iter()
            .find_map(|row| match &row.row.record {
                TelemetryRecord::Operational(row) => match &row.fact {
                    OperationalFact::MssqlPersistentVersionStore(native) => Some(native),
                    _ => None,
                },
                _ => None,
            })
            .unwrap();
        assert!(native.pages_skipped_for_long_snapshot_scan.is_none());
        assert_eq!(
            native.available_columns.iter().any(|column| column == "pvs_off_row_page_skipped_min_useful_xts"),
            case == CleanupCase::NativeNulls
        );
    }
    let empty = analyze_case(CleanupCase::EmptyHorizon).await;
    assert!(empty.report.sources.iter().any(
        |source| source.source == source_id::PERSISTENT_VERSION_STORE && source.state == SourceState::NotApplicable
    ));
    let partial = analyze_case(CleanupCase::UnattributedParticipant).await;
    assert!(
        partial
            .report
            .sources
            .iter()
            .any(|source| source.source == source_id::OPEN_TRANSACTIONS && source.state == SourceState::Partial)
    );
    assert!(
        partial
            .report
            .entries
            .iter()
            .any(|entry| { entry.mechanisms.contains(&CleanupMechanism::UnattributedVersioning) })
    );
    let failed = analyze_case(CleanupCase::UnavailableNativeSources).await;
    assert!(failed.report.entries.is_empty());
    assert!(failed.report.sources.iter().all(|source| source.state == SourceState::Unavailable));
}

#[tokio::test]
async fn pg_horizons_preserve_independent_oldest_ties_and_only_the_captured_coverage_can_authorize_empty_bounds() {
    let tied = analyze_case(CleanupCase::TiedRetention).await;
    assert!(boundary(&tied.report, CleanupHorizon::Data).complete);
    assert_eq!(boundary(&tied.report, CleanupHorizon::Data).oldest_observed.len(), 2);
    assert!(boundary(&tied.report, CleanupHorizon::Catalog).complete);
    assert_eq!(boundary(&tied.report, CleanupHorizon::Catalog).oldest_observed.len(), 1);
    assert!(!boundary(&tied.report, CleanupHorizon::SharedCatalog).complete);
    for row in &tied.context.records {
        if let TelemetryRecord::Operational(row) = &row.row.record
            && let OperationalFact::Retention(holder) = &row.fact
        {
            assert!(holder.session.is_none() && holder.query.is_none());
            assert_eq!(holder.unresolved_session_id.as_deref(), Some("99"));
            assert_eq!(holder.scope, culprit_core::MeasurementScope::Server);
        }
    }
    let empty = analyze_case(CleanupCase::QualifiedEmptyRetention).await;
    assert!(empty.report.entries.is_empty());
    assert!(boundary(&empty.report, CleanupHorizon::Data).complete);
    assert!(boundary(&empty.report, CleanupHorizon::Catalog).complete);
    assert!(!boundary(&empty.report, CleanupHorizon::SharedCatalog).complete);
    let unknown = analyze_case(CleanupCase::UnqualifiedEmptyRetention).await;
    assert!(unknown.report.entries.is_empty());
    assert!(unknown.report.boundaries.iter().all(|boundary| !boundary.complete));
}

#[tokio::test]
async fn partial_visibility_unknown_distances_and_failed_sources_cannot_claim_a_complete_oldest_boundary() {
    for case in [CleanupCase::PartialRetention, CleanupCase::UnorderedRetention, CleanupCase::UnavailableRetention] {
        let analysis = analyze_case(case).await;
        assert!(analysis.report.boundaries.iter().all(|boundary| !boundary.complete), "{case:?}");
        if case != CleanupCase::UnavailableRetention {
            assert!(!analysis.report.entries.is_empty());
        }
        if case == CleanupCase::UnorderedRetention {
            assert_eq!(boundary(&analysis.report, CleanupHorizon::Data).oldest_observed.len(), 2);
        }
    }
}

#[tokio::test]
async fn losing_the_pg_metadata_row_in_the_warehouse_keeps_holder_prose_without_forging_coverage() {
    let (source, records) = cleanup_evidence(CleanupCase::TiedRetention);
    let fixture = Fixture::new(source, records).await;
    fixture.remove_first_row(TelemetryStream::OperationalSamples);
    let capture =
        fixture.read_families(&[culprit_collector::analytical::evidence::EvidenceFamily::OperationalSamples]).await;
    assert!(!capture.is_read_complete());
    let analysis = collected_resources::cleanup(&capture).unwrap();
    assert_eq!(analysis.report.entries.len(), 2);
    assert!(analysis.report.boundaries.iter().all(|boundary| !boundary.complete));
    assert!(
        analysis
            .report
            .sources
            .iter()
            .any(|source| source.source == RetentionCoverage::SOURCE && source.state == SourceState::Partial)
    );
    assert_eq!(analysis.context.records.len(), 2);
    assert!(analysis.context.records.iter().all(|row| matches!(&row.row.record,
        TelemetryRecord::Operational(row) if matches!(row.fact, OperationalFact::Retention(_)))));
}

#[tokio::test]
async fn cleanup_rejects_unrelated_datasets_and_operational_populations_without_a_declared_cleanup_family() {
    let (source, records) = culprit_collector::analytical::test_support::query_evidence();
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    assert_eq!(
        collected_resources::cleanup(&capture).unwrap_err().detail,
        Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema))
    );
    let (mut source, mut records) = cleanup_evidence(CleanupCase::UnrelatedOperationalCounter);
    records.retain(|row| matches!(row, TelemetryRecord::Operational(row) if row.source == "unrelated-operational"));
    let SourceObservation::Snapshot(native) = &mut source.observation else { unreachable!() };
    native.sources.retain(|source| source.source == "unrelated-operational");
    native.records = records.len() as u32;
    let fixture = Fixture::new(source, records).await;
    let capture =
        fixture.read_families(&[culprit_collector::analytical::evidence::EvidenceFamily::OperationalSamples]).await;
    assert_eq!(
        collected_resources::cleanup(&capture).unwrap_err().detail,
        Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::UnsupportedSchema))
    );
}

#[tokio::test]
async fn database_capture_rejects_foreign_unlabelled_and_other_scope_holders_even_with_valid_native_receipts() {
    use culprit_core::MeasurementScope;
    for defect in ["foreign-database", "missing-database", "node", "pool", "session", "cluster", "file"] {
        let (source, mut records) = cleanup_evidence(CleanupCase::TiedRetention);
        let database = source.target.database().unwrap().database.clone();
        let holder = records
            .iter_mut()
            .find_map(|record| match record {
                TelemetryRecord::Operational(OperationalObservation {
                    fact: OperationalFact::Retention(holder),
                    ..
                }) => Some(holder),
                _ => None,
            })
            .unwrap();
        holder.scope = MeasurementScope::Database;
        holder.database = Some(database);
        match defect {
            "foreign-database" => holder.database = Some("a-different-database".into()),
            "missing-database" => holder.database = None,
            "node" => holder.scope = MeasurementScope::Node,
            "pool" => holder.scope = MeasurementScope::Pool,
            "session" => holder.scope = MeasurementScope::Session,
            "cluster" => holder.scope = MeasurementScope::Cluster,
            "file" => holder.scope = MeasurementScope::File,
            _ => unreachable!(),
        }
        assert!(holder.is_valid(), "{defect}: the raw native fact is valid independently of database admission");
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let capture =
            fixture.read_families(&[culprit_collector::analytical::evidence::EvidenceFamily::OperationalSamples]).await;
        assert_cleanup_capture(&capture, &source, &records);
        assert!(capture.is_read_complete());
        assert_eq!(
            collected_resources::cleanup(&capture).unwrap_err().detail,
            Some(culprit_core::ErrorDetail::Delivery(DeliveryProblem::Integrity)),
            "{defect}"
        );
    }
}

#[tokio::test]
async fn server_scoped_slot_and_walsender_horizons_preserve_foreign_database_context_without_relabelling_it() {
    use culprit_core::{CleanupHolder, MeasurementScope};
    for kind in [CleanupHolder::ReplicationSlot, CleanupHolder::Session] {
        let (source, mut records) = cleanup_evidence(CleanupCase::TiedRetention);
        for record in &mut records {
            if let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::Retention(holder),
                ..
            }) = record
            {
                holder.scope = MeasurementScope::Server;
                holder.database = Some("context-from-another-database".into());
                holder.holder = kind;
                assert!(holder.is_valid());
            }
        }
        let fixture = Fixture::new(source.clone(), records.clone()).await;
        let capture =
            fixture.read_families(&[culprit_collector::analytical::evidence::EvidenceFamily::OperationalSamples]).await;
        assert_cleanup_capture(&capture, &source, &records);
        let analysis = collected_resources::cleanup(&capture).unwrap();
        assert_eq!(analysis.report.entries.len(), 2);
        assert!(boundary(&analysis.report, CleanupHorizon::Data).complete);
        assert_eq!(boundary(&analysis.report, CleanupHorizon::Data).oldest_observed.len(), 2);
        for record in analysis.context.records {
            if let TelemetryRecord::Operational(OperationalObservation {
                fact: OperationalFact::Retention(holder),
                ..
            }) = record.row.record
            {
                assert_eq!(holder.scope, MeasurementScope::Server);
                assert_eq!(holder.database.as_deref(), Some("context-from-another-database"));
            }
        }
    }
}
