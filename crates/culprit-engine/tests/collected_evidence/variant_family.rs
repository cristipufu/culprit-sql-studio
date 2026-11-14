use super::*;
use culprit_core::{QueryVariantFamily, collected_analysis::CollectedQueryAnalysis};

fn history(with_family: bool) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (_, mut source, mut records) = recorded();
    records.truncate(2);
    let CollectionTarget::Database(database) = &mut source.target else { panic!("database source") };
    database.engine = culprit_core::Engine::Postgres;
    source.source = "embedding-variant-history".into();
    let SourceObservation::QueryHistory(history) = &mut source.observation else { panic!("query history") };
    history.native_properties.insert(
        "variant_families".into(),
        if with_family { "native_view_readable" } else { "visibility_unverified" }.into(),
    );
    let TelemetryRecord::QueryDefinition(definition) = &mut records[0] else { panic!("definition") };
    definition.id = definition.identity.definition_id(&source).unwrap();
    if with_family {
        let parent = NativeQueryIdentity {
            query_id: "parent-native".into(),
            qualifiers: [("native-generation".into(), "parent-compile".into())].into(),
        };
        definition.variant_family = Some(QueryVariantFamily {
            variant: definition.identity.clone(),
            parent: parent.clone(),
            dispatcher: NativePlanIdentity {
                plan_id: "dispatcher-native".into(),
                query: parent,
                qualifiers: [("native-generation".into(), "dispatcher-compile".into())].into(),
            },
        });
    }
    let reference =
        NativeDefinitionReference { native: definition.identity.native(), definition_id: Some(definition.id.clone()) };
    let TelemetryRecord::QueryRuntime(runtime) = &mut records[1] else { panic!("runtime") };
    runtime.key.references.query = Some(reference);
    (source, records)
}

fn projected(analysis: &CollectedQueryAnalysis) -> &QueryStat {
    let workload = analysis.workload.as_ref().unwrap_or_else(|| panic!("{:?}", analysis.limitations));
    assert_eq!(workload.queries.len(), 1);
    &workload.queries[0]
}

#[tokio::test]
async fn authenticated_native_family_survives_analysis_with_scoped_ids_without_an_engine_switch_or_parent_totals() {
    let (source, records) = history(true);
    let TelemetryRecord::QueryDefinition(definition) = &records[0] else { panic!() };
    let expected_family = definition.variant_family.clone().unwrap();
    let definition_id = definition.id.clone();
    let TelemetryRecord::QueryRuntime(runtime) = &records[1] else { panic!() };
    let executions = runtime.executions.parse::<f64>().unwrap();
    let total = runtime.metric(&RuntimeMetricKey::Duration).unwrap().average * executions;
    let references = runtime.key.references.clone();
    let native_row = runtime.key.native.clone();
    let fixture = Fixture::new(source.clone(), records).await;
    let capture = fixture.read().await;
    assert!(capture.is_read_complete(), "{:?}", capture.gaps());
    let analysis = collected_evidence::analyze(&capture).unwrap();
    let query = projected(&analysis);
    assert_eq!(analysis.context.query_inputs.len(), 1);
    let input = &analysis.context.query_inputs[0];
    assert_eq!(input.query_id, query.id);
    assert_eq!(input.references, references);
    assert_eq!(input.references.query.as_ref().unwrap().definition_id.as_deref(), Some(definition_id.as_str()));
    assert_eq!(input.native_rows, [native_row]);
    assert_ne!(query.id, definition_id, "query groups retain their own authenticated scope");
    assert_ne!(query.id, expected_family.variant.query_id, "public query groups cannot become numeric/native aliases");
    assert_eq!(query.variant_family.as_ref(), Some(&expected_family));
    assert_eq!(query.executions, executions);
    assert!((query.total_time_ms - total).abs() < 1e-8);
    let workload = analysis.workload.as_ref().unwrap();
    assert_eq!(workload.total_executions, executions);
    assert!((workload.total_time_ms - total).abs() < 1e-8);
    assert_eq!(workload.query_count, 1.0);
    assert!(query.plans.is_empty(), "a dispatcher identity cannot manufacture measured plan variants");
    assert_eq!(analysis.context.evidence.source, source);
    assert!(analysis.context.evidence.records.iter().any(|record|matches!(&record.row.record,TelemetryRecord::QueryDefinition(value) if value.variant_family.as_ref()==Some(&expected_family))));
}

#[tokio::test]
async fn unverified_visibility_keeps_raw_capability_and_positive_runtime_without_inventing_a_family() {
    let (source, records) = history(false);
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    let analysis = collected_evidence::analyze(&capture).unwrap();
    assert!(projected(&analysis).variant_family.is_none());
    assert!(projected(&analysis).executions > 0.0);
    assert_eq!(
        analysis.context.evidence.source.query_history().unwrap().native_properties["variant_families"],
        "visibility_unverified"
    );
}

#[tokio::test]
async fn a_parent_definition_with_identical_sql_cannot_replace_the_missing_child_definition() {
    let (source, mut records) = history(true);
    let TelemetryRecord::QueryDefinition(child) = records.remove(0) else { panic!() };
    let parent = child.variant_family.as_ref().unwrap().parent.clone();
    records.push(TelemetryRecord::QueryDefinition(QueryDefinition {
        id: parent.definition_id(&source).unwrap(),
        identity: parent,
        variant_family: None,
        ..child
    }));
    let fixture = Fixture::new(source, records).await;
    let capture = fixture.read().await;
    assert!(capture.is_read_complete(), "all provided native rows were retrieved");
    let analysis = collected_evidence::analyze(&capture).unwrap();
    assert!(analysis.workload.is_none());
    assert!(analysis.limitations.iter().any(|limit| limit.reason == CollectedAnalysisLimit::MissingQueryDefinition));
    assert!(
        analysis
            .context
            .evidence
            .records
            .iter()
            .any(|record| matches!(&record.row.record, TelemetryRecord::QueryRuntime(_)))
    );
}
