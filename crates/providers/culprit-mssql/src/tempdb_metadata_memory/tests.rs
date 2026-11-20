use super::*;
use crate::{
    instance::test_support::{epoch, metadata},
    resources::test_support::{FakeRead, replace},
    value::Val,
};

pub(crate) fn layout_row() -> Rec {
    Rec::of(&[
        ("metadata_enabled", Val::Int(1)),
        ("observed_at", Val::Text("2020-01-01T06:02:00Z".into())),
        ("database_id", Val::Int(2)),
        ("database_created", Val::Text("2020-01-01T08:00:00.003".into())),
        ("permission", Val::Int(1)),
        ("metadata_visible", Val::Int(1)),
        ("columns_present", Val::Int(6)),
    ])
}

pub(crate) fn row() -> Rec {
    Rec::of(&[
        ("metadata_enabled", Val::Int(1)),
        ("observed_at", Val::Text("2020-01-01T06:02:00.5Z".into())),
        ("database_id", Val::Int(2)),
        ("database_created", Val::Text("2020-01-01T08:00:00.003".into())),
        ("memory_consumer_id", Val::Int(42)),
        ("memory_consumer_type", Val::Int(2)),
        ("memory_consumer_type_desc", Val::Text("VARHEAP".into())),
        ("memory_consumer_desc", Val::Text("Transaction".into())),
        ("allocated_bytes", Val::Int(8192)),
        ("used_bytes", Val::Int(4096)),
    ])
}

async fn owner(edition: i64, major: &str) -> instance::Context {
    let mut io = FakeRead::new(vec![
        Ok(vec![replace(&metadata(), &[("edition", Val::Int(edition)), ("major", Val::Text(major.into()))])]),
        Ok(vec![epoch(100)]),
    ]);
    instance::read(&mut io).await.unwrap().0
}

#[tokio::test]
async fn native_tempdb_consumers_keep_independent_exact_bytes_without_collecting_addresses_or_owners() {
    let native = owner(3, "16").await;
    let pool = replace(
        &row(),
        &[
            ("memory_consumer_id", Val::Int(-7)),
            ("memory_consumer_type", Val::Int(4)),
            ("memory_consumer_type_desc", Val::Text("PGPOOL".into())),
        ],
    );
    let mut io = FakeRead::new(vec![Ok(vec![layout_row()]), Ok(vec![row(), pool])]);
    let read = read(&mut io, &native, 64).await.unwrap().unwrap();
    assert!(read.complete_scope);
    assert_eq!(read.counters.len(), 3);
    assert_eq!(read.counters[0].properties["metadata_enabled"], "1");
    let known = &read.counters[1];
    assert_eq!(known.scope, MeasurementScope::Server);
    assert_eq!(known.identity.qualifiers["database_id"], "2");
    assert_eq!(known.counters[0].value.integer(), Some(8192));
    assert_eq!(known.counters[1].value.integer(), Some(4096));
    assert_eq!(known.properties["observed_at"], "2020-01-01T06:02:00.500000000Z");
    assert!(known.ratios.is_empty());
    let pool = &read.counters[2];
    assert_eq!(pool.identity.id, "-7", "an internal consumer ID is signed, not a session or owner");
    assert_eq!(pool.counters.len(), 1, "PGPOOL does not acquire documented usage semantics");
    assert_eq!(pool.properties["used_bytes_state"], "not-applicable");
    assert_eq!(pool.properties["native_used_bytes"], "4096");
    assert_eq!(io.calls.len(), 2);
    assert_eq!(io.closed, 0, "the existing source retains its one borrowed reader");
    assert!(io.calls.iter().all(|call| call.sql.contains("/* culprit */")));
    assert!(io.calls[1].sql.contains("FROM sys.dm_db_xtp_memory_consumers"));
    assert!(
        io.calls
            .iter()
            .all(|call| call.sql.starts_with("EXEC /* culprit */ sys.sp_executesql N'USE /* culprit */ [tempdb]; "))
    );
    assert!(io.calls.iter().all(|call| call.sql.contains("DB_ID() AS [database_id]")));
    assert!(io.calls.iter().all(|call| call.sql.contains("d.database_id=DB_ID()")));
    assert!(io.calls[1].sql.ends_with(", N'@P1 int', @P1=@P1"));
    assert!(io.calls[1].sql.contains("c.memory_consumer_type<>0"));
    for forbidden in
        ["memory_consumer_address", "SUM(", "GROUP BY", "ORDER BY", "SELECT *", "ALTER ", "USE /* culprit */ [master]"]
    {
        assert!(io.calls.iter().all(|call| !call.sql.contains(forbidden)), "{forbidden}");
    }
}

