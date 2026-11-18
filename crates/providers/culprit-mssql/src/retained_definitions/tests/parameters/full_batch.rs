//! The controlled originals reproduce the real 98/314/316 native byte offsets. They establish parser and
//! content-proof behavior; only the separately owned native case establishes server compatibility.
use super::*;
use crate::plan_xml::{CachedInputContext, PlanInputOrigin};
use culprit_core::plan_facts::{PlanParameterComparison, PlanParameterContext};

const PREFIX: &str = "CREATE PROCEDURE dbo.scalar_probe @bucket int AS ";
const FRAGMENT: &str =
    "SELECT SUM(CONVERT(bigint,LEN(r.pad))) AS scalar_original_probe FROM dbo.scalar_rows r WHERE r.bucket=@bucket";

struct Original {
    full: String,
    fragment: String,
    row: Rec,
    source: CollectionDescription,
}
impl Original {
    fn new(prefix: &str, fragment: &str) -> Self {
        let full = format!("{prefix}{fragment}");
        let xml = xml(&full, false);
        let row = replace(
            &row(Some(&full), &xml),
            &[
                ("statement_start_offset", Val::Int((prefix.encode_utf16().count() * 2) as i64)),
                ("statement_end_offset", Val::Int((full.encode_utf16().count() * 2 - 2) as i64)),
            ],
        );
        Self {
            full,
            fragment: fragment.into(),
            row,
            source: crate::plan_xml::test_source(SnapshotDataset::RetainedPlans),
        }
    }
    fn native_context(&self) -> CachedStatementContext {
        crate::cached_statements::context(&self.row).unwrap()
    }
    fn evidence(
        &self,
        full: &str,
        context: &CachedStatementContext,
        origin: PlanInputOrigin<'_>,
    ) -> Option<PlanParameterContext> {
        crate::plan_xml::original_statement_evidence(
            &xml(full, false),
            CachedInputContext { statement: context, module_parameters: None, text: Some(&self.fragment), origin },
        )
        .and_then(|value| value.parameter_context)
    }
    fn captured_evidence(&self, full: &str, context: &CachedStatementContext) -> Option<PlanParameterContext> {
        self.evidence(full, context, PlanInputOrigin::OriginalCapture { source: &self.source })
    }
}
fn escaped(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('"', "&quot;")
}
fn xml(statement: &str, unsupported_use: bool) -> String {
    let access = if unsupported_use {
        r#"<Predicate><ScalarOperator><Identifier><ColumnReference Column="@bucket"/></Identifier></ScalarOperator></Predicate>"#
    } else {
        r#"<SeekPredicates><SeekPredicateNew><SeekKeys><Prefix ScanType="EQ"><RangeColumns><ColumnReference Table="[scalar_rows]" Column="bucket"/></RangeColumns><RangeExpressions><ScalarOperator><Identifier><ColumnReference Column="@bucket"/></Identifier></ScalarOperator></RangeExpressions></Prefix></SeekKeys></SeekPredicateNew></SeekPredicates>"#
    };
    let physical = if unsupported_use { "Clustered Index Scan" } else { "Index Seek" };
    format!(
        r#"<ShowPlanXML><BatchSequence><Batch><Statements><StmtSimple StatementType="SELECT" StatementText="{}" QueryHash="0x0123456789ABCDEF" RetrievedFromCache="true" StatementSubTreeCost="1"><QueryPlan>
<ParameterList><ColumnReference Column="@bucket" ParameterDataType="int" ParameterCompiledValue="(1)" ParameterRuntimeValue="(2)"/></ParameterList>
<RelOp NodeId="7" PhysicalOp="{physical}" LogicalOp="{physical}" Parallel="false" EstimateRows="100" EstimatedRowsRead="100" EstimatedTotalSubtreeCost="1">
<RunTimeInformation><RunTimeCountersPerThread Thread="0" ActualRows="1000" ActualRowsRead="1000" ActualExecutions="1"/></RunTimeInformation>
<IndexScan><Object Database="[app]" Schema="[dbo]" Table="[scalar_rows]" Alias="[r]" Index="[ix_bucket]"/>{access}</IndexScan>
</RelOp></QueryPlan></StmtSimple></Statements></Batch></BatchSequence></ShowPlanXML>"#,
        escaped(statement)
    )
}

#[test]
fn native_98_314_316_full_module_selects_only_its_exact_complete_source_fragment() {
    let original = Original::new(PREFIX, FRAGMENT);
    let context = original.native_context();
    assert_eq!(original.full.chars().count(), 158);
    assert_eq!(original.fragment.chars().count(), 109);
    assert_eq!(context.identity.statement_start_bytes, 98);
    assert_eq!(context.identity.statement_end_bytes, 314);
    assert_eq!(context.source_text_bytes.as_deref(), Some("316"));
    let facts = original.captured_evidence(&original.full, &context).unwrap();
    assert_eq!(facts.parameters[0].comparison, PlanParameterComparison::DifferentRepresentation);
    assert_eq!(facts.parameters[0].integer_values_equal, Some(false));
    assert_eq!(facts.retrieved_from_cache, Some(true));
    let mut to_end = context.clone();
    to_end.identity.statement_end_bytes = -1;
    assert!(original.captured_evidence(&original.full, &to_end).is_some());
    let read = NativeReadContext::from_snapshot(&original.source).unwrap();
    assert!(
        original.evidence(&original.full, &context, PlanInputOrigin::OriginalNative { read: Some(&read) }).is_some()
    );
}

