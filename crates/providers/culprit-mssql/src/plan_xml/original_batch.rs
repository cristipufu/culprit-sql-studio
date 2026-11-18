//! The original-only guard is exercised directly so stored-redacted's earlier proof-generation refusal
//! cannot hide an accidental widening of the UTF-16 fallback used by other binding consumers.
use super::*;
use culprit_core::telemetry::*;

#[test]
fn full_batch_binding_itself_requires_qualified_original_scope_and_never_rebinds_stored_redacted_text() {
    let prefix = "CREATE PROCEDURE dbo.p @p int AS ";
    let fragment = "SELECT @p AS native_value";
    let full = format!("{prefix}{fragment}");
    let row = crate::resources::test_support::replace(
        &crate::cached_definitions::tests::row(),
        &[
            ("statement_start_offset", crate::value::Val::Int((prefix.encode_utf16().count() * 2) as i64)),
            ("statement_end_offset", crate::value::Val::Int((full.encode_utf16().count() * 2 - 2) as i64)),
            ("source_text_bytes", crate::value::Val::Int((full.encode_utf16().count() * 2) as i64)),
            ("text_complete", crate::value::Val::Int(1)),
            ("text_prefix", crate::value::Val::Text(full.clone())),
        ],
    );
    let native = crate::cached_statements::context(&row).unwrap();
    let source = super::test_source(SnapshotDataset::RetainedPlans);
    assert!(
        input(&native, fragment, PlanInputOrigin::OriginalCapture { source: &source }).matches_original_text(&full)
    );
    assert!(!input(&native, fragment, PlanInputOrigin::StoredRedacted { proof: None }).matches_original_text(&full));
    assert!(!input(&native, fragment, PlanInputOrigin::OriginalNative { read: None }).matches_original_text(&full));
    let read = NativeReadContext::from_snapshot(&source).unwrap();
    assert!(
        input(&native, fragment, PlanInputOrigin::OriginalNative { read: Some(&read) }).matches_original_text(&full)
    );
    let mut unknown = read;
    unknown.partition = SourcePartition::InstanceLocal { member: None, epoch: None };
    assert!(
        !input(&native, fragment, PlanInputOrigin::OriginalNative { read: Some(&unknown) })
            .matches_original_text(&full)
    );
    assert!(
        input(&native, fragment, PlanInputOrigin::StoredRedacted { proof: None }).matches_original_text(fragment),
        "the existing direct path is unchanged"
    );
}

fn input<'a>(native: &'a CachedStatementContext, text: &'a str, origin: PlanInputOrigin<'a>) -> CachedInputContext<'a> {
    CachedInputContext { statement: native, module_parameters: None, text: Some(text), origin }
}
