use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

#[async_trait::async_trait]
impl ProbeRead for FakeRead {
    async fn application_probe(&mut self, probe: &OwnedApplicationProbe) -> ReadResult<Vec<Rec>> {
        NativeRead::query(self, probe.sql(), vec![], 1).await
    }
}

fn row(index: usize) -> Rec {
    let identity = replace(&crate::query_store::tests::summary_row(), &[("query_id", Val::Int(index as i64 + 1))]);
    let mut fields = identity.columns().map(|(name, value)| (name, value.clone())).collect::<Vec<_>>();
    fields.push(("engine_internal", Val::Int(0)));
    fields.push(("cloud_internal", Val::Int(0)));
    fields.push(("fixture_text", Val::Text(probe_sql("/* fixture marker */", index, ProbePolicy::Collector))));
    Rec::of(&fields)
}
fn rows() -> Vec<Rec> {
    (0..MAX_PROBES).rev().map(row).collect()
}

#[test]
fn all_64_original_native_identities_and_projections_are_required_independent_of_row_order() {
    let mut progress = SetupProgress::default();
    let identities = decode_population(&rows(), &mut progress, MAX_PROBES, ProbePolicy::Collector).unwrap();
    assert_eq!(identities.len(), 64);
    assert_eq!(progress.lookup_rows, Some(64));
    assert_eq!(progress.rows_examined, 64);
    assert_eq!(progress.matched_projections, 64);
    assert_eq!(progress.unique_projections, 64);
    assert_eq!(progress.decoded_identities, 64);
    assert_eq!(progress.unique_identities, 64);
    assert_eq!(progress.stage, SetupStage::Complete);
    assert_eq!(progress.failure, None);
    assert_eq!(identities["1"], decode::query_identity(&row(0)).unwrap());
}

#[test]
fn empty_and_incomplete_native_populations_report_observed_counts_instead_of_guessed_parser_failures() {
    for count in [0, 1, 63] {
        let mut progress = SetupProgress::default();
        assert!(decode_population(&rows()[..count], &mut progress, MAX_PROBES, ProbePolicy::Collector).is_err());
        assert_eq!(progress.lookup_rows, Some(count));
        assert_eq!(progress.unique_projections, count);
        assert_eq!(progress.unique_identities, count);
        assert_eq!(progress.failure, Some(SetupFailure::PopulationCardinality));
        assert_eq!(progress.stage, SetupStage::VerifyPopulation);
    }
}

#[test]
fn duplicate_projections_and_duplicate_native_ids_are_distinct_failures_never_representative_64() {
    let mut duplicate_projection = rows();
    duplicate_projection[0] = replace(
        &row(63),
        &[("fixture_text", Val::Text(probe_sql("/* fixture marker */", 62, ProbePolicy::Collector)))],
    );
    let mut progress = SetupProgress::default();
    assert!(decode_population(&duplicate_projection, &mut progress, MAX_PROBES, ProbePolicy::Collector).is_err());
    assert_eq!(progress.failure, Some(SetupFailure::DuplicateProjection));
    assert!(progress.unique_identities < 64);

    let mut duplicate_id = rows();
    duplicate_id[0] = replace(&row(63), &[("query_id", Val::Int(63))]);
    let mut progress = SetupProgress::default();
    assert!(decode_population(&duplicate_id, &mut progress, MAX_PROBES, ProbePolicy::Collector).is_err());
    assert_eq!(progress.failure, Some(SetupFailure::DuplicateIdentity));
    assert!(progress.unique_identities < 64);
}

