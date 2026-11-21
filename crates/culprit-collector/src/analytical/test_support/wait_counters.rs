//! One controlled wait corpus exercises original capture/projection and the same scan/archive explanation.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitCounterCase {
    Exact,
    InconsistentSignal,
    NativePartial,
    Empty,
    Denied,
}
impl WaitCounterCase {
    pub const ALL: [Self; 5] = [Self::Exact, Self::InconsistentSignal, Self::NativePartial, Self::Empty, Self::Denied];
}

pub fn wait_counter_evidence(case: WaitCounterCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, mut rows) = rbio_wait_evidence();
    let TelemetryRecord::WaitCounter(template) = &rows[0] else { panic!() };
    let template = template.clone();
    for (name, total, signal) in [("SLEEP_TASK", "2", "0"), ("WRITELOG", "3", "1")] {
        rows.push(TelemetryRecord::WaitCounter(WaitCounterObservation {
            wait_type: name.into(),
            mechanism: None,
            wait_time_ms: total.into(),
            signal_wait_time_ms: signal.into(),
            ..template.clone()
        }));
    }
    if case == WaitCounterCase::InconsistentSignal {
        let TelemetryRecord::WaitCounter(row) = &mut rows[0] else { panic!() };
        row.wait_time_ms = "1".into();
        row.signal_wait_time_ms = "2".into();
    }
    if matches!(case, WaitCounterCase::Empty | WaitCounterCase::Denied) {
        rows.clear();
    }
    let SourceObservation::Snapshot(snapshot) = &mut source.observation else { panic!() };
    snapshot.observed_from = "2020-01-01T12:00:00.000000001Z".into();
    snapshot.observed_until = "2020-01-01T12:00:00.000000009Z".into();
    snapshot.records = rows.len() as u32;
    snapshot.sources = vec![match case {
        WaitCounterCase::Denied => StateSourceCoverage::unavailable(
            culprit_core::source_id::WAITS.into(),
            culprit_core::ErrorCode::PermissionDenied,
            None,
        ),
        _ => StateSourceCoverage::observed(
            culprit_core::source_id::WAITS.into(),
            rows.len() as u32,
            case != WaitCounterCase::NativePartial,
        ),
    }];
    (source, rows)
}

/// Controlled native-shaped source coverage, never a live Azure qualification or source-cost receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolWaitCounterCase {
    GeneralPurpose,
    BusinessCritical,
    Hyperscale,
    UnknownTier,
    VisibilityUnknown,
    Denied,
    NativePartial,
    Empty,
}
impl PoolWaitCounterCase {
    pub const ALL: [Self; 8] = [
        Self::GeneralPurpose,
        Self::BusinessCritical,
        Self::Hyperscale,
        Self::UnknownTier,
        Self::VisibilityUnknown,
        Self::Denied,
        Self::NativePartial,
        Self::Empty,
    ];
}
pub fn pool_wait_counter_evidence(case: PoolWaitCounterCase) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (settings, _) = pool_settings_evidence(PoolSettingsCase::Exact);
    let membership = mssql_pool_settings_membership(&settings).unwrap().unwrap();
    let (mut source, mut rows) = wait_counter_evidence(WaitCounterCase::Exact);
    let mut snapshot = source.observation.snapshot().unwrap().clone();
    let names = ["WRITELOG", "PAGEIOLATCH_SH", "RESOURCE_SEMAPHORE", "SOS_SCHEDULER_YIELD", "THREADPOOL", "SLEEP_TASK"];
    rows.truncate(names.len());
    for (record, name) in rows.iter_mut().zip(names) {
        let TelemetryRecord::WaitCounter(row) = record else { unreachable!() };
        row.wait_type = name.into();
        row.mechanism = culprit_core::Engine::Mssql.spec().native_wait_mechanism(name);
        row.scope = culprit_core::MeasurementScope::Pool;
        row.reset = culprit_core::ResetMarker::Unknown;
    }
    let tier = match case {
        PoolWaitCounterCase::BusinessCritical => Some("Business Critical"),
        PoolWaitCounterCase::Hyperscale => Some("Hyperscale"),
        PoolWaitCounterCase::UnknownTier => None,
        _ => Some("General Purpose"),
    };
    let visibility = match case {
        PoolWaitCounterCase::VisibilityUnknown => None,
        PoolWaitCounterCase::Denied => Some(false),
        _ => Some(true),
    };
    let before = MssqlPoolCurrentPoint {
        pool: membership.elastic_pool.clone().unwrap(),
        member: membership.member.clone(),
        database_id: 7,
        database_name: membership.database.database.clone(),
        engine_edition: 5,
        native_tier: tier.map(str::to_owned),
        server_performance_state: visibility,
        database_writable: true,
        observed_at: "2020-01-01T12:00:00Z".into(),
    };
    let qualified = before.scope_qualification() == MssqlPoolScopeQualification::Qualified;
    if !qualified || case == PoolWaitCounterCase::Empty {
        rows.clear();
    }
    snapshot.records = rows.len() as u32;
    snapshot.sources = vec![if qualified {
        StateSourceCoverage::observed(
            culprit_core::source_id::WAITS.into(),
            rows.len() as u32,
            case != PoolWaitCounterCase::NativePartial,
        )
    } else {
        StateSourceCoverage::unavailable(
            culprit_core::source_id::WAITS.into(),
            if matches!(case, PoolWaitCounterCase::Denied | PoolWaitCounterCase::VisibilityUnknown) {
                culprit_core::ErrorCode::PermissionDenied
            } else {
                culprit_core::ErrorCode::Unsupported
            },
            None,
        )
    }];
    let after = MssqlPoolCurrentPoint { observed_at: snapshot.observed_until.clone(), ..before.clone() };
    source.target = CollectionTarget::Shared(before.pool.clone());
    source.partition = SourcePartition::Target;
    source.source = MSSQL_POOL_CURRENT_SOURCE.into();
    source.continuity = SourceContinuity::Unverified;
    source.observation = SourceObservation::MssqlPoolCurrent {
        snapshot,
        authority: Box::new(MssqlPoolCurrentAuthority { before, after }),
    };
    assert!(source.partition_is_consistent());
    (source, rows)
}