#[tokio::test]
async fn native_disabled_unknown_unsupported_and_permission_states_never_turn_into_measured_zero() {
    let mut io = FakeRead::new(vec![]);
    assert!(read(&mut io, &owner(8, "16").await, 64).await.unwrap().is_none());
    assert!(io.calls.is_empty(), "MI has no documented feature support");
    assert_eq!(read(&mut io, &owner(3, "15").await, 64).await.err().unwrap().code, ErrorCode::Unsupported);
    assert!(io.calls.is_empty(), "earlier version semantics never start an unsupported projection");
    let native = owner(3, "16").await;
    let mut disabled = FakeRead::new(vec![Ok(vec![replace(&layout_row(), &[("metadata_enabled", Val::Int(0))])])]);
    let disabled_read = read(&mut disabled, &native, 64).await.unwrap().unwrap();
    assert!(disabled_read.complete_scope);
    assert_eq!(disabled_read.counters.len(), 1);
    assert_eq!(disabled_read.counters[0].properties["metadata_enabled"], "0");
    assert_eq!(disabled.calls.len(), 1);
    for field in ["metadata_enabled", "permission", "metadata_visible", "columns_present", "database_id"] {
        let value = match field {
            "metadata_enabled" => Val::Null,
            "permission" | "metadata_visible" => Val::Int(0),
            "columns_present" => Val::Int(5),
            "database_id" => Val::Int(7),
            _ => unreachable!(),
        };
        let mut io = FakeRead::new(vec![Ok(vec![replace(&layout_row(), &[(field, value)])])]);
        if field == "database_id" {
            assert!(read(&mut io, &native, 64).await.is_err(), "{field}");
        } else {
            let observed = read(&mut io, &native, 64).await.unwrap().unwrap();
            assert!(!observed.complete_scope);
            assert_eq!(observed.counters.len(), 1);
            let expected = match field {
                "permission" | "metadata_visible" => Some(ErrorCode::PermissionDenied),
                "columns_present" => Some(ErrorCode::Unsupported),
                _ => None,
            };
            assert_eq!(observed.error_code, expected);
            if field == "metadata_enabled" {
                assert_eq!(observed.counters[0].properties["metadata_enabled_state"], "native-null");
                assert!(!observed.counters[0].properties.contains_key("metadata_enabled"));
            }
        }
        assert_eq!(io.calls.len(), 1, "{field}: no data fallback or retry");
    }
}

#[tokio::test]
async fn native_null_zero_and_unusual_used_values_remain_separate_source_facts() {
    let native = owner(3, "16").await;
    for (allocated, used) in
        [(Val::Null, Val::Int(0)), (Val::Int(0), Val::Null), (Val::Int(0), Val::Int(0)), (Val::Int(1), Val::Int(2))]
    {
        let mut io = FakeRead::new(vec![
            Ok(vec![layout_row()]),
            Ok(vec![replace(&row(), &[("allocated_bytes", allocated.clone()), ("used_bytes", used.clone())])]),
        ]);
        let result = read(&mut io, &native, 64).await.unwrap().unwrap();
        let sample = &result.counters[1];
        assert_eq!(sample.counters.iter().any(|counter| counter.key == "allocated_bytes"), allocated != Val::Null);
        assert_eq!(sample.counters.iter().any(|counter| counter.key == "used_bytes"), used != Val::Null);
        assert_eq!(sample.properties["used_bytes_state"], if used == Val::Null { "native-null" } else { "measured" });
        assert!(sample.ratios.is_empty(), "non-atomic contradictions are not repaired with a ratio");
    }
}