#[test]
fn fixture_projection_rejections_record_safe_native_shape_without_silently_relaxing_the_match() {
    for (text, semicolon, brackets, double_quotes) in [
        ("SELECT 1 AS culprit_owned_compile_0;", true, false, false),
        ("SELECT 1 AS [culprit_owned_compile_0]", false, true, false),
        ("SELECT 1 AS \"culprit_owned_compile_0\"", false, false, true),
        ("SELECT 1 AS private-projection-canary", false, false, false),
    ] {
        let rows = [replace(&row(0), &[("fixture_text", Val::Text(text.into()))])];
        let mut progress = SetupProgress::default();
        assert!(decode_population(&rows, &mut progress, MAX_PROBES, ProbePolicy::Collector).is_err());
        assert_eq!(progress.failure, Some(SetupFailure::ProjectionNotRecognized));
        let shape = progress.rejected_projection_shape.as_ref().unwrap();
        assert_eq!(shape.ends_in_semicolon, semicolon);
        assert_eq!(shape.bracket_quoted, brackets);
        assert_eq!(shape.double_quoted, double_quotes);
        assert!(!serde_json::to_string(&progress).unwrap().contains("private-projection-canary"));
    }
}

#[test]
fn native_identity_decode_failure_preserves_field_availability_without_values_or_raw_text() {
    for (field, value, kind) in [
        ("query_id", Val::Null, "null"),
        ("context_settings_id", Val::Float(1.0), "float"),
        ("query_hash", Val::Null, "null"),
        ("query_initial_compile_at", Val::Text("private-clock-canary".into()), "text"),
    ] {
        let mut progress = SetupProgress::default();
        let rows = [replace(&row(0), &[(field, value)])];
        let original = decode::query_identity(&rows[0]).unwrap_err();
        let error = decode_population(&rows, &mut progress, MAX_PROBES, ProbePolicy::Collector).unwrap_err();
        assert_eq!(error.code, original.code);
        assert_eq!(error.detail, original.detail);
        assert_eq!(progress.failure, Some(SetupFailure::NativeIdentity));
        assert_eq!(progress.identity_fields.as_ref().unwrap().kinds[field], kind);
        assert_eq!(progress.decoded_identities, 0);
        assert!(!serde_json::to_string(&progress).unwrap().contains("private-clock-canary"));
    }
}

#[test]
fn no_lookup_yet_and_an_observed_empty_lookup_are_distinct_and_overflow_cannot_pass() {
    let not_read = SetupProgress::default();
    assert_eq!(not_read.lookup_rows, None);
    let mut observed = SetupProgress::default();
    assert!(decode_population(&[], &mut observed, MAX_PROBES, ProbePolicy::Collector).is_err());
    assert_eq!(observed.lookup_rows, Some(0));
    let mut overflow = rows();
    overflow.push(row(0));
    let mut progress = SetupProgress::default();
    assert!(decode_population(&overflow, &mut progress, MAX_PROBES, ProbePolicy::Collector).is_err());
    assert_eq!(progress.lookup_rows, Some(65));
    assert!(progress.unique_identities <= 64);
}

fn inventory(query_rows: i64, marker_query_rows: i64, alias_query_rows: i64) -> Rec {
    Rec::of(&[
        ("query_rows", Val::Int(query_rows)),
        ("marker_query_rows", Val::Int(marker_query_rows)),
        ("alias_query_rows", Val::Int(alias_query_rows)),
    ])
}

#[tokio::test]
async fn successful_and_rejected_nonempty_populations_never_request_inventory_or_repeat_workload() {
    let mut owner = FakeRead::new(vec![]);
    let mut progress = SetupProgress::default();
    assert_eq!(
        resolve_population(&mut owner, "owned marker", &rows(), &mut progress, MAX_PROBES, ProbePolicy::Collector)
            .await
            .unwrap()
            .len(),
        64
    );
    assert!(owner.calls.is_empty());
    assert!(progress.empty_lookup_inventory.is_none());
    for native in [
        vec![row(0)],
        vec![replace(&row(0), &[("fixture_text", Val::Text("SELECT 1 AS unrecognized_projection".into()))])],
    ] {
        let mut progress = SetupProgress::default();
        assert!(
            resolve_population(&mut owner, "owned marker", &native, &mut progress, MAX_PROBES, ProbePolicy::Collector)
                .await
                .is_err()
        );
        assert!(owner.calls.is_empty());
        assert!(progress.empty_lookup_inventory.is_none());
    }
}