#[test]
fn source_sized_xml_never_rebinds_unknown_native_or_stored_redacted_scope() {
    let original = Original::new(PREFIX, FRAGMENT);
    let context = original.native_context();
    assert!(original.evidence(&original.full, &context, PlanInputOrigin::OriginalNative { read: None }).is_none());
    assert!(original.evidence(&original.full, &context, PlanInputOrigin::StoredRedacted { proof: None }).is_none());
    let mut unknown = original.source.clone();
    unknown.partition = SourcePartition::Target;
    assert!(
        original.evidence(&original.full, &context, PlanInputOrigin::OriginalCapture { source: &unknown }).is_none()
    );
    // Direct original text remains the independent existing path without a new source-size condition.
    assert!(original.evidence(FRAGMENT, &context, PlanInputOrigin::OriginalNative { read: None }).is_some());
}

#[test]
fn partial_wrong_length_and_invalid_native_ranges_cannot_supply_parameter_evidence() {
    let original = Original::new(PREFIX, FRAGMENT);
    let context = original.native_context();
    let mut variants = Vec::new();
    let mut value = context.clone();
    value.source_text_complete = false;
    variants.push(value);
    let mut value = context.clone();
    value.source_text_bytes = Some("318".into());
    variants.push(value);
    let mut value = context.clone();
    value.identity.statement_start_bytes = 99;
    variants.push(value);
    let mut value = context.clone();
    value.identity.statement_end_bytes = 313;
    variants.push(value);
    let mut value = context.clone();
    value.identity.statement_end_bytes = 96;
    variants.push(value);
    let mut value = context.clone();
    value.identity.statement_end_bytes = 318;
    variants.push(value);
    let mut value = context.clone();
    value.identity.statement_start_bytes = 318;
    value.identity.statement_end_bytes = -1;
    variants.push(value);
    let mut value = context.clone();
    value.identity.plan_handle = "0x00".into();
    variants.push(value);
    let mut value = context.clone();
    value.identity.sql_handle = "0x00".into();
    variants.push(value);
    let mut value = context.clone();
    value.query_hash = Some("0xFEDCBA9876543210".into());
    variants.push(value);
    for value in variants {
        assert!(original.captured_evidence(&original.full, &value).is_none());
    }
    assert!(original.captured_evidence(&format!("{} ", original.full), &context).is_none());
    assert!(original.captured_evidence(&original.full[1..], &context).is_none());
}

#[test]
fn same_length_changed_statement_values_identifiers_hints_and_parameters_are_not_equivalent() {
    let fragment = format!("{FRAGMENT} AND r.id=1 AND N'left'=N'left' /*exact*/ OPTION(MAXDOP 1)");
    let original = Original::new(PREFIX, &fragment);
    let context = original.native_context();
    assert!(original.captured_evidence(&original.full, &context).is_some());
    for (before, after) in [
        ("r.id=1", "r.id=2"),
        ("N'left'", "N'lest'"),
        ("r.pad", "r.bad"),
        ("r.pad", "r.Pad"),
        ("/*exact*/", "/*other*/"),
        ("MAXDOP 1", "MAXDOP 2"),
        ("@bucket", "@buckes"),
    ] {
        let changed_fragment = fragment.replace(before, after);
        let changed = format!("{PREFIX}{changed_fragment}");
        assert_eq!(changed.encode_utf16().count(), original.full.encode_utf16().count());
        assert!(original.captured_evidence(&changed, &context).is_none(), "no normalized or hash-only binding");
    }
    let empty = crate::plan_xml::original_statement_evidence(
        &xml(&original.full, false),
        CachedInputContext {
            statement: &context,
            module_parameters: None,
            text: None,
            origin: PlanInputOrigin::OriginalCapture { source: &original.source },
        },
    );
    assert!(empty.and_then(|value| value.parameter_context).is_none());
}

