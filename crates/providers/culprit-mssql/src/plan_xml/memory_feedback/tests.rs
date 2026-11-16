use super::*;
use culprit_core::{Engine, Plan, PlanKind, telemetry::*};

const HASH: &str = "0x0123456789ABCDEF";
const OTHER_HASH: &str = "0xFEDCBA9876543210";
const RUNTIME: &str = r#"<RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="100" ActualExecutions="1"/></RunTimeInformation>"#;
const DISABLED: &str =
    r#"IsMemoryGrantFeedbackAdjusted="No: Feedback disabled" LastRequestedMemory="9007199254740993""#;

fn statement(hash: &str, metadata: &str, runtime: &str) -> String {
    format!(
        r#"<StmtSimple QueryHash="{hash}" StatementText="SELECT value FROM dbo.items ORDER BY value" StatementSubTreeCost="1">
<QueryPlan>{metadata}<RelOp NodeId="0" PhysicalOp="Sort" LogicalOp="Sort" EstimateRows="100">
{runtime}<Sort/></RelOp></QueryPlan></StmtSimple>"#
    )
}

fn document(statements: &str) -> String {
    format!(
        "<ShowPlanXML><BatchSequence><Batch><Statements>{statements}</Statements></Batch></BatchSequence></ShowPlanXML>"
    )
}

fn plan(attributes: &str, runtime: &str) -> Plan {
    crate::plan_xml::parse(&document(&statement(HASH, &format!("<MemoryGrantInfo {attributes}/>"), runtime))).unwrap()
}

fn feedback(attributes: &str) -> PlanMemoryGrantFeedback {
    plan(attributes, RUNTIME).facts.memory_grant_feedback.unwrap()
}

fn native(xml: &str, hash: &str) -> culprit_core::Result<Plan> {
    let context = CachedStatementContext {
        identity: CachedStatementIdentity {
            plan_handle: "0x0102".into(),
            sql_handle: "0x0304".into(),
            statement_start_bytes: 0,
            statement_end_bytes: -1,
            created_without_offset: "2026-11-16T12:00:00".into(),
            plan_generation: "1".into(),
        },
        database_id: 7,
        query_hash: Some(hash.into()),
        origin: StatementOrigin::Unknown,
        monitoring_template_id: None,
        source_text_bytes: None,
        source_text_complete: false,
    };
    crate::plan_xml::parse_cached_statement(
        xml,
        crate::plan_xml::CachedInputContext {
            statement: &context,
            module_parameters: None,
            text: None,
            origin: crate::plan_xml::PlanInputOrigin::OriginalNative { read: None },
        },
    )
}

#[test]
fn all_native_feedback_states_have_distinct_typed_meaning_without_a_version_guess() {
    for (native, expected) in [
        ("No: First Execution", State::FirstExecution),
        ("No: Accurate Grant", State::AccurateGrant),
        ("No: Feedback disabled", State::DisabledUnstable),
        ("Yes: Adjusting", State::Adjusting),
        ("Yes: Percentile Adjusting", State::PercentileAdjusting),
        ("Yes: Stable", State::Stable),
    ] {
        let parsed = feedback(&format!("IsMemoryGrantFeedbackAdjusted=\"{native}\" LastRequestedMemory=\"2048\""));
        assert!(parsed.is_valid() && parsed.runtime_observed);
        assert_eq!(parsed.state, expected);
        assert_eq!(parsed.previous_grant_kb.as_deref(), Some("2048"));
        assert!(parsed.issues.is_empty());
        assert_eq!(
            PlanMemoryGrantFeedback::disabled_in(&plan(
                &format!("IsMemoryGrantFeedbackAdjusted=\"{native}\""),
                RUNTIME
            ))
            .is_some(),
            expected == State::DisabledUnstable
        );
    }
}