#[tokio::test]
async fn empty_lookup_records_one_parameterized_exact_inventory_but_cannot_supply_the_original_identities() {
    let mut owner = FakeRead::new(vec![Ok(vec![inventory(9_007_199_254_740_993, 0, 64)])]);
    let mut progress = SetupProgress::default();
    let error =
        resolve_population(&mut owner, "private-marker-canary", &[], &mut progress, MAX_PROBES, ProbePolicy::Collector)
            .await
            .unwrap_err();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage)));
    assert_eq!(progress.failure, Some(SetupFailure::PopulationCardinality));
    assert_eq!(progress.lookup_rows, Some(0));
    assert_eq!(progress.decoded_identities, 0);
    let observed = progress.empty_lookup_inventory.as_ref().unwrap();
    assert!(observed.complete);
    let counts = observed.counts.as_ref().unwrap();
    assert_eq!(counts.query_rows, "9007199254740993");
    assert_eq!(counts.marker_query_rows, "0");
    assert_eq!(counts.alias_query_rows, "64");
    assert_eq!(owner.calls.len(), 1);
    assert_eq!(owner.calls[0].limit, 1);
    assert_eq!(owner.calls[0].sql, EMPTY_LOOKUP_INVENTORY_SQL);
    assert!(owner.calls[0].sql.contains("LEFT JOIN"), "all query rows remain counted when text is absent");
    assert!(!owner.calls[0].sql.contains("private-marker-canary"));
    assert!(matches!(&owner.calls[0].parameters[0], Parameter::Text(marker) if marker == "private-marker-canary"));
    assert!(matches!(&owner.calls[0].parameters[1], Parameter::Text(prefix) if prefix == "culprit_owned_compile_"));
    assert!(!serde_json::to_string(&progress).unwrap().contains("private-marker-canary"));
}

#[tokio::test]
async fn denied_missing_or_invalid_inventory_preserves_original_failure_and_unknown_counts_without_retry() {
    for reply in [
        Err(ReadError::Source(culprit_core::Error::new(
            culprit_core::ErrorCode::PermissionDenied,
            "private-native-error",
        ))),
        Ok(vec![]),
        Ok(vec![replace(&inventory(64, 0, 64), &[("marker_query_rows", Val::Null)])]),
        Ok(vec![inventory(1, 0, 2)]),
    ] {
        let mut owner = FakeRead::new(vec![reply]);
        let mut progress = SetupProgress::default();
        let error =
            resolve_population(&mut owner, "owned marker", &[], &mut progress, MAX_PROBES, ProbePolicy::Collector)
                .await
                .unwrap_err();
        assert_eq!(error.detail, Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage)));
        assert_eq!(progress.failure, Some(SetupFailure::PopulationCardinality));
        assert_eq!(progress.decoded_identities, 0);
        let observed = progress.empty_lookup_inventory.as_ref().unwrap();
        assert!(!observed.complete);
        assert!(observed.counts.is_none());
        assert!(observed.error.is_some());
        assert_eq!(owner.calls.len(), 1);
        assert!(!serde_json::to_string(&progress).unwrap().contains("private-native-error"));
    }
}

fn options() -> Rec {
    Rec::of(&[
        ("actual_state_desc", Val::Text("READ_WRITE".into())),
        ("query_capture_mode_desc", Val::Text("ALL".into())),
    ])
}
fn table_result(count: i64, value: Val) -> Rec {
    Rec::of(&[("fixture_rows", Val::Int(count)), ("fixture_value", value)])
}
fn probe_result(index: usize, value: Val) -> Rec {
    Rec::of(&[(format!("culprit_owned_compile_{index}").as_str(), value)])
}