#[test]
fn utf16_offsets_preserve_whole_surrogates_and_refuse_split_native_ranges() {
    let prefix = format!("/*😀*/{PREFIX}");
    let original = Original::new(&prefix, FRAGMENT);
    let context = original.native_context();
    assert!(original.captured_evidence(&original.full, &context).is_some());
    let mut split = context.clone();
    split.identity.statement_start_bytes = 6;
    assert!(split.is_valid());
    assert!(crate::cached_statements::native_statement_from_prefix(&split, Some(&original.full)).text().is_none());
    assert!(original.captured_evidence(&original.full, &split).is_none());
    let fragment = "SELECT N'😀' AS native_value WHERE @bucket=@bucket";
    let original = Original::new(PREFIX, fragment);
    let mut split = original.native_context();
    split.identity.statement_end_bytes =
        ((PREFIX.encode_utf16().count() + "SELECT N'".encode_utf16().count()) * 2) as i32;
    assert!(split.is_valid());
    assert!(crate::cached_statements::native_statement_from_prefix(&split, Some(&original.full)).text().is_none());
    assert!(original.captured_evidence(&original.full, &split).is_none());
}

#[test]
fn full_batch_scalar_proof_survives_redaction_and_original_payload_restore_without_inventing_operator_uses() {
    let mut original = Original::new(PREFIX, FRAGMENT);
    let native_xml = xml(&original.full, true);
    original.row = replace(
        &original.row,
        &[
            ("plan_bytes", Val::Int((native_xml.encode_utf16().count() * 2) as i64)),
            ("query_plan", Val::Text(native_xml.clone())),
        ],
    );
    let definition = decode(&original.row, &limits(), Some(&original.source)).unwrap();
    let proof = definition.parameter_context_proof.as_ref().unwrap();
    assert!(proof.matches_retained(&original.source, &definition));
    assert_eq!(proof.context.parameters[0].integer_values_equal, Some(false));
    assert!(proof.context.issues.contains(&Issue::UnsupportedUse));
    assert!(proof.context.uses.is_empty());
    let native = crate::plan_xml::original_statement_evidence(
        &native_xml,
        CachedInputContext {
            statement: &definition.context,
            module_parameters: definition.module_parameters.as_ref(),
            text: Some(FRAGMENT),
            origin: PlanInputOrigin::OriginalCapture { source: &original.source },
        },
    )
    .unwrap()
    .parameter_context
    .unwrap();
    let restored = crate::plan_xml::parse_retained_definition(&definition, &original.source).unwrap();
    assert_eq!(restored.facts().parameter_context.as_ref(), Some(&native));
    assert!(restored.facts().parameter_context.as_ref().unwrap().issues.contains(&Issue::UnsupportedUse));
    let encoded = serde_json::to_string(&definition).unwrap();
    // Native XML keeps its schema attributes so it remains usable in native viewers; their values are redacted.
    assert!(encoded.contains("ParameterCompiledValue") && encoded.contains("ParameterRuntimeValue"));
    assert!(!encoded.contains("(1)") && !encoded.contains("(2)"));
    let safe_facts = serde_json::to_string(&proof.context).unwrap();
    assert!(!safe_facts.contains("ParameterCompiledValue") && !safe_facts.contains("ParameterRuntimeValue"));
    let mut changed = definition.clone();
    changed.context.identity.plan_generation = "9".into();
    assert!(!changed.parameter_context_proof.as_ref().unwrap().matches_retained(&original.source, &changed));
    assert!(crate::plan_xml::parse_retained_definition(&changed, &original.source).is_err());
}

#[test]
fn duplicate_native_statement_or_query_plan_never_selects_a_parameter_representative() {
    let original = Original::new(PREFIX, FRAGMENT);
    let context = original.native_context();
    let one = xml(&original.full, false);
    let statement = one.split("<Statements>").nth(1).unwrap().split("</Statements>").next().unwrap();
    let duplicate = format!(
        "<ShowPlanXML><BatchSequence><Batch><Statements>{statement}{statement}</Statements></Batch></BatchSequence></ShowPlanXML>"
    );
    let evidence = crate::plan_xml::original_statement_evidence(
        &duplicate,
        CachedInputContext {
            statement: &context,
            module_parameters: None,
            text: Some(FRAGMENT),
            origin: PlanInputOrigin::OriginalCapture { source: &original.source },
        },
    );
    assert!(evidence.is_none());
    let two = one.replace("</QueryPlan>", "</QueryPlan><QueryPlan/>");
    let evidence = crate::plan_xml::original_statement_evidence(
        &two,
        CachedInputContext {
            statement: &context,
            module_parameters: None,
            text: Some(FRAGMENT),
            origin: PlanInputOrigin::OriginalCapture { source: &original.source },
        },
    );
    assert!(evidence.and_then(|value| value.parameter_context).is_none());
}

#[test]
fn full_batch_slice_requires_exact_fragment_bytes_without_even_trivia_normalization() {
    let fragment = format!(" {FRAGMENT} ");
    let original = Original::new(PREFIX, &fragment);
    let context = original.native_context();
    assert!(original.captured_evidence(&original.full, &context).is_some());
    let changed = format!("{PREFIX}  {FRAGMENT}");
    assert_eq!(changed.encode_utf16().count(), original.full.encode_utf16().count());
    assert!(original.captured_evidence(&changed, &context).is_none());
}
