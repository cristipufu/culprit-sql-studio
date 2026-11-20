use super::*;

pub(super) fn work() -> NativeSessionReadWork {
    let mut context = crate::telemetry::native_read_context::tests::read_context();
    let CollectionTarget::Database(database) = &mut context.target else { panic!() };
    database.database = "private-native-cost-db-canary".into();
    NativeSessionReadWork::MssqlSession(Box::new(MssqlSessionReadWork {
        qualification_work: None,
        settlement: NativeReadSettlement::Quiescent,
        issue: None,
        interval: Some(Box::new(MssqlSessionReadInterval {
            context,
            session: NativeActivityIdentity { id: "71".into(), generation: "2020-01-01T10:00:00.0030000".into() },
            reader_generation: "1".into(),
            initial: MssqlSessionReadPoint {
                query_ordinal: 3,
                cpu_ms: Some("12".into()),
                reads: Some("9007199254740993".into()),
                logical_reads: Some("9223372036854775803".into()),
                dirty_marks: Some("21".into()),
                memory_pages: Some("30".into()),
            },
            latest: Some(MssqlSessionReadPoint {
                query_ordinal: 9,
                cpu_ms: Some("17".into()),
                reads: Some("9007199254740997".into()),
                logical_reads: Some("9223372036854775807".into()),
                dirty_marks: Some("24".into()),
                memory_pages: Some("20".into()),
            }),
        })),
    }))
}
fn source(work: &NativeSessionReadWork) -> CollectionDescription {
    let NativeSessionReadWork::MssqlSession(work) = work;
    let interval = work.interval.as_ref().unwrap();
    CollectionDescription {
        target: interval.context.target.clone(),
        partition: interval.context.partition.clone(),
        source: "native-source-control".into(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::Snapshot(StateSnapshotDescription {
            dataset: SnapshotDataset::QueryCompilation,
            observed_from: interval.context.observed_from.clone(),
            observed_until: interval.context.observed_until.clone(),
            records: 0,
            sources: vec![StateSourceCoverage::observed("native-source-control".into(), 0, true)],
        }),
    }
}
fn observed(summary: &NativeSessionReadSummary) -> &MssqlSessionIntervalSummary {
    let NativeSessionReadSummary::MssqlSession { interval: Some(interval), .. } = summary else { panic!() };
    interval
}

#[test]
fn exact_session_deltas_keep_large_native_counts_and_decreasing_memory_as_a_gauge() {
    let work = work();
    assert!(work.is_valid());
    let summary = work.summary(Some(&source(&work))).unwrap();
    let NativeSessionReadSummary::MssqlSession { state, binding, .. } = &summary;
    assert_eq!(*state, NativeReadWorkState::Settled);
    assert_eq!(*binding, NativeReadOwnerBinding::Matched);
    let interval = observed(&summary);
    assert_eq!(interval.cpu_ms.as_deref(), Some("5"));
    assert_eq!(interval.reads.as_deref(), Some("4"));
    assert_eq!(interval.logical_reads.as_deref(), Some("4"));
    assert_eq!(interval.dirty_marks.as_deref(), Some("3"));
    assert_eq!(interval.memory_pages_initial.as_deref(), Some("30"));
    assert_eq!(interval.memory_pages_latest.as_deref(), Some("20"));
    assert_eq!(interval.first_query_ordinal, 3);
    assert_eq!(interval.last_query_ordinal, Some(9));
}

#[test]
fn an_initial_zero_dispatch_boundary_can_cover_the_whole_source_but_no_dispatch_cannot_settle() {
    let mut native = work();
    let NativeSessionReadWork::MssqlSession(body) = &mut native;
    body.interval.as_mut().unwrap().initial.query_ordinal = 0;
    assert!(native.is_valid());
    let summary = native.summary(Some(&source(&native))).unwrap();
    assert_eq!(observed(&summary).first_query_ordinal, 0);
    assert_eq!(observed(&summary).last_query_ordinal, Some(9));
    assert_eq!(observed(&summary).cpu_ms.as_deref(), Some("5"));

    let NativeSessionReadWork::MssqlSession(body) = &mut native;
    body.interval.as_mut().unwrap().latest.as_mut().unwrap().query_ordinal = 0;
    assert!(!native.is_valid(), "a final zero boundary proves no original dispatch");
    assert!(native.summary(None).is_err());
}

#[test]
fn public_summary_retains_an_opaque_original_proof_receipt_and_no_private_identity_or_preview_bytes() {
    let native = work();
    let raw = serde_json::to_value(&native).unwrap();
    let NativeSessionReadSummary::MssqlSession { reference, .. } = native.summary(Some(&source(&native))).unwrap();
    assert_eq!(reference, digest(&serde_json::to_vec(&native).unwrap()));
    let text = "authorization=private-source-template-canary";
    let metrics = ReadWorkMetrics {
        active_tasks: 0,
        untracked_tasks: 0,
        templates: vec![ReadQueryMetrics {
            template_id: digest(b"template"),
            statement: DefinitionPayload::Available {
                text: text.into(),
                sha256: digest(text.as_bytes()),
                redaction_version: REDACTION_VERSION,
            },
            completed_tasks: 1,
            failures: 0,
            timeouts: 0,
            cancellations: 0,
            unconfirmed_cancellations: 0,
            stop_causes: vec![],
            rows_decoded: "1".into(),
            charged_value_bytes: "9007199254740993".into(),
            client_elapsed_ms: 2.0,
        }],
        native_session_work: Some(Box::new(native.clone())),
    };
    let projected = metrics.summary(Some(&source(&native))).unwrap();
    let public = serde_json::to_string(&projected).unwrap();
    for private in [
        "private-native-cost-db-canary",
        "owned.invalid",
        "owned-machine",
        "owned-instance",
        "owned-process-epoch",
        "2020-01-01T10:00:00.0030000",
        "authorization=",
        "private-source-template-canary",
    ] {
        assert!(!public.contains(private), "{private}");
    }
    assert_eq!(serde_json::to_value(&native).unwrap(), raw, "projection cannot mask or re-sign the private proof");
    assert!(public.contains(&reference));
    assert!(public.contains("9007199254740993"));
    assert!(
        serde_json::from_str::<ReadWorkMetrics>(&public).is_err(),
        "a public projection is not the original private measurement"
    );
}

#[test]
fn provisional_and_unconfirmed_source_work_never_exposes_native_deltas() {
    for settlement in [NativeReadSettlement::Pending, NativeReadSettlement::Unconfirmed] {
        let mut native = work();
        let NativeSessionReadWork::MssqlSession(body) = &mut native;
        body.settlement = settlement;
        let summary = native.summary(Some(&source(&native))).unwrap();
        let interval = observed(&summary);
        assert!(interval.cpu_ms.is_none() && interval.reads.is_none() && interval.memory_pages_latest.is_none());
        let NativeSessionReadSummary::MssqlSession { state, .. } = summary;
        assert_eq!(
            state,
            if settlement == NativeReadSettlement::Pending {
                NativeReadWorkState::Observing
            } else {
                NativeReadWorkState::Unavailable
            }
        );
        assert_eq!(native.has_unconfirmed_work(), settlement == NativeReadSettlement::Unconfirmed);
    }
}

#[test]
fn source_poison_and_unconfirmed_shutdown_remain_independent_and_do_not_heal() {
    let mut native = work();
    let NativeSessionReadWork::MssqlSession(body) = &mut native;
    body.issue = Some(NativeReadWorkIssue::SourceChanged);
    body.settlement = NativeReadSettlement::Unconfirmed;
    let summary = native.summary(Some(&source(&native))).unwrap();
    let NativeSessionReadSummary::MssqlSession { issue, settlement, state, .. } = &summary;
    assert_eq!(*issue, Some(NativeReadWorkIssue::SourceChanged));
    assert_eq!(*settlement, NativeReadSettlement::Unconfirmed);
    assert_eq!(*state, NativeReadWorkState::Unavailable);
    assert!(observed(&summary).cpu_ms.is_none());
}

#[test]
fn a_missing_original_counter_is_unknown_even_when_the_final_counter_is_recorded() {
    let mut native = work();
    let NativeSessionReadWork::MssqlSession(body) = &mut native;
    body.interval.as_mut().unwrap().initial.cpu_ms = None;
    let summary = native.summary(Some(&source(&native))).unwrap();
    assert!(observed(&summary).cpu_ms.is_none());
    assert_eq!(observed(&summary).reads.as_deref(), Some("4"));
}

#[test]
fn a_counter_decrease_must_be_explicit_and_never_wraps_into_a_positive_delta() {
    let mut native = work();
    let NativeSessionReadWork::MssqlSession(body) = &mut native;
    body.interval.as_mut().unwrap().latest.as_mut().unwrap().cpu_ms = Some("1".into());
    assert!(!native.is_valid());
    let NativeSessionReadWork::MssqlSession(body) = &mut native;
    body.issue = Some(NativeReadWorkIssue::CounterDecreased);
    assert!(native.is_valid());
    let summary = native.summary(Some(&source(&native))).unwrap();
    assert!(observed(&summary).cpu_ms.is_none());
}

#[test]
fn logical_or_unknown_dataset_mapping_does_not_become_a_false_session_owner_conflict() {
    let native = work();
    let mut original = source(&native);
    original.partition = SourcePartition::Target;
    let summary = native.summary(Some(&original)).unwrap();
    let NativeSessionReadSummary::MssqlSession { binding, state, .. } = &summary;
    assert_eq!(*binding, NativeReadOwnerBinding::Unverified);
    assert_eq!(*state, NativeReadWorkState::Settled);
    assert_eq!(observed(&summary).cpu_ms.as_deref(), Some("5"));
}

#[test]
fn shared_dataset_and_application_database_session_remain_separate_scopes() {
    let native = work();
    let mut original = source(&native);
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &original.partition else { panic!() };
    original.target = CollectionTarget::Shared(member.clone());
    let summary = native.summary(Some(&original)).unwrap();
    let NativeSessionReadSummary::MssqlSession { binding, state, .. } = &summary;
    assert_eq!(*binding, NativeReadOwnerBinding::Unverified);
    assert_eq!(*state, NativeReadWorkState::Settled);
    assert_eq!(observed(&summary).cpu_ms.as_deref(), Some("5"));
}

#[test]
fn known_foreign_member_or_same_scope_epoch_withholds_native_values_without_rewriting_proof() {
    for field in ["member", "epoch", "engine"] {
        let native = work();
        let mut original = source(&native);
        match field {
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut original.partition else {
                    panic!()
                };
                member.native.id = "other-native-reader".into();
            }
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut original.partition else { panic!() };
                *epoch = Some("other-process".into());
            }
            "engine" => {
                let CollectionTarget::Database(database) = &mut original.target else { panic!() };
                database.engine = crate::Engine::Postgres;
            }
            _ => unreachable!(),
        }
        let summary = native.summary(Some(&original)).unwrap();
        let NativeSessionReadSummary::MssqlSession { binding, state, .. } = &summary;
        assert_eq!(*binding, NativeReadOwnerBinding::Contradictory, "{field}");
        assert_eq!(*state, NativeReadWorkState::Unavailable);
        assert!(observed(&summary).cpu_ms.is_none());
        assert!(native.is_valid());
    }
}

