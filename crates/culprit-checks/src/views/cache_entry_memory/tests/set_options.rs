use super::*;
use crate::views::cache_entry_memory::set_options::select;

fn pair(left: u32, right: u32) -> (OperationalObservation, OperationalObservation) {
    (observation("0x0102", Some(&left.to_string())), observation("0x0506", Some(&right.to_string())))
}

fn entry(row: &OperationalObservation) -> &MssqlCacheEntryMemory {
    let OperationalFact::MssqlCacheEntryMemory(entry) = &row.fact else { panic!() };
    entry
}

fn assert_generic(left: &OperationalObservation, right: &OperationalObservation) {
    let report = explain(&context(), &[left, right], None).unwrap();
    assert_eq!(report.entries.len(), 2);
    assert_eq!(report.context_differences.len(), 1);
    let difference = &report.context_differences[0];
    assert_eq!(
        difference.explanation,
        t!("view.cache_entry_memory.context.difference", attributes = difference.attributes.join(", "))
    );
    assert_eq!(difference.investigation, t!("view.cache_entry_memory.context.investigation"));
}

#[test]
fn every_documented_boolean_uses_native_plan_attribute_bits_and_exact_direction() {
    for (mask, option) in [
        (1, "ANSI_PADDING"),
        (4, "FORCEPLAN"),
        (8, "CONCAT_NULL_YIELDS_NULL"),
        (16, "ANSI_WARNINGS"),
        (32, "ANSI_NULLS"),
        (64, "QUOTED_IDENTIFIER"),
        (128, "ANSI_NULL_DFLT_ON"),
        (256, "ANSI_NULL_DFLT_OFF"),
        (4096, "ARITH_ABORT"),
        (8192, "NUMERIC_ROUNDABORT"),
    ] {
        let (mut left, mut right) = pair(0, mask);
        native(&mut right).size_bytes = "131072".into();
        let left = native(&mut left);
        let right = native(&mut right);
        let (explanation, investigation) = select(left, right).unwrap().render();
        assert_eq!(
            explanation,
            t!(
                "view.cache_entry_memory.context.set_options",
                options = t!("view.cache_entry_memory.context.set_option_off_on", option = option),
                left_handle = "0x0102",
                right_handle = "0x0506",
                left_bytes = "9007199254740993",
                right_bytes = "131072"
            )
        );
        assert_eq!(investigation, t!("view.cache_entry_memory.context.set_options_investigation"));
        let (explanation, _) = select(right, left).unwrap().render();
        assert_eq!(
            explanation,
            t!(
                "view.cache_entry_memory.context.set_options",
                options = t!("view.cache_entry_memory.context.set_option_on_off", option = option),
                left_handle = "0x0506",
                right_handle = "0x0102",
                left_bytes = "131072",
                right_bytes = "9007199254740993"
            )
        );
    }
}

#[test]
fn all_named_options_keep_full_native_pair_references_without_repeating_each_handle() {
    let (mut left, mut right) = pair(0, 1 | 4 | 8 | 16 | 32 | 64 | 128 | 256 | 4096 | 8192);
    let left = native(&mut left);
    let right = native(&mut right);
    left.plan_handle = format!("0x{}", "01".repeat(64));
    right.plan_handle = format!("0x{}", "02".repeat(64));
    let (explanation, _) = select(left, right).unwrap().render();
    assert_eq!(explanation.matches(&left.plan_handle).count(), 1);
    assert_eq!(explanation.matches(&right.plan_handle).count(), 1);
    for option in [
        "ANSI_PADDING",
        "FORCEPLAN",
        "CONCAT_NULL_YIELDS_NULL",
        "ANSI_WARNINGS",
        "ANSI_NULLS",
        "QUOTED_IDENTIFIER",
        "ANSI_NULL_DFLT_ON",
        "ANSI_NULL_DFLT_OFF",
        "ARITH_ABORT",
        "NUMERIC_ROUNDABORT",
    ] {
        assert!(explanation.contains(option), "{option}");
    }
    assert!(explanation.chars().count() < 1024, "all original pair facts fit one brief field");
}

#[test]
fn named_options_keep_pair_links_and_one_exact_whole_footprint_per_multi_statement_object() {
    let (mut left, mut right) = pair(2, 2 | 1 | 4096);
    for row in [&mut left, &mut right] {
        native(row).cache_keys.push(CacheKeyAttribute {
            name: "user_id".into(),
            native_type: Some("int".into()),
            value: Some("1".into()),
        });
    }
    native(&mut right).size_bytes = "131072".into();
    let report = explain(&context(), &[&right, &left], None).unwrap();
    assert_eq!(report.entries.len(), 2);
    assert_eq!(report.context_differences.len(), 1);
    let difference = &report.context_differences[0];
    assert_eq!(difference.attributes, ["set_options"]);
    let rows = [&left, &right];
    let expected_left = rows.iter().find(|row| row.key == difference.left_observation_key).unwrap();
    let expected_right = rows.iter().find(|row| row.key == difference.right_observation_key).unwrap();
    let (explanation, investigation) = select(entry(expected_left), entry(expected_right)).unwrap().render();
    assert_eq!(difference.explanation, explanation);
    assert_eq!(difference.investigation, investigation);
    for (entry, bytes) in [(&left, "9007199254740993"), (&right, "131072")] {
        let rendered = report.entries.iter().find(|rendered| rendered.observation_key == entry.key).unwrap();
        let metrics = rendered
            .metrics
            .iter()
            .filter(|metric| metric.is("view.cache_entry_memory.metric.bytes"))
            .collect::<Vec<_>>();
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].text.as_deref(), Some(bytes));
        assert_eq!(
            rendered
                .metrics
                .iter()
                .find(|metric| metric.is("view.cache_entry_memory.metric.lookups"))
                .unwrap()
                .text
                .as_deref(),
            Some("1")
        );
    }
    assert!(report.limitations.contains(&t!("view.cache_entry_memory.limit.memory")));
    assert!(report.limitations.contains(&t!("view.cache_entry_memory.limit.lookups")));
    assert!(report.limitations.contains(&t!("view.cache_entry_memory.limit.continuity")));
}