#[test]
fn prior_execution_granted_kilobytes_are_exact_and_do_not_become_current_memory() {
    for amount in ["0", "9007199254740993", "18446744073709551615"] {
        let plan = plan(
            &format!("IsMemoryGrantFeedbackAdjusted=\"No: Feedback disabled\" LastRequestedMemory=\"{amount}\""),
            RUNTIME,
        );
        let feedback = plan.facts().memory_grant_feedback.as_ref().unwrap();
        assert_eq!(feedback.previous_grant_kb.as_deref(), Some(amount));
        assert!(feedback.is_valid());
        assert_eq!(plan.facts().memory_requested_kb, None);
        assert_eq!(plan.facts().memory_granted_kb, None);
        assert_eq!(plan.facts().memory_max_used_kb, None);
        assert_eq!(plan.facts().compile_memory_kb, None);
    }
    assert_eq!(feedback(r#"LastRequestedMemory="000000""#).previous_grant_kb.as_deref(), Some("0"));
}

#[test]
fn absent_unknown_and_malformed_attributes_are_not_interchangeable_or_retained_as_arbitrary_text() {
    for metadata in ["", "<MemoryGrantInfo/>"] {
        let plan = crate::plan_xml::parse(&document(&statement(HASH, metadata, RUNTIME))).unwrap();
        assert!(plan.facts().memory_grant_feedback.is_none());
    }
    let only_count = feedback(r#"LastRequestedMemory="0""#);
    assert_eq!(only_count.state, State::NotRecorded);
    assert_eq!(only_count.previous_grant_kb.as_deref(), Some("0"));
    let unknown = feedback(r#"IsMemoryGrantFeedbackAdjusted="private-future-native-state-canary""#);
    assert_eq!(unknown.state, State::Unrecognized);
    assert!(unknown.issues.is_empty());
    assert!(!serde_json::to_string(&unknown).unwrap().contains("canary"));
    for value in [String::new(), " ".into(), "x".repeat(MAX_NATIVE_STATE_BYTES + 1)] {
        let parsed = feedback(&format!("IsMemoryGrantFeedbackAdjusted=\"{value}\""));
        assert_eq!(parsed.state, State::Unrecognized);
        assert_eq!(parsed.issues, [Issue::InvalidState]);
        assert!(parsed.is_valid());
    }
    for amount in ["", "-1", "+1", "1.5", "1e3", "NaN", "18446744073709551616"] {
        let parsed = feedback(&format!(
            "IsMemoryGrantFeedbackAdjusted=\"No: Feedback disabled\" LastRequestedMemory=\"{amount}\""
        ));
        assert_eq!(parsed.state, State::DisabledUnstable);
        assert_eq!(parsed.previous_grant_kb, None);
        assert_eq!(parsed.issues, [Issue::InvalidPreviousGrant]);
        assert!(parsed.is_valid());
    }
}

#[test]
fn estimated_unexecuted_or_invalid_native_runtime_never_authorizes_a_feedback_mechanism() {
    for runtime in [
        "".into(),
        "<RunTimeInformation/>".into(),
        RUNTIME.replace("ActualExecutions=\"1\"", "ActualExecutions=\"0\""),
        RUNTIME.replace("ActualExecutions=\"1\"", "ActualExecutions=\"0.5\""),
        RUNTIME.replace("ActualExecutions=\"1\"", "ActualExecutions=\"NaN\""),
        RUNTIME.replace("ActualRows=\"100\"", "ActualRows=\"-1\""),
        RUNTIME.replace("ActualRows=\"100\"", "ActualRows=\"NaN\""),
    ] {
        let plan = plan(DISABLED, &runtime);
        let feedback = plan.facts().memory_grant_feedback.as_ref().unwrap();
        assert_eq!(feedback.state, State::DisabledUnstable);
        assert!(!feedback.runtime_observed, "{runtime}");
        assert!(PlanMemoryGrantFeedback::disabled_in(&plan).is_none());
    }
    assert_eq!(plan(DISABLED, "").kind, PlanKind::Estimated);
}

#[test]
fn another_statement_or_nested_query_plan_cannot_lend_runtime_or_feedback_state() {
    let xml = document(&format!(
        "{}{}",
        statement(HASH, &format!("<MemoryGrantInfo {DISABLED}/>"), ""),
        statement(OTHER_HASH, r#"<MemoryGrantInfo IsMemoryGrantFeedbackAdjusted="Yes: Stable"/>"#, RUNTIME)
    ));
    let selected = native(&xml, HASH).unwrap();
    assert!(!selected.facts().memory_grant_feedback.as_ref().unwrap().runtime_observed);
    assert!(PlanMemoryGrantFeedback::disabled_in(&selected).is_none());
    let healthy = native(&xml, OTHER_HASH).unwrap();
    assert_eq!(healthy.facts().memory_grant_feedback.as_ref().unwrap().state, State::Stable);
    assert!(healthy.facts().memory_grant_feedback.as_ref().unwrap().runtime_observed);
    let nested = format!("<InternalInfo><QueryPlan>{RUNTIME}</QueryPlan></InternalInfo>");
    let selected = plan(DISABLED, &nested);
    assert!(!selected.facts().memory_grant_feedback.as_ref().unwrap().runtime_observed);
    assert!(PlanMemoryGrantFeedback::disabled_in(&selected).is_none());
}

#[test]
fn exact_native_statement_selection_refuses_hash_mismatch_and_ambiguous_duplicates() {
    let selected = statement(HASH, &format!("<MemoryGrantInfo {DISABLED}/>"), RUNTIME);
    let xml = document(&selected);
    assert!(PlanMemoryGrantFeedback::disabled_in(&native(&xml, HASH).unwrap()).is_some());
    assert!(native(&xml, OTHER_HASH).is_err());
    assert!(native(&document(&format!("{selected}{selected}")), HASH).is_err());
    // The public native DOM seam also refuses to pick one of several independent statement candidates.
    culprit_core::xml::read(&document(&format!("{selected}{selected}")), |doc| {
        assert!(crate::plan_xml::select_native_statement(&doc.unwrap(), None).is_err());
    });
}

#[test]
fn duplicate_grant_elements_withhold_a_representative_but_preserve_the_plan_and_actual_counters() {
    let xml = document(&statement(
        HASH,
        &format!("<MemoryGrantInfo {DISABLED}/><MemoryGrantInfo IsMemoryGrantFeedbackAdjusted=\"Yes: Stable\"/>"),
        RUNTIME,
    ));
    let plan = native(&xml, HASH).unwrap();
    let feedback = plan.facts().memory_grant_feedback.as_ref().unwrap();
    assert_eq!(feedback.state, State::Unrecognized);
    assert_eq!(feedback.issues, [Issue::AmbiguousMetadata]);
    assert!(!feedback.runtime_observed);
    assert_eq!(feedback.previous_grant_kb, None);
    assert_eq!(plan.root.act_rows, Some(100.0));
    assert_eq!(plan.kind, PlanKind::Actual);
    assert!(PlanMemoryGrantFeedback::disabled_in(&plan).is_none());
}

#[test]
fn redacted_retained_xml_keeps_the_native_verdict_without_parameter_values_or_current_ownership() {
    let xml = document(&statement(
        HASH,
        &format!(
            "<ParameterList><ColumnReference Column=\"@p\" ParameterCompiledValue=\"'private-value-canary'\"/></ParameterList><MemoryGrantInfo {DISABLED}/>"
        ),
        RUNTIME,
    ));
    let redacted = culprit_core::redact::redact_native(&xml, Engine::Mssql);
    let parsed = native(&redacted, HASH).unwrap();
    assert!(PlanMemoryGrantFeedback::disabled_in(&parsed).is_some());
    assert_eq!(
        parsed.facts().memory_grant_feedback.as_ref().unwrap().previous_grant_kb.as_deref(),
        Some("9007199254740993")
    );
    assert!(!redacted.contains("private-value-canary"));
    assert!(parsed.facts().retained.is_none());
    assert!(parsed.facts().execution_protection.is_none());
}
