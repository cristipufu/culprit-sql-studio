//! Native family facts use the existing referenced-definition source and exact original child lifetime.
use super::*;
use crate::{
    query_store::tests::{definition_row, runtime_row, summary_row, variant_definition_row},
    read::ReadResult,
    resources::test_support::{FakeRead, replace},
    telemetry::{
        Source,
        history_tests::{description_rows, pressure, request},
    },
    value::Val,
};
use culprit_core::telemetry::{SourceShutdown, TelemetrySource};

fn with_fields(base: &Rec, extra: &[(&str, Val)]) -> Rec {
    let mut fields = base
        .columns()
        .filter(|(name, _)| !extra.iter().any(|(key, _)| key == name))
        .map(|(name, value)| (name, value.clone()))
        .collect::<Vec<_>>();
    fields.extend(extra.iter().cloned());
    Rec::of(&fields)
}

fn reference() -> Rec {
    let identity = summary_row();
    let fields = identity
        .columns()
        .filter(|(name, _)| {
            matches!(
                *name,
                "query_id"
                    | "context_settings_id"
                    | "object_id"
                    | "query_hash"
                    | "query_initial_compile_at"
                    | "definition_plan_id"
                    | "plan_hash"
                    | "plan_initial_compile_at"
            )
        })
        .map(|(name, value)| (name, value.clone()))
        .collect::<Vec<_>>();
    with_fields(&replace(&runtime_row(2000), &[("replica_group_id", Val::Int(1))]), &fields)
}

fn lengths(row: &Rec) -> Rec {
    Rec::of(
        &row.columns()
            .filter(|(name, _)| matches!(*name, "id" | "source_bytes"))
            .map(|(name, value)| (name, value.clone()))
            .collect::<Vec<_>>(),
    )
}

fn replies(query: Rec, availability: i64) -> Vec<ReadResult<Vec<Rec>>> {
    let mut replies = description_rows();
    let Ok(settings) = &mut replies[2] else { panic!("native settings") };
    settings[0] = replace(&settings[0], &[("variant_families", Val::Int(availability))]);
    replies[6] = Ok(vec![Rec::of(&[("plan_id", Val::Int(4))])]);
    replies.extend([
        Ok(vec![pressure()]),
        Ok(vec![reference()]),
        Ok(vec![pressure()]),
        Ok(vec![]),
        Ok(vec![pressure()]),
        Ok(vec![lengths(&query)]),
        Ok(vec![query]),
    ]);
    replies
}

async fn referenced_source(query: Rec, availability: i64) -> (Source<FakeRead>, CollectionDescription) {
    let mut source =
        Source::with_io(FakeRead::new(replies(query, availability)), &request(), "fixture.invalid".into(), 1433)
            .unwrap();
    let description = source.describe().await.unwrap();
    let runtime = source.read_page(TelemetryStream::QueryRuntime, None).await.unwrap();
    assert!(matches!(runtime,CollectionRead::Page{records,next_cursor:None} if records.len()==1));
    let waits = source.read_page(TelemetryStream::QueryWaits, None).await.unwrap();
    assert!(matches!(waits,CollectionRead::Page{records,next_cursor:None} if records.is_empty()));
    (source, description)
}

#[tokio::test]
async fn family_is_exported_with_the_original_child_definition_without_parent_runtime_or_an_extra_read() {
    let (mut source, description) = referenced_source(variant_definition_row("SELECT N'private-value'"), 1).await;
    assert_eq!(description.query_history().unwrap().native_properties["variant_families"], "native_view_readable");
    let CollectionRead::Page { records, next_cursor: None } =
        source.read_page(TelemetryStream::QueryDefinitions, None).await.unwrap()
    else {
        panic!("complete bounded query dictionary")
    };
    assert_eq!(records.len(), 1, "family metadata must not add a parent query definition");
    let TelemetryRecord::QueryDefinition(definition) = &records[0] else { panic!("query definition") };
    assert!(definition.is_valid());
    let family = definition.variant_family.as_ref().unwrap();
    assert_eq!(family.variant, definition.identity);
    assert_eq!(family.parent.query_id, "1");
    assert_eq!(family.dispatcher.plan_id, "2");
    let DefinitionPayload::Available { text, .. } = &definition.sql else { panic!("bounded redacted SQL") };
    assert!(!text.contains("private-value"));
    assert_eq!(source.query_count(), 14, "existing pressure, length and payload reads contain all family metadata");
    let payload = &source.session.calls[13];
    assert!(payload.sql.contains("v.query_variant_query_id=ids.id"));
    assert!(!payload.sql.contains("query_store_runtime_stats"));
    assert!(matches!(&payload.parameters[0],Parameter::Text(ids) if ids=="<ids><id id=\"3\"/></ids>"));
    assert_eq!(source.close().await, SourceShutdown::Quiescent);
}

#[tokio::test]
async fn unsupported_and_unverified_visibility_remain_distinct_and_do_not_query_the_relationship_view() {
    for (availability, property) in [(0, "unsupported_native_version"), (2, "visibility_unverified")] {
        let (mut source, description) = referenced_source(definition_row("SELECT 1"), availability).await;
        assert_eq!(description.query_history().unwrap().native_properties["variant_families"], property);
        let CollectionRead::Page { records, .. } =
            source.read_page(TelemetryStream::QueryDefinitions, None).await.unwrap()
        else {
            panic!("dictionary without unavailable optional metadata")
        };
        assert!(matches!(&records[0],TelemetryRecord::QueryDefinition(query) if query.variant_family.is_none()));
        assert!(!source.session.calls[13].sql.contains("sys.query_store_query_variant"));
        assert_eq!(source.query_count(), 14);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}

#[tokio::test]
async fn recycled_or_incomplete_native_family_stops_before_publishing_a_dictionary_page() {
    for changes in [
        vec![("query_initial_compile_at", Val::Text("2020-01-01T06:00:01Z".into()))],
        vec![("family_variant_id", Val::Int(99))],
        vec![("dispatcher_query_id", Val::Int(99))],
        vec![("dispatcher_definition_plan_id", Val::Null)],
        vec![("parent_query_initial_compile_at", Val::Null)],
    ] {
        let (mut source, _) = referenced_source(replace(&variant_definition_row("SELECT 1"), &changes), 1).await;
        let result = source.read_page(TelemetryStream::QueryDefinitions, None).await.unwrap();
        assert!(matches!(result, CollectionRead::Stopped(CollectionStop::SourceChanged)));
        assert!(source.read_page(TelemetryStream::PlanDefinitions, None).await.is_err());
        assert_eq!(source.query_count(), 14);
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
    }
}