#[tokio::test]
async fn owned_one_row_setup_and_64_distinct_projected_reads_share_one_bounded_reader_and_exact_oracle() {
    let mut replies = vec![Ok(vec![options()]), Ok(vec![table_result(1, Val::Int(1))])];
    replies.extend((0..MAX_PROBES).map(|index| Ok(vec![probe_result(index, Val::Int(1))])));
    replies.push(Ok(vec![])); // Existing flush.
    replies.push(Ok(rows())); // Original marker lookup and native identity decoder.
    let mut owner = FakeRead::new(replies).with_query_limit(70);
    let mut progress = SetupProgress::default();
    let identities =
        seed_and_resolve(&mut owner, &mut progress, MAX_PROBES, ProbePolicy::Collector, None).await.unwrap();
    assert_eq!(identities.len(), 64);
    assert_eq!(owner.calls.len(), 68);
    assert_eq!(owner.calls[1].sql, OWNED_TABLE_SETUP_SQL);
    assert_eq!(owner.calls[1].limit, 1);
    assert_eq!(progress.owned_table_rows.as_deref(), Some("1"));
    assert_eq!(progress.owned_table_value.as_deref(), Some("1"));
    assert_eq!(progress.probes_completed, 64);
    assert_eq!(progress.unique_identities, 64);
    assert!(progress.empty_lookup_inventory.is_none());
    for (index, call) in owner.calls[2..66].iter().enumerate() {
        assert_eq!(call.limit, 1);
        assert!(call.sql.contains("(SELECT /* culprit */ id FROM dbo.culprit_owned_compile_probe)"));
        assert_eq!(probe_projection(&call.sql, MAX_PROBES), Some(index));
    }
    assert!(OWNED_TABLE_SETUP_SQL.contains("CREATE TABLE /* culprit */"));
    assert!(OWNED_TABLE_SETUP_SQL.contains("INSERT /* culprit */"));
    assert!(OWNED_TABLE_SETUP_SQL.contains("SELECT /* culprit */ COUNT_BIG(*)"));
}

#[tokio::test]
async fn wrong_or_unknown_table_cardinality_and_value_stop_before_any_probe_or_source_work() {
    for table in [
        vec![],
        vec![table_result(0, Val::Null)],
        vec![table_result(2, Val::Int(1))],
        vec![table_result(1, Val::Null)],
        vec![table_result(1, Val::Int(2))],
    ] {
        let mut owner = FakeRead::new(vec![Ok(vec![options()]), Ok(table)]);
        let mut progress = SetupProgress::default();
        assert!(seed_and_resolve(&mut owner, &mut progress, MAX_PROBES, ProbePolicy::Collector, None).await.is_err());
        assert_eq!(owner.calls.len(), 2);
        assert_eq!(progress.probes_completed, 0);
        assert!(!progress.flush_completed);
        assert_eq!(progress.lookup_rows, None);
        assert!(matches!(progress.failure, Some(SetupFailure::OwnedTableRows | SetupFailure::OwnedTableValue)));
    }
}

#[tokio::test]
async fn a_probe_returning_null_or_the_wrong_owned_value_cannot_count_as_a_completed_projection() {
    for value in [Val::Null, Val::Int(0), Val::Int(2)] {
        let mut owner = FakeRead::new(vec![
            Ok(vec![options()]),
            Ok(vec![table_result(1, Val::Int(1))]),
            Ok(vec![probe_result(0, value)]),
        ]);
        let mut progress = SetupProgress::default();
        assert!(seed_and_resolve(&mut owner, &mut progress, MAX_PROBES, ProbePolicy::Collector, None).await.is_err());
        assert_eq!(owner.calls.len(), 3);
        assert_eq!(progress.probes_completed, 0);
        assert_eq!(progress.failure, Some(SetupFailure::ProbeValue));
        assert!(!progress.flush_completed);
        assert_eq!(progress.lookup_rows, None);
    }
}

#[tokio::test]
async fn one_and_sixteen_probes_use_the_same_table_decoder_and_exact_requested_native_oracle() {
    for count in [1, 16] {
        let mut replies = vec![Ok(vec![options()]), Ok(vec![table_result(1, Val::Int(1))])];
        replies.extend((0..count).map(|index| Ok(vec![probe_result(index, Val::Int(1))])));
        replies.push(Ok(vec![]));
        replies.push(Ok((0..count).rev().map(row).collect()));
        let mut owner = FakeRead::new(replies);
        let mut progress = SetupProgress::default();
        let identities =
            seed_and_resolve(&mut owner, &mut progress, count, ProbePolicy::Collector, None).await.unwrap();
        assert_eq!(identities.len(), count);
        assert_eq!(owner.calls.len(), count + 4);
        assert_eq!(owner.calls.last().unwrap().limit, count + 1);
        assert_eq!(progress.unique_projections, count);
        assert_eq!(progress.unique_identities, count);
    }
}