#[tokio::test]
async fn changed_native_db_clock_feature_or_consumer_contract_cannot_publish_a_qualified_counter_set() {
    let native = owner(3, "16").await;
    for (field, value) in [
        ("database_id", Val::Int(7)),
        ("database_created", Val::Text("2020-01-02T08:00:00.003".into())),
        ("observed_at", Val::Text("2020-01-01T06:01:59Z".into())),
        ("observed_at", Val::Text("2020-01-01T06:02:00.5".into())),
        ("metadata_enabled", Val::Int(0)),
        ("memory_consumer_type", Val::Int(0)),
        ("memory_consumer_type", Val::Int(-1)),
        ("memory_consumer_type", Val::Int(i64::from(i32::MAX) + 1)),
        ("memory_consumer_type_desc", Val::Text("HASH".into())),
        ("allocated_bytes", Val::Int(-1)),
        ("used_bytes", Val::Float(0.0)),
    ] {
        let mut io = FakeRead::new(vec![Ok(vec![layout_row()]), Ok(vec![replace(&row(), &[(field, value)])])]);
        assert!(read(&mut io, &native, 64).await.is_err(), "{field}");
        assert_eq!(io.calls.len(), 2);
        assert_eq!(io.closed, 0);
    }
    for duplicate in [true, false] {
        let peer = if duplicate {
            row()
        } else {
            replace(
                &row(),
                &[("memory_consumer_id", Val::Int(43)), ("observed_at", Val::Text("2020-01-01T06:02:00.6Z".into()))],
            )
        };
        let mut io = FakeRead::new(vec![Ok(vec![layout_row()]), Ok(vec![row(), peer])]);
        assert!(read(&mut io, &native, 64).await.is_err(), "one read owns unique consumers and one native point");
    }
}

#[tokio::test]
async fn bounded_overflow_retains_positive_partial_consumers_without_a_complete_population_or_requery() {
    let native = owner(3, "16").await;
    let second = replace(&row(), &[("memory_consumer_id", Val::Int(43))]);
    let mut io = FakeRead::new(vec![Ok(vec![layout_row()]), Ok(vec![row(), second])]);
    let result = read(&mut io, &native, 2).await.unwrap().unwrap();
    assert!(!result.complete_scope);
    assert_eq!(result.counters.len(), 2);
    assert_eq!(io.calls[1].limit, 2);
    assert!(matches!(io.calls[1].parameters[0], Parameter::Integer(2)));
    let mut io = FakeRead::new(vec![Ok(vec![layout_row()]), Ok(vec![])]);
    let empty = read(&mut io, &native, 1000).await.unwrap().unwrap();
    assert_eq!(empty.counters.len(), 1, "the native feature point remains even without consumer rows");
    assert!(empty.complete_scope);
    assert_eq!(io.calls[1].limit, 65);
    let mut io = FakeRead::new(vec![]);
    assert!(read(&mut io, &native, 0).await.is_err());
    assert!(io.calls.is_empty());
}

#[tokio::test]
async fn native_read_failures_remain_on_the_original_owner_without_fallback_or_invented_shutdown() {
    let native = owner(3, "16").await;
    for stop in [CollectionStop::TimedOut, CollectionStop::Cancelled, CollectionStop::CancellationUnconfirmed] {
        let mut io = FakeRead::new(vec![Ok(vec![layout_row()]), Err(ReadError::Stopped(stop))]);
        let error = read(&mut io, &native, 64).await.err().unwrap();
        assert_eq!(CollectionStop::from_error(&error), stop);
        assert_eq!(io.calls.len(), 2);
        assert_eq!(io.closed, 0, "borrowing the reader cannot certify its owner's shutdown");
        assert_eq!(io.close().await, SourceShutdown::Quiescent);
        assert_eq!(io.closed, 1);
    }
}