#[test]
fn native_signed_counter_bounds_and_canonical_integers_are_deliberate() {
    for field in ["cpu", "memory", "reads", "leading_zero", "negative", "ordinal", "session", "login", "generation"] {
        let mut native = work();
        let NativeSessionReadWork::MssqlSession(body) = &mut native;
        let interval = body.interval.as_mut().unwrap();
        match field {
            "cpu" => interval.initial.cpu_ms = Some((i64::from(i32::MAX) + 1).to_string()),
            "memory" => interval.initial.memory_pages = Some((i64::from(i32::MAX) + 1).to_string()),
            "reads" => interval.initial.reads = Some((i64::MAX as u64 + 1).to_string()),
            "leading_zero" => interval.initial.cpu_ms = Some("001".into()),
            "negative" => interval.initial.cpu_ms = Some("-1".into()),
            "ordinal" => {
                interval.initial.query_ordinal = 0;
                interval.latest.as_mut().unwrap().query_ordinal = 0;
            }
            "session" => interval.session.id = "0".into(),
            "login" => interval.session.generation = "1900-01-01T00:00:00.0000000".into(),
            "generation" => interval.reader_generation = "0".into(),
            _ => unreachable!(),
        }
        assert!(!native.is_valid(), "{field}");
        assert!(native.summary(None).is_err());
    }
}

#[test]
fn a_session_proof_requires_its_connected_database_context_independent_of_any_shared_dataset() {
    let mut native = work();
    let NativeSessionReadWork::MssqlSession(body) = &mut native;
    let interval = body.interval.as_mut().unwrap();
    let SourcePartition::InstanceLocal { member: Some(member), .. } = &interval.context.partition else { panic!() };
    interval.context.target = CollectionTarget::Shared(member.clone());
    assert!(
        interval.context.is_valid(),
        "the shared resource is valid independently, but is not the connected-session database"
    );
    assert!(!native.is_valid());
    assert!(native.summary(None).is_err());
}
