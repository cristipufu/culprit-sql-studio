use super::super::{CacheEntryMemoryEvidence, compare};
use super::*;

#[derive(Clone)]
struct Frame {
    context: NativeReadContext,
    rows: Vec<OperationalObservation>,
    source: SourceOutcome,
    complete: bool,
}

impl Frame {
    fn new(mut rows: Vec<OperationalObservation>, at: &str) -> Self {
        let mut context = context();
        context.database_writable = Some(true);
        context.observed_from = at.into();
        context.observed_until = at.into();
        for row in &mut rows {
            let entry = native(row);
            entry.observed_at = at.into();
            row.key = entry.key().unwrap();
            assert!(row.is_valid());
        }
        Self {
            context,
            rows,
            source: SourceOutcome {
                source: "selected-cache-memory".into(),
                state: SourceState::Partial,
                reason: None,
                reason_message: None,
            },
            complete: true,
        }
    }
    fn run(&self, previous: &Self) -> CacheEntryMemoryReport {
        let before = previous.rows.iter().collect::<Vec<_>>();
        let after = self.rows.iter().collect::<Vec<_>>();
        compare(
            CacheEntryMemoryEvidence {
                context: &previous.context,
                observations: &before,
                source: Some(&previous.source),
                read_complete: previous.complete,
            },
            CacheEntryMemoryEvidence {
                context: &self.context,
                observations: &after,
                source: Some(&self.source),
                read_complete: self.complete,
            },
        )
        .unwrap()
    }
}

fn pair() -> (Frame, Frame) {
    let mut anchor = observation("0x0102", Some("1"));
    native(&mut anchor).size_bytes = "131072".into();
    native(&mut anchor).lookup_count = "40".into();
    native(&mut anchor).object_type = "Prepared".into();
    let before = Frame::new(vec![anchor.clone()], "2020-01-01T06:00:02Z");
    native(&mut anchor).lookup_count = "42".into();
    let mut additional = observation("0x0506", Some("4097"));
    native(&mut additional).size_bytes = "524288".into();
    native(&mut additional).lookup_count = "1".into();
    native(&mut additional).object_type = "Prepared".into();
    let after = Frame::new(vec![anchor, additional], "2020-01-01T06:00:04Z");
    (before, after)
}

#[test]
fn additionally_observed_set_context_and_retained_bytes_keep_lookup_deltas_and_whole_entries_separate() {
    let (before, after) = pair();
    let report = after.run(&before);
    let comparison = report.comparison.unwrap();
    assert_eq!((comparison.entries_before, comparison.entries_after), (1, 2));
    assert_eq!((comparison.bytes_before.as_str(), comparison.bytes_after.as_str()), ("131072", "655360"));
    assert_eq!(comparison.retained_entries.len(), 1);
    let retained = &comparison.retained_entries[0];
    assert_eq!(retained.baseline_observation_key, before.rows[0].key);
    assert_eq!(retained.observation_key, after.rows[0].key);
    let metric = |key| retained.metrics.iter().find(|metric| metric.is(key)).unwrap().text.as_deref().unwrap();
    assert_eq!(metric("view.cache_entry_memory.comparison.metric.byte_change"), "0");
    assert_eq!(metric("view.cache_entry_memory.comparison.metric.lookup_change"), "2");
    assert_eq!(comparison.additional_contexts.len(), 1);
    let additional = &comparison.additional_contexts[0];
    assert_eq!(additional.left_observation_key, before.rows[0].key);
    assert_eq!(additional.right_observation_key, after.rows[1].key);
    assert_eq!(additional.attributes, ["set_options"]);
    assert!(additional.explanation.contains("ARITH_ABORT"));
    assert!(additional.explanation.starts_with(&t!("view.cache_entry_memory.comparison.additional")));
    assert!(comparison.limitations.contains(&t!("view.cache_entry_memory.comparison.limit.selected")));
    assert_eq!(report.source_state, Some(SourceState::Partial));
    assert_eq!(report.entries.len(), 2, "each multi-statement object is charged once");
    let encoded = serde_json::to_value(comparison).unwrap();
    for absent in ["severity", "fix_sql", "executions", "global_growth", "causal_share"] {
        assert!(encoded.get(absent).is_none());
    }
}

#[test]
fn additional_context_selection_is_deterministic_under_both_original_row_orders() {
    let (before, mut after) = pair();
    let expected = serde_json::to_value(after.run(&before).comparison).unwrap();
    after.rows.reverse();
    assert_eq!(serde_json::to_value(after.run(&before).comparison).unwrap(), expected);
}

#[test]
fn unknown_role_member_process_or_archive_body_cannot_lend_the_previous_selection() {
    let (before, after) = pair();
    for missing in ["role", "process", "member", "before-body", "after-body"] {
        let (mut before, mut after) = (before.clone(), after.clone());
        match missing {
            "role" => before.context.database_writable = None,
            "process" => {
                let SourcePartition::InstanceLocal { epoch, .. } = &mut before.context.partition else { panic!() };
                *epoch = Some(digest(b"other-native-process"));
            }
            "member" => {
                let SourcePartition::InstanceLocal { member, epoch } = &mut before.context.partition else { panic!() };
                *member = None;
                *epoch = None;
            }
            "before-body" => before.complete = false,
            "after-body" => after.complete = false,
            _ => unreachable!(),
        }
        let report = after.run(&before);
        assert!(report.comparison.is_none(), "{missing}");
        assert_eq!(report.entries.len(), 2, "{missing}: current point evidence remains");
    }
}