#[test]
fn changed_non_setting_or_unknown_bits_keep_the_entire_ordinary_context_explanation() {
    for flag in [2, 512, 1024, 2048, 16384, 32768, 65536, 131072, 262144, 1 << 20] {
        let (left, right) = pair(0, flag | 4096);
        assert_generic(&left, &right);
    }
}

#[test]
fn unknown_non_integer_negative_and_overflow_masks_never_become_setting_advice() {
    let left = observation("0x0102", Some("0"));
    for value in ["-1", "2147483648", "invalid", "4096.0"] {
        assert_generic(&left, &observation("0x0506", Some(value)));
    }
    for kind in ["bigint", "varchar", "INT"] {
        let mut right = observation("0x0506", Some("4096"));
        native(&mut right).cache_keys[0].native_type = Some(kind.into());
        assert_generic(&left, &right);
    }
    let unknown = observation("0x0506", None);
    let report = explain(&context(), &[&left, &unknown], None).unwrap();
    assert!(report.context_differences.is_empty());
    assert!(report.limitations.contains(&t!("view.cache_entry_memory.limit.keys")));
}

#[test]
fn missing_key_rosters_and_other_changed_contexts_keep_generic_advice() {
    for case in ["missing", "changed", "unknown"] {
        let (mut left, mut right) = pair(0, 4096);
        native(&mut left).cache_keys.push(CacheKeyAttribute {
            name: "user_id".into(),
            native_type: Some("int".into()),
            value: Some("1".into()),
        });
        if case != "missing" {
            native(&mut right).cache_keys.push(CacheKeyAttribute {
                name: "user_id".into(),
                native_type: Some("int".into()),
                value: (case != "unknown").then(|| "2".into()),
            });
        }
        if case == "unknown" {
            assert!(explain(&context(), &[&left, &right], None).unwrap().context_differences.is_empty());
        } else {
            assert_generic(&left, &right);
        }
    }
}

#[test]
fn missing_changed_or_ambiguous_original_bodies_never_acquire_named_options() {
    for case in ["restricted", "not-collected", "changed", "ambiguous", "native-bytes"] {
        let (left, mut right) = pair(0, 4096);
        let entry = native(&mut right);
        for statement in &mut entry.statements {
            statement.statement = match case {
                "restricted" => DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted },
                "not-collected" => DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
                "changed" | "ambiguous" => DefinitionPayload::sql("SELECT other_column", Engine::Mssql),
                _ => statement.statement.clone(),
            };
            if case == "native-bytes" {
                statement.context.source_text_bytes = Some("36".into());
            }
        }
        if case == "ambiguous" {
            entry.statements.truncate(1);
            entry.statements[0].statement = DefinitionPayload::sql("SELECT 1", Engine::Mssql);
            let mut duplicate = entry.statements[0].clone();
            duplicate.context.identity.plan_generation = "2".into();
            entry.statements.push(duplicate);
        }
        assert_generic(&left, &right);
    }
}

#[test]
fn native_sql_database_and_byte_ranges_cannot_be_repaired_by_equal_redacted_text_or_hashes() {
    for case in ["sql-handle", "database", "range"] {
        let (left, mut right) = pair(0, 4096);
        let entry = native(&mut right);
        if case == "sql-handle" {
            entry.sql_handle = "0x0708".into();
        }
        for statement in &mut entry.statements {
            match case {
                "sql-handle" => statement.context.identity.sql_handle = "0x0708".into(),
                "database" => statement.context.database_id = 8,
                _ => {
                    statement.context.identity.statement_start_bytes += 2;
                    if statement.context.identity.statement_end_bytes >= 0 {
                        statement.context.identity.statement_end_bytes += 2;
                    }
                }
            }
        }
        right.key = native(&mut right).key().unwrap();
        assert!(explain(&context(), &[&left, &right], None).unwrap().context_differences.is_empty(), "{case}");
    }
}

#[test]
fn identical_set_masks_and_stubs_preserve_variant_and_single_lookup_context_without_a_diagnosis() {
    let (left, right) = pair(4097, 4097);
    let report = explain(&context(), &[&left, &right], None).unwrap();
    assert_eq!(report.entries.len(), 2);
    assert!(report.context_differences.is_empty());
    let mut stub = observation("0x0506", Some("1"));
    native(&mut stub).cache_object_type = "Compiled Plan Stub".into();
    assert_generic(&left, &stub);
    let report = explain(&context(), &[&stub], None).unwrap();
    assert!(report.entries[0].explanation.starts_with(&t!("view.cache_entry_memory.entry.stub")));
    assert!(report.limitations.contains(&t!("view.cache_entry_memory.limit.lookups")));
}
