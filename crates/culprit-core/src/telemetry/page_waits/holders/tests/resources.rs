use super::*;

const FIRST: &str = "5:1:42 (LATCH 0x0000000000000044: Class: BUFFER KP: 0 SH: 0 UP: 0 EX: 1 DT: 0 Sublatch: 0 HasWaiters: 1 Task: 0x0000000000000098 AnyReleasor: 0)";
const LAST: &str = "5:1:42 (LATCH 0x0000000000000044: Class: BUFFER KP: 0 SH: 0 UP: 0 EX: 1 DT: 0 Sublatch: 0 HasWaiters: 0 Task: 0x0000000000000099 AnyReleasor: 1)";

#[test]
fn original_native_task_coordinates_must_match_the_declared_address_and_header() {
    let (original, _, _) = fixture();
    assert!(original.is_valid());
    for resource in ["5:1:43", "5:2:42", "6:1:42", "5:1:42 (unrecognized diagnostic text)"] {
        let mut focus = original.clone();
        focus.task.resource.native.id = resource.into();
        assert!(focus.task.is_valid(), "native task structure alone does not bind a supported physical address");
        assert!(focus.address.matches_header(&focus.target.header));
        assert!(!focus.is_valid(), "{resource}");
        assert!(focus.key().is_err(), "a contradictory original cannot name an admitted question");
    }
    let mut decorated = original;
    decorated.task.resource.native.id = FIRST.into();
    assert!(decorated.is_valid(), "supported native diagnostics retain the exact same physical coordinates");
    assert_eq!(decorated.task.resource.native.id, FIRST, "the original native text remains unmodified");
}

#[test]
fn current_native_latch_flags_can_change_while_exact_page_request_and_source_guards_remain_intact() {
    let (focus, context, mut value) = fixture();
    let original_task = focus.task.clone();
    let original_owner = value.before.holder.request.as_ref().unwrap().key.clone();
    let page = value.page.as_mut().unwrap();
    page.resource.native.id = FIRST.into();
    page.task_wait.as_mut().unwrap().resource.native.id = FIRST.into();
    value.after_task.as_mut().unwrap().resource.native.id = LAST.into();
    assert!(value.is_valid());
    assert!(focus.accepts_observation(&context, &value));
    assert_eq!(focus.task, original_task);
    // Neither diagnostic Task suffix supplies the independently captured holder's identity.
    assert_eq!(value.before.holder.request.as_ref().unwrap().key, original_owner);
    assert_eq!(value.after.holder.request.as_ref().unwrap().key, original_owner);
    let retained = serde_json::to_string(&value).unwrap();
    assert!(retained.contains(FIRST) && retained.contains(LAST), "normalization never replaces the native text");
}

#[test]
fn claiming_the_original_wait_on_a_different_page_type_or_native_provenance_is_refused() {
    let (focus, context, original) = fixture();
    for field in ["database", "file", "page", "suffix", "kind", "wait-type", "qualifier", "source"] {
        let mut value = original.clone();
        let task = value.after_task.as_mut().unwrap();
        match field {
            "database" => task.resource.native.id = "6:1:42".into(),
            "file" => task.resource.native.id = "5:2:42".into(),
            "page" => task.resource.native.id = "5:1:43".into(),
            "suffix" => task.resource.native.id = "5:1:42 (unrecognized diagnostic text)".into(),
            "kind" => {
                task.resource.kind = WaitResourceKind::PageIo;
                task.wait_type = "PAGEIOLATCH_SH".into();
            }
            "wait-type" => task.wait_type = "PAGELATCH_SH".into(),
            "qualifier" => {
                task.resource.native.qualifiers.insert("native_context".into(), "another-context".into());
            }
            _ => {
                task.resource.native.qualifiers.insert("source".into(), "sys.dm_exec_requests.wait_resource".into());
            }
        }
        if field != "source" {
            assert!(value.is_valid(), "{field} is a native point, but cannot claim the original page wait");
        }
        assert!(!focus.accepts_observation(&context, &value), "{field}");
    }
}

#[test]
fn equivalent_resource_text_cannot_replace_generation_member_role_or_native_clock_proof() {
    let (focus, context, mut value) = fixture();
    value.after_task.as_mut().unwrap().resource.native.id = LAST.into();
    assert!(focus.accepts_observation(&context, &value));
    for field in ["request", "role", "epoch", "member", "clock"] {
        let mut current = value.clone();
        let mut source = context.clone();
        match field {
            "request" => {
                current.page.as_mut().unwrap().request.key.request.as_mut().unwrap().generation = "replacement".into();
            }
            "role" => source.database_writable = Some(false),
            "epoch" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut source.partition else { panic!() };
                *epoch = Some("replacement".into());
            }
            "member" => {
                let SourcePartition::InstanceLocal { member: Some(member), .. } = &mut source.partition else {
                    panic!()
                };
                member.native.id = "replacement".into();
            }
            _ => {
                current.after.observed_at = "2026-11-13T09:00:00Z".into();
                current.after_task.as_mut().unwrap().observed_at = current.after.observed_at.clone();
            }
        }
        assert!(!focus.accepts_observation(&source, &current), "{field}");
    }
}

#[test]
fn original_page_receipts_keep_exact_raw_task_resource_even_when_current_page_comparison_is_semantic() {
    let (mut focus, _, value) = fixture();
    let mut original = *value.page.unwrap();
    original.observed_from = focus.task.observed_at.clone();
    original.observed_until = focus.original_observed_until.clone();
    original.task_wait = Some(focus.task.clone());
    original.before_check.as_mut().unwrap().observed_at = "2026-11-13T10:00:00.3Z".into();
    original.after_check.as_mut().unwrap().observed_at = "2026-11-13T10:00:00.7Z".into();
    let source = CollectionDescription {
        target: focus.participant.target.clone(),
        partition: focus.participant.partition.clone(),
        source: original.source.clone(),
        continuity: SourceContinuity::Unverified,
        observation: SourceObservation::PageWaitTargets {
            original_activity: Some(focus.participant.parent.capture_ref()),
            database_writable: focus.participant.database_writable,
            snapshot: StateSnapshotDescription {
                dataset: SnapshotDataset::PageWaitTargets,
                observed_from: original.observed_from.clone(),
                observed_until: original.observed_until.clone(),
                records: 1,
                sources: vec![StateSourceCoverage::observed(original.source.clone(), 1, false)],
            },
        },
    };
    let exported = ExportedRecord::new(
        &source,
        &focus.page.capture,
        TelemetryRecord::Operational(OperationalObservation {
            source: original.source.clone(),
            key: original.key().unwrap(),
            fact: OperationalFact::PageWait(Box::new(original.clone())),
        }),
    )
    .unwrap();
    focus.page = DiagnosticEvidenceRef::from_export(&source, &focus.page.manifest_sha256, &exported, 0).unwrap();
    assert!(focus.matches_page(&source, &original, &focus.page));

    let mut altered = original;
    altered.resource.native.id = FIRST.into();
    altered.task_wait.as_mut().unwrap().resource.native.id = FIRST.into();
    assert!(altered.is_valid());
    assert!(altered.resource.same_sql_server_page(&focus.task.resource));
    assert!(!focus.matches_page(&source, &altered, &focus.page));
}
