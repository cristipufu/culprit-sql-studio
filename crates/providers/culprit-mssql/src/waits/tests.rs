use super::*;
use crate::{
    resources::test_support::{FakeRead, replace},
    value::{Rec, Val},
};

pub(crate) fn row(wait: &str, ms: i64) -> Rec {
    Rec::of(&[
        ("wait_type", Val::Text(wait.into())),
        ("waiting_tasks_count", Val::Int(3)),
        ("wait_time_ms", Val::Int(ms)),
        ("signal_wait_time_ms", Val::Int(2)),
        ("max_wait_time_ms", Val::Int(7)),
    ])
}

#[tokio::test]
async fn raw_counters_keep_idle_types_exact_integers_scope_and_unknown_reset_evidence() {
    let native = vec![row("WRITELOG", 9_007_199_254_740_993), row("SLEEP_TASK", 10), row("NEW_NATIVE_WAIT", 20)];
    for database in [true, false] {
        let mut reader = FakeRead::new(vec![Ok(native.clone())]);
        let counters = read(&mut reader, database, 500).await.map_err(crate::read::ReadError::into_error).unwrap();
        assert_eq!(counters.len(), 3);
        assert_eq!(counters[0].wait_time_ms, "9007199254740993");
        assert_eq!(counters[0].reset, ResetMarker::Unknown);
        assert_eq!(counters[0].scope, if database { MeasurementScope::Database } else { MeasurementScope::Server });
        let app = totals(&counters);
        assert_eq!(app.len(), 2);
        assert!(!app.contains_key("SLEEP_TASK"));
        assert_eq!(categories(&app)[1].category, "Other");
        assert_eq!(reader.calls[0].limit, 500);
        assert!(reader.calls[0].sql.contains(if database { "sys.dm_db_wait_stats" } else { "sys.dm_os_wait_stats" }));
    }
}

#[tokio::test]
async fn malformed_missing_approximate_negative_or_duplicate_native_values_cannot_become_zeroes() {
    let base = row("WRITELOG", 10);
    let mut invalid = vec![vec![base.clone(), base.clone()]];
    for value in [Val::Null, Val::Int(-1), Val::Float(10.0), Val::Text("10".into())] {
        invalid.push(vec![replace(&base, &[("waiting_tasks_count", value)])]);
    }
    invalid.push(vec![replace(&base, &[("wait_type", Val::Text("".into()))])]);
    for rows in invalid {
        let mut reader = FakeRead::new(vec![Ok(rows)]);
        assert!(read(&mut reader, true, 500).await.is_err());
    }
}

#[tokio::test]
async fn deltas_subtract_exact_counters_before_converting_to_display_numbers() {
    let mut reader = FakeRead::new(vec![
        Ok(vec![row("WRITELOG", 9_007_199_254_740_992)]),
        Ok(vec![row("WRITELOG", 9_007_199_254_740_993)]),
    ]);
    let before = read(&mut reader, true, 500).await.map_err(crate::read::ReadError::into_error).unwrap();
    let after = read(&mut reader, true, 500).await.map_err(crate::read::ReadError::into_error).unwrap();
    assert_eq!(deltas(&before, &after).unwrap()["WRITELOG"], (1.0, 0.0, 0.0));
}

#[tokio::test]
async fn missing_or_regressing_counters_including_idle_waits_invalidate_the_short_sample() {
    let mut reader = FakeRead::new(vec![Ok(vec![row("WRITELOG", 10), row("SLEEP_TASK", 50)])]);
    let before = read(&mut reader, true, 500).await.map_err(crate::read::ReadError::into_error).unwrap();
    let mut absent = before.clone();
    absent.pop();
    assert!(deltas(&before, &absent).is_err());
    for field in 0..4 {
        let mut after = before.clone();
        let idle = &mut after[1];
        let fields = [
            &mut idle.waiting_tasks_count,
            &mut idle.wait_time_ms,
            &mut idle.signal_wait_time_ms,
            &mut idle.max_wait_time_ms,
        ];
        *fields.into_iter().nth(field).unwrap() = "0".into();
        assert!(deltas(&before, &after).is_err());
    }
    let mut scope_changed = before.clone();
    scope_changed[0].scope = MeasurementScope::Server;
    assert!(deltas(&before, &scope_changed).is_err());
}

#[test]
fn invalid_wire_counts_and_reset_markers_are_rejected() {
    let base = WaitCounterObservation {
        source: "waits".into(),
        wait_type: "WRITELOG".into(),
        scope: MeasurementScope::Database,
        reset: ResetMarker::Unknown,
        waiting_tasks_count: "0".into(),
        wait_time_ms: "1".into(),
        signal_wait_time_ms: "0".into(),
        max_wait_time_ms: "1".into(),
    };
    assert!(base.is_valid());
    for value in ["-1", "1.5", "18446744073709551616", "", "NaN"] {
        let mut invalid = base.clone();
        invalid.wait_time_ms = value.into();
        assert!(!invalid.is_valid());
    }
    let mut invalid = base;
    invalid.reset = ResetMarker::At("unknown-time".into());
    assert!(!invalid.is_valid());
}

#[test]
fn categorises_wait_types() {
    assert_eq!(wait_type_category("LCK_M_X"), "Lock");
    assert!(is_lock_wait_type("LCK_M_X"));
    assert!(is_lock_wait_type("lck_m_s"));
    assert!(!is_lock_wait_type("PAGELATCH_EX"));
    assert!(!is_lock_wait_type("EXTENSION_FUTURE"));
    assert_eq!(wait_type_category("PAGEIOLATCH_SH"), "IO");
    assert_eq!(wait_type_category("WRITELOG"), "Log");
    assert_eq!(wait_type_category("HTREPARTITION"), "Parallelism");
    assert_eq!(wait_type_category("BMPBUILD"), "Parallelism");
    assert_eq!(wait_type_category("SOS_SCHEDULER_YIELD"), "CPU");
    assert_eq!(wait_type_category("THREADPOOL"), "Workers");
    assert_eq!(wait_type_category("RESOURCE_SEMAPHORE_QUERY_COMPILE"), "Compilation");
    assert_eq!(wait_type_category("RESOURCE_SEMAPHORE"), "Memory");
    assert_eq!(wait_type_category("CXPACKET"), "Parallelism");
    assert_eq!(wait_type_category("ASYNC_NETWORK_IO"), "Network");
}