#[tokio::test]
async fn invalid_probe_counts_and_missing_application_authority_refuse_before_options_or_table_work() {
    for count in [0, MAX_PROBES + 1, usize::MAX] {
        let mut owner = FakeRead::new(vec![]);
        assert!(
            seed_and_resolve(&mut owner, &mut SetupProgress::default(), count, ProbePolicy::Collector, None)
                .await
                .is_err()
        );
        assert!(owner.calls.is_empty());
    }
    let mut owner = FakeRead::new(vec![]);
    assert!(
        seed_and_resolve(&mut owner, &mut SetupProgress::default(), 1, ProbePolicy::OwnedApplication, None)
            .await
            .is_err()
    );
    assert!(owner.calls.is_empty());
}

fn pool() -> Arc<Pool> {
    Arc::new(Pool::new(super::super::config(1, "fixture", "unused-fixture-password", "fixture-probe-test"), None))
}

fn application_row(index: usize) -> Rec {
    replace(
        &row(index),
        &[("fixture_text", Val::Text(probe_sql("/* fixture marker */", index, ProbePolicy::OwnedApplication)))],
    )
}

#[tokio::test]
async fn owned_application_uses_one_real_table_probe_and_requires_actual_native_application_provenance() {
    let mut owner = FakeRead::new(vec![
        Ok(vec![options()]),
        Ok(vec![table_result(1, Val::Int(1))]),
        Ok(vec![probe_result(0, Val::Int(1))]),
        Ok(vec![]),
        Ok(vec![application_row(0)]),
    ]);
    let mut progress = SetupProgress::default();
    let identities =
        seed_and_resolve(&mut owner, &mut progress, 1, ProbePolicy::OwnedApplication, Some(&pool())).await.unwrap();
    assert_eq!(identities.len(), 1);
    assert_eq!(owner.calls.len(), 5);
    assert!(owner.calls[2].sql.contains("(SELECT id FROM dbo.culprit_owned_compile_probe)"));
    assert!(!owner.calls[2].sql.contains(crate::statement_origin::OWN_QUERY_MARKER));
    assert_eq!(owner.calls[2].limit, 1);
    for (policy, native) in [
        (ProbePolicy::Collector, application_row(0)),
        (ProbePolicy::OwnedApplication, row(0)),
        (ProbePolicy::OwnedApplication, replace(&application_row(0), &[("engine_internal", Val::Null)])),
    ] {
        let mut rejected = SetupProgress::default();
        assert!(decode_population(&[native], &mut rejected, 1, policy).is_err());
        assert_eq!(rejected.failure, Some(SetupFailure::ProbeProvenance));
        assert_eq!(rejected.unique_identities, 0);
    }
}

#[tokio::test]
async fn application_probe_cannot_cross_pool_ownership_even_with_identical_configuration() {
    let admitted_pool = pool();
    let probe = OwnedApplicationProbe {
        pool: admitted_pool.clone(),
        sql: probe_sql("/* fixture marker */", 0, ProbePolicy::OwnedApplication),
    };
    assert!(probe.authorizes_pool(&admitted_pool));
    let other_pool = pool();
    assert!(!probe.authorizes_pool(&other_pool));
    let mut owner = ReadSession::new(other_pool, CollectionBudget::default());
    let error = owner.owned_application_probe(&probe).await.unwrap_err().into_error();
    assert_eq!(error.detail, Some(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage)));
    assert_eq!(owner.queries(), 0, "no admission, connection or native statement is attempted");
    assert_eq!(owner.close().await, SourceShutdown::Quiescent);
}