#[test]
fn unknown_or_unavailable_native_source_and_reversed_or_same_clocks_are_not_a_zero_growth_result() {
    let (before, after) = pair();
    for state in [SourceState::Unavailable, SourceState::NotApplicable] {
        let mut unavailable = before.clone();
        unavailable.rows.clear();
        unavailable.source.state = state;
        assert!(after.run(&unavailable).comparison.is_none());
    }
    assert!(before.run(&after).comparison.is_none());
    assert!(before.run(&before).comparison.is_none());
    let mut unknown = before.clone();
    unknown.source.source = "different-native-source".into();
    for row in &mut unknown.rows {
        row.source = unknown.source.source.clone();
    }
    assert!(after.run(&unknown).comparison.is_none());
}

#[test]
fn changed_incarnation_incomplete_statement_roster_and_reversed_cache_counter_withhold_continuity() {
    let (before, after) = pair();
    for changed in ["generation", "created", "roster", "counter", "keys"] {
        let mut after = after.clone();
        let row = native(&mut after.rows[0]);
        match changed {
            "generation" => {
                for statement in &mut row.statements {
                    statement.context.identity.plan_generation = "900".into();
                }
            }
            "created" => {
                for statement in &mut row.statements {
                    statement.context.identity.created_without_offset = "2020-01-01T05:59:00".into();
                }
            }
            "roster" => {
                row.statements.pop();
            }
            "counter" => row.lookup_count = "39".into(),
            "keys" => row.cache_keys[0].value = Some("4097".into()),
            _ => unreachable!(),
        }
        assert!(row.is_valid());
        assert!(after.run(&before).comparison.is_none(), "{changed}");
    }
}

#[test]
fn healthy_same_context_native_variants_unsupported_flags_and_other_contexts_keep_no_fragmentation_claim() {
    let (before, after) = pair();
    for changed in ["same-context", "parameterization", "other-key", "sql-handle", "body", "missing-sql"] {
        let mut after = after.clone();
        let row = native(&mut after.rows[1]);
        match changed {
            "same-context" => row.cache_keys[0].value = Some("1".into()),
            "parameterization" => row.cache_keys[0].value = Some("4099".into()),
            "other-key" => row.cache_keys.push(CacheKeyAttribute {
                name: "user_id".into(),
                native_type: Some("int".into()),
                value: Some("2".into()),
            }),
            "sql-handle" => {
                row.sql_handle = "0x7788".into();
                for statement in &mut row.statements {
                    statement.context.identity.sql_handle = row.sql_handle.clone();
                }
            }
            "body" => row.statements[0].statement = DefinitionPayload::sql("SELECT other", Engine::Mssql),
            "missing-sql" => {
                for statement in &mut row.statements {
                    statement.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
                }
            }
            _ => unreachable!(),
        }
        row.observed_at = after.context.observed_from.clone();
        let key = row.key().unwrap();
        after.rows[1].key = key;
        let report = after.run(&before);
        assert!(report.comparison.unwrap().additional_contexts.is_empty(), "{changed}");
        assert_eq!(report.entries.len(), 2, "measured bytes survive without an invented variant cause");
    }
}

#[test]
fn a_context_already_present_before_does_not_become_additional_when_its_plan_is_recompiled() {
    let (mut before, mut after) = pair();
    let mut earlier_variant = after.rows[1].clone();
    native(&mut earlier_variant).plan_handle = "0x0708".into();
    for statement in &mut native(&mut earlier_variant).statements {
        statement.context.identity.plan_handle = "0x0708".into();
    }
    before = Frame::new(vec![before.rows[0].clone(), earlier_variant], &before.context.observed_from);
    native(&mut after.rows[1]).lookup_count = "0".into();
    let comparison = after.run(&before).comparison.unwrap();
    assert_eq!((comparison.entries_before, comparison.entries_after), (2, 2));
    assert!(comparison.additional_contexts.is_empty());
}

#[test]
fn exact_selected_totals_can_exceed_u64_without_duplicate_statement_charges_or_binary64_rounding() {
    let (before, after) = pair();
    let mut second = observation("0x0708", Some("1"));
    native(&mut second).object_type = "Prepared".into();
    let mut before_rows = vec![before.rows[0].clone(), second.clone()];
    let mut after_rows = vec![after.rows[0].clone(), second, after.rows[1].clone()];
    for row in before_rows.iter_mut().chain(&mut after_rows) {
        native(row).size_bytes = i64::MAX.to_string();
    }
    let before = Frame::new(before_rows, &before.context.observed_from);
    let after = Frame::new(after_rows, &after.context.observed_from);
    let comparison = after.run(&before).comparison.unwrap();
    assert_eq!(comparison.bytes_before, ((i64::MAX as u128) * 2).to_string());
    assert_eq!(comparison.bytes_after, ((i64::MAX as u128) * 3).to_string());
    assert_eq!(comparison.retained_entries[0].metrics[0].text.as_deref(), Some("0"));
}

#[test]
fn prepared_lookup_one_remains_an_unknown_execution_and_reuse_count_in_every_language() {
    let (mut before, mut after) = pair();
    native(&mut before.rows[0]).lookup_count = "1".into();
    native(&mut after.rows[0]).lookup_count = "1".into();
    for language in ["en", "es", "fr", "de"] {
        culprit_core::i18n::scope(language, || {
            let report = after.run(&before);
            let comparison = report.comparison.unwrap();
            assert_eq!(comparison.retained_entries[0].metrics[1].text.as_deref(), Some("0"));
            assert!(comparison.limitations.contains(&t!("view.cache_entry_memory.limit.lookups")));
            assert!(report.limitations.contains(&t!("view.cache_entry_memory.limit.memory")));
            assert!(report.entries.iter().all(|entry| !entry.investigation.is_empty() && !entry.verify.is_empty()));
        });
    }
}
