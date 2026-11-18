//! Independent native compiler and original Query Store definition selections retain their own capture proofs.
use super::*;
use culprit_core::query_store_statement_report::QueryStoreStatementBinding;

pub struct CompilationDefinitionCase {
    pub name: &'static str,
    pub baseline: (CollectionDescription, Vec<TelemetryRecord>),
    pub current: (CollectionDescription, Vec<TelemetryRecord>),
    pub definitions: Option<(CollectionDescription, Vec<TelemetryRecord>)>,
    pub binding: Option<QueryStoreStatementBinding>,
    pub expensive_directive: bool,
}

pub fn compilation_definition_evidence_cases() -> Vec<CompilationDefinitionCase> {
    [
        "matched",
        "no_companion",
        "ordinary_statement",
        "cheap_directive",
        "native_zero",
        "native_large_count",
        "endpoint_alias",
        "missing_namespace",
        "changed_namespace",
        "changed_context",
        "reused_query_number",
        "restricted_sql",
        "monitoring",
        "internal",
        "unknown_origin",
        "short_unicode_sql",
    ]
    .into_iter()
    .map(case)
    .collect()
}

fn case(name: &'static str) -> CompilationDefinitionCase {
    let [mut baseline, mut current] = super::compilation_interval_evidence();
    native(&mut baseline.1[0]).compilations = "10".into();
    native(&mut current.1[0]).compilations = "12".into();
    native(&mut current.1[0]).average_duration_us = 1_000_000.0;
    let mut definitions = definitions(&current);
    let mut binding = Some(QueryStoreStatementBinding::Matched);
    let mut expensive_directive = true;
    match name {
        "matched" => {}
        "no_companion" => {
            binding = None;
            expensive_directive = false;
        }
        "ordinary_statement" => {
            let sql = "SELECT id FROM dbo.original_compile_input WHERE tenant_id=81724";
            let value = definition(&mut definitions.1[0]);
            value.source_text_bytes = Some((sql.encode_utf16().count() * 2).to_string());
            value.sql = DefinitionPayload::sql(sql, culprit_core::Engine::Mssql);
            expensive_directive = false;
        }
        "cheap_directive" => {
            native(&mut current.1[0]).average_duration_us = 1_000.0;
            expensive_directive = false;
        }
        "native_zero" => {
            native(&mut current.1[0]).compilations = "0".into();
            native(&mut current.1[0]).average_duration_us = 0.0;
            expensive_directive = false;
        }
        "native_large_count" => {
            native(&mut current.1[0]).compilations = "9007199254740993".into();
            native(&mut current.1[0]).average_duration_us = 1_000.0;
            expensive_directive = false;
        }
        "endpoint_alias" => {
            database(&mut definitions.0).location = culprit_core::DatabaseLocation::Server {
                host: "another-original-route.invalid".into(),
                port: 1433,
                instance: None,
            };
        }
        "missing_namespace" => {
            database(&mut current.0).incarnation = None;
            binding = Some(QueryStoreStatementBinding::SourceUnverified);
            expensive_directive = false;
        }
        "changed_namespace" => {
            database(&mut definitions.0).incarnation = Some("another-original-native-query-store".into());
            binding = Some(QueryStoreStatementBinding::SourceChanged);
            expensive_directive = false;
        }
        "changed_context" | "reused_query_number" => {
            let identity = &mut definition(&mut definitions.1[0]).identity;
            let (field, value) = if name == "changed_context" {
                ("context_settings_id", "6")
            } else {
                ("initial_compile_at", "2020-01-01T07:00:00Z")
            };
            identity.qualifiers.insert(field.into(), value.into());
            binding = Some(QueryStoreStatementBinding::NotObserved);
            expensive_directive = false;
        }
        "restricted_sql" => {
            definition(&mut definitions.1[0]).sql =
                DefinitionPayload::Unavailable { reason: PayloadUnavailable::Restricted };
            expensive_directive = false;
        }
        "monitoring" => {
            let value = definition(&mut definitions.1[0]);
            value.origin = StatementOrigin::CulpritMonitoring;
            value.monitoring_template_id = Some(digest(b"original-compiler-monitoring-template"));
            expensive_directive = false;
        }
        "internal" | "unknown_origin" => {
            definition(&mut definitions.1[0]).origin =
                if name == "internal" { StatementOrigin::EngineInternal } else { StatementOrigin::Unknown };
            expensive_directive = false;
        }
        "short_unicode_sql" => {
            let sql = format!(
                "SELECT [{}] AS a,[{}] AS b,[{}] AS c,[{}] AS d FROM dbo.orders OPTION(RECOMPILE)",
                "界".repeat(100),
                "界".repeat(100),
                "界".repeat(100),
                "界".repeat(100),
            );
            assert!(sql.chars().count() < 512 && sql.len() > 1024);
            let value = definition(&mut definitions.1[0]);
            value.source_text_bytes = Some((sql.encode_utf16().count() * 2).to_string());
            value.sql = DefinitionPayload::sql(&sql, culprit_core::Engine::Mssql);
        }
        _ => unreachable!(),
    }
    for record in &mut definitions.1 {
        let value = definition(record);
        value.id = value.identity.definition_id(&definitions.0).unwrap();
    }
    CompilationDefinitionCase {
        name,
        baseline,
        current,
        definitions: (name != "no_companion").then_some(definitions),
        binding,
        expensive_directive,
    }
}

fn definitions(
    current: &(CollectionDescription, Vec<TelemetryRecord>),
) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let mut source = current.0.clone();
    source.source = "original-query-store-definitions".into();
    source.observation = SourceObservation::QueryHistory(QueryHistoryDescription {
        acquisition_owner: None,
        interval: TelemetryInterval {
            native_id: "independent-runtime-history".into(),
            from: "2020-01-01T07:00:00Z".into(),
            until: "2020-01-01T08:00:00Z".into(),
        },
        population_catalog: None,
        population_binding: QueryHistoryPopulationBinding::Unknown,
        waits: None,
        dimensions: Default::default(),
        native_properties: Default::default(),
    });
    let OperationalFact::MssqlQueryCompilation(native) = &operational(&current.1[0]).fact else { panic!() };
    let sql = "SELECT id FROM dbo.original_compile_input WHERE tenant_id=81724 OPTION(RECOMPILE)";
    let record = TelemetryRecord::QueryDefinition(QueryDefinition {
        id: native.identity.definition_id(&source).unwrap(),
        identity: native.identity.clone(),
        origin: StatementOrigin::Application,
        monitoring_template_id: None,
        variant_family: None,
        context: [("set_options".into(), "00000000000000FB".into())].into(),
        context_available: true,
        source_text_bytes: Some((sql.encode_utf16().count() * 2).to_string()),
        sql: DefinitionPayload::sql(sql, culprit_core::Engine::Mssql),
    });
    (source, vec![record])
}

fn operational(record: &TelemetryRecord) -> &OperationalObservation {
    let TelemetryRecord::Operational(value) = record else { panic!() };
    value
}
fn native(record: &mut TelemetryRecord) -> &mut MssqlQueryCompilation {
    let TelemetryRecord::Operational(OperationalObservation {
        fact: OperationalFact::MssqlQueryCompilation(value),
        ..
    }) = record
    else {
        panic!()
    };
    value
}
fn definition(record: &mut TelemetryRecord) -> &mut QueryDefinition {
    let TelemetryRecord::QueryDefinition(value) = record else { panic!() };
    value
}
fn database(source: &mut CollectionDescription) -> &mut culprit_core::DatabaseIdentity {
    let CollectionTarget::Database(value) = &mut source.target else { panic!() };
    value
}
