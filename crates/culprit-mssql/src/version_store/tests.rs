use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

pub(crate) fn row() -> Rec {
    Rec::of(&[("native_rows", Val::Int(1)), ("known_rows", Val::Int(1)), ("reserved_space_kb", Val::Int(0))])
}
#[tokio::test]
async fn tempdb_store_keeps_native_integer_kibibytes_and_projects_bytes_only_for_display() {
    let mut reader =
        FakeRead::new(vec![Ok(vec![replace(&row(), &[("reserved_space_kb", Val::Int(9_007_199_254_740_993))])])]);
    let sample = read(&mut reader).await.unwrap();
    assert_eq!(sample.value, "9007199254740993");
    assert_eq!(sample.unit, CounterUnit::Kibibytes);
    assert_eq!(sample.nature, CounterNature::Gauge);
    assert_eq!(sample.scope, MeasurementScope::Database);
    assert_eq!(sample.native_object, "sys.dm_tran_version_store_space_usage");
    assert!(sample.is_valid());
    assert!(observation(sample).is_valid());
    let mut reader = FakeRead::new(vec![Ok(vec![replace(&row(), &[("reserved_space_kb", Val::Int(42))])])]);
    assert_eq!(bytes(&read(&mut reader).await.unwrap()), Some(42.0 * 1024.0));
}
#[tokio::test]
async fn an_empty_visible_store_is_zero_but_unknown_contributors_or_missing_results_are_not() {
    let empty =
        replace(&row(), &[("native_rows", Val::Int(0)), ("known_rows", Val::Int(0)), ("reserved_space_kb", Val::Null)]);
    let mut reader = FakeRead::new(vec![Ok(vec![empty])]);
    assert_eq!(read(&mut reader).await.unwrap().value, "0");
    for result in [
        Ok(vec![]),
        Ok(vec![row(), row()]),
        Ok(vec![replace(&row(), &[("known_rows", Val::Int(0))])]),
        Ok(vec![replace(&row(), &[("native_rows", Val::Int(0)), ("known_rows", Val::Int(0))])]),
        Ok(vec![replace(&row(), &[("reserved_space_kb", Val::Null)])]),
        Ok(vec![replace(&row(), &[("reserved_space_kb", Val::Int(-1))])]),
        Err(ReadError::Stopped(CollectionStop::TimedOut)),
    ] {
        let mut reader = FakeRead::new(vec![result]);
        assert!(read(&mut reader).await.is_err());
        assert_eq!(reader.calls.len(), 1);
    }
}
#[tokio::test]
async fn unknown_meaning_or_scope_cannot_be_projected_as_database_store_bytes() {
    let mut reader = FakeRead::new(vec![Ok(vec![row()])]);
    let sample = read(&mut reader).await.unwrap();
    for changed in [
        NativeCounterSample { unit: CounterUnit::Unknown, ..sample.clone() },
        NativeCounterSample { nature: CounterNature::Unknown, unit: CounterUnit::Unknown, ..sample.clone() },
        NativeCounterSample { scope: MeasurementScope::Server, ..sample.clone() },
    ] {
        assert!(bytes(&changed).is_none());
    }
}

#[test]
fn tempdb_context_cannot_measure_adr_versions_or_an_unknown_recovery_configuration() {
    for adr in [None, Some(true), Some(false)] {
        let context = StoreContext { tempdb_bytes: Some(1024.0), persistent: None, adr_enabled: adr };
        assert_eq!(context.tempdb_cleanup_context(), if adr == Some(false) { Some(1024.0) } else { None });
    }
}
