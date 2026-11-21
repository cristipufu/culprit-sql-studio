use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};
pub(crate) fn layout(kind: MssqlNumaNodeKind) -> Vec<Rec> {
    crate::native_counter_fields::all_columns(kind.fields())
}
pub(crate) fn row(kind: MssqlNumaNodeKind) -> Rec {
    Rec::of(
        &kind
            .fields()
            .iter()
            .map(|field| {
                (
                    field.name,
                    match field.kind {
                        MssqlNativeFieldKind::TextProperty => Val::Text("ONLINE DAC".into()),
                        MssqlNativeFieldKind::BooleanProperty => Val::Bool(false),
                        MssqlNativeFieldKind::Identity => Val::Int(0),
                        MssqlNativeFieldKind::IntegerProperty if field.name.ends_with("mask") => Val::Int(i64::MIN),
                        _ => Val::Int(17),
                    },
                )
            })
            .collect::<Vec<_>>(),
    )
}
#[tokio::test]
async fn both_native_populations_use_two_fixed_reads_with_exact_units_signed_masks_and_no_cross_population_join() {
    for kind in MssqlNumaNodeKind::ALL {
        let mut io = FakeRead::new(vec![Ok(layout(kind)), Ok(vec![row(kind)])]);
        let values = read(&mut io, kind, 1).await.unwrap();
        let point = MssqlNumaNodePoint::from_sample(&values[0]).unwrap();
        assert_eq!(point.kind(), kind);
        assert_eq!(io.calls.len(), 2);
        assert!(io.calls.iter().all(|call| call.sql.contains("/* culprit */")));
        assert!(io.calls[1].sql.contains(kind.object()));
        assert!(!io.calls[1].sql.contains("JOIN"));
        assert_eq!(io.calls[1].limit, 2);
        assert!(values[0].counters.iter().all(
            |counter| counter.reset == culprit_core::ResetMarker::Unknown && counter.nature == CounterNature::Gauge
        ));
        if kind == MssqlNumaNodeKind::Cpu {
            assert_eq!(values[0].properties["cpu_affinity_mask"], i64::MIN.to_string());
        }
        assert_eq!(io.close().await, SourceShutdown::Quiescent);
    }
}
#[tokio::test]
async fn absent_columns_native_nulls_and_zero_remain_distinct_and_never_trigger_a_second_reader() {
    let kind = MssqlNumaNodeKind::Memory;
    let mut columns = layout(kind);
    columns.retain(|row| crate::read::text(row, "name").unwrap() != "target_kb");
    let native = replace(
        &row(kind),
        &[("target_kb", Val::Null), ("pages_kb", Val::Null), ("locked_page_allocations_kb", Val::Int(0))],
    );
    let mut io = FakeRead::new(vec![Ok(columns), Ok(vec![native])]);
    let values = read(&mut io, kind, 1).await.unwrap();
    let point = MssqlNumaNodePoint::from_sample(&values[0]).unwrap();
    let field = |name| kind.fields().iter().find(|field| field.name == name).unwrap();
    assert!(matches!(point.field(field("target_kb")).unwrap(), MssqlNativeFieldValue::ColumnUnavailable));
    assert!(matches!(point.field(field("pages_kb")).unwrap(), MssqlNativeFieldValue::NativeNull));
    assert!(matches!(point.field(field("locked_page_allocations_kb")).unwrap(), MssqlNativeFieldValue::Observed("0")));
    assert_eq!(io.calls.len(), 2);
    assert_eq!(io.close().await, SourceShutdown::Quiescent);
}
#[tokio::test]
async fn missing_native_identity_duplicate_ids_overflow_and_malformed_values_refuse_without_fallback() {
    for change in ["identity-layout", "duplicate", "overflow", "negative-count", "wrong-type"] {
        let kind = MssqlNumaNodeKind::Cpu;
        let mut columns = layout(kind);
        let mut rows = vec![row(kind)];
        let limit = if change == "duplicate" { 2 } else { 1 };
        match change {
            "identity-layout" => columns.retain(|row| crate::read::text(row, "name").unwrap() != "node_id"),
            "duplicate" | "overflow" => rows.push(row(kind)),
            "negative-count" => rows[0] = replace(&rows[0], &[("cpu_count", Val::Int(-1))]),
            _ => rows[0] = replace(&rows[0], &[("active_worker_count", Val::Float(1.5))]),
        }
        let mut io = FakeRead::new(vec![Ok(columns), Ok(rows)]);
        assert!(read(&mut io, kind, limit).await.is_err(), "{change}");
        assert!(io.calls.len() <= 2);
        assert_eq!(io.close().await, SourceShutdown::Quiescent);
    }
}
