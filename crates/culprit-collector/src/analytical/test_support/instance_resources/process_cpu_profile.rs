//! Controlled typed originals for conformance; never native acquisition, CPU load or cost evidence.
use super::*;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessCpuIntervalCase {
    Exact,
    ManyPools,
    Zero,
    Reset,
    Capacity,
    Roster,
    Decreased,
    PartialPools,
    HistoryDenied,
    LinuxIdle,
    UnknownPlatform,
    NullTotal,
    Restart,
}
impl ProcessCpuIntervalCase {
    pub const ALL: [Self; 13] = [
        Self::Exact,
        Self::ManyPools,
        Self::Zero,
        Self::Reset,
        Self::Capacity,
        Self::Roster,
        Self::Decreased,
        Self::PartialPools,
        Self::HistoryDenied,
        Self::LinuxIdle,
        Self::UnknownPlatform,
        Self::NullTotal,
        Self::Restart,
    ];
    pub fn comparable(self) -> bool {
        matches!(
            self,
            Self::Exact | Self::ManyPools | Self::Zero | Self::HistoryDenied | Self::LinuxIdle | Self::UnknownPlatform
        )
    }
}
pub fn process_cpu_interval_evidence(
    case: ProcessCpuIntervalCase,
) -> [(CollectionDescription, Vec<TelemetryRecord>); 2] {
    [point(case, false), point(case, true)]
}
fn point(case: ProcessCpuIntervalCase, later: bool) -> (CollectionDescription, Vec<TelemetryRecord>) {
    let (mut source, _) = instance_resource_evidence(InstanceResourceCase::Exact);
    let CollectionTarget::Shared(owner) = &source.target else { panic!() };
    let id = owner.telemetry_id().unwrap();
    let epoch = digest(if later && case == ProcessCpuIntervalCase::Restart {
        b"restarted-cpu"
    } else {
        b"canonical-cpu-process"
    });
    source.partition = SourcePartition::InstanceLocal { member: Some(owner.clone()), epoch: Some(epoch.clone()) };
    source.source = MSSQL_CPU_PROFILE_SOURCE.into();
    let (from, at, until, ticks) = if later {
        ("2020-01-01T12:00:01Z", "2020-01-01T12:00:02.000000000Z", "2020-01-01T12:00:03Z", "2000")
    } else {
        ("2020-01-01T11:59:59Z", "2020-01-01T12:00:00.000000000Z", "2020-01-01T12:00:01Z", "1000")
    };
    let capacity = if later && case == ProcessCpuIntervalCase::Capacity { "8" } else { "4" };
    let mut rows = Vec::new();
    for (key, name, object, value) in [
        ("host-logical-processors", "cpu_count", "sys.dm_os_sys_info", capacity.parse::<u64>().unwrap()),
        ("visible-online-user-schedulers", "online_schedulers", "sys.dm_os_schedulers", 1),
        ("visible-online-numa-nodes", "numa_nodes", "sys.dm_os_schedulers", 1),
    ] {
        rows.push(TelemetryRecord::Operational(OperationalObservation {
            source: culprit_core::source_id::PROCESSOR_TOPOLOGY.into(),
            key: key.into(),
            fact: OperationalFact::Counter(NativeCounterSample::gauge(
                key,
                value,
                CounterUnit::Count,
                culprit_core::MeasurementScope::Server,
                object,
                name,
            )),
        }));
    }
    let mut totals: NativeCounterSet = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fixtures/process-cpu.sample.json"
    )))
    .unwrap();
    totals.identity.qualifiers.insert("instance_id".into(), id.clone());
    totals.identity.qualifiers.insert("instance_epoch".into(), epoch.clone());
    totals.properties.insert("observed_at".into(), at.into());
    totals.properties.insert("sample_ms_ticks".into(), ticks.into());
    for counter in &mut totals.counters {
        counter.reset = culprit_core::ResetMarker::Generation(epoch.clone());
    }
    rows.push(TelemetryRecord::Operational(
        OperationalObservation::from_counters(culprit_core::source_id::PROCESSOR_TOPOLOGY, totals).unwrap(),
    ));
    let pool_count = if case == ProcessCpuIntervalCase::ManyPools { 12 } else { 2 };
    for pool in 1..=pool_count {
        let native_id = if later && case == ProcessCpuIntervalCase::Roster && pool == 2 { 3 } else { pool };
        let mut sample =
            header(&id, &epoch, at, ticks, MSSQL_CPU_POOL_KIND, MSSQL_CPU_POOL_OBJECT, &native_id.to_string());
        sample.properties.insert("host_logical_processors".into(), capacity.into());
        sample.properties.insert(
            "statistics_started_without_offset".into(),
            if later && case == ProcessCpuIntervalCase::Reset {
                "2020-01-01T11:59:59.0030000"
            } else {
                "2020-01-01T11:00:00.0030000"
            }
            .into(),
        );
        let total = if case == ProcessCpuIntervalCase::Zero {
            1000
        } else if later && !(case == ProcessCpuIntervalCase::ManyPools && pool > 2) {
            if case == ProcessCpuIntervalCase::Decreased { 500 } else { 2000 }
        } else {
            1000
        };
        if !(case == ProcessCpuIntervalCase::NullTotal && pool == 1) {
            sample.counters.push(NativeCounterSample {
                key: "total_cpu_usage_ms".into(),
                native_name: "total_cpu_usage_ms".into(),
                native_object: MSSQL_CPU_POOL_OBJECT.into(),
                native_instance: Some(native_id.to_string()),
                native_type: None,
                value: MeasurementValue::Integer(total.to_string()),
                unit: CounterUnit::Milliseconds,
                nature: CounterNature::Cumulative,
                scope: culprit_core::MeasurementScope::Server,
                reset: culprit_core::ResetMarker::Unknown,
            });
        }
        rows.push(TelemetryRecord::Operational(
            OperationalObservation::from_counters(MSSQL_CPU_POOLS_SOURCE, sample).unwrap(),
        ));
    }
    if case != ProcessCpuIntervalCase::HistoryDenied {
        let mut sample = header(&id, &epoch, at, ticks, MSSQL_CPU_HISTORY_KIND, MSSQL_CPU_HISTORY_OBJECT, "7");
        sample.properties.insert("record_ms_ticks".into(), if later { "1900" } else { "900" }.into());
        sample.properties.insert(
            "host_platform".into(),
            match case {
                ProcessCpuIntervalCase::LinuxIdle => "Linux",
                ProcessCpuIntervalCase::UnknownPlatform => "FutureOS",
                _ => "Windows",
            }
            .into(),
        );
        sample.properties.insert("process_utilization".into(), "10".into());
        sample
            .properties
            .insert("system_idle".into(), if case == ProcessCpuIntervalCase::LinuxIdle { "0" } else { "50" }.into());
        rows.push(TelemetryRecord::Operational(
            OperationalObservation::from_counters(MSSQL_CPU_HISTORY_SOURCE, sample).unwrap(),
        ));
    }
    let SourceObservation::Snapshot(header) = &mut source.observation else { panic!() };
    header.dataset = SnapshotDataset::ProcessCpu;
    header.observed_from = from.into();
    header.observed_until = until.into();
    header.records = rows.len() as u32;
    header.sources = vec![
        StateSourceCoverage::collected(culprit_core::source_id::PROCESSOR_TOPOLOGY.into(), 4),
        StateSourceCoverage::observed(
            MSSQL_CPU_POOLS_SOURCE.into(),
            pool_count as u32,
            case != ProcessCpuIntervalCase::PartialPools,
        ),
        if case == ProcessCpuIntervalCase::HistoryDenied {
            StateSourceCoverage::unavailable(
                MSSQL_CPU_HISTORY_SOURCE.into(),
                culprit_core::ErrorCode::PermissionDenied,
                None,
            )
        } else {
            StateSourceCoverage::collected(MSSQL_CPU_HISTORY_SOURCE.into(), 1)
        },
    ];
    assert!(source.partition_is_consistent());
    for row in &rows {
        let TelemetryRecord::Operational(row) = row else { panic!() };
        assert!(cpu_profile_observation_fits(row, &source));
    }
    (source, rows)
}
fn header(id: &str, epoch: &str, at: &str, ticks: &str, kind: &str, object: &str, native_id: &str) -> NativeCounterSet {
    NativeCounterSet {
        identity: NativeIdentity::new(native_id)
            .qualified("kind", kind)
            .qualified("native_object", object)
            .qualified("instance_id", id)
            .qualified("instance_epoch", epoch),
        scope: culprit_core::MeasurementScope::Server,
        counters: vec![],
        ratios: vec![],
        ratio_gaps: vec![],
        properties: [
            ("observed_at".into(), at.into()),
            ("started_at_ticks".into(), "100".into()),
            ("sample_ms_ticks".into(), ticks.into()),
            ("engine_edition".into(), "3".into()),
            ("major".into(), "16".into()),
        ]
        .into(),
    }
}

/// Controlled structural originals for pure/report consumers. Actual SDK tests authenticate their own Outbox/store receipts.
pub fn process_cpu_interval_originals(case: ProcessCpuIntervalCase) -> [CapturedOperationalRead; 2] {
    process_cpu_interval_evidence(case)
        .into_iter()
        .enumerate()
        .map(|(index, (source, records))| {
            let capture = CaptureIdentity {
                observation_id: format!("controlled-cpu-{index}"),
                revision: Some(CaptureRevision {
                    outbox_id: "controlled-cpu-outbox".into(),
                    owner_epoch: "1".into(),
                    sequence: (index + 1).to_string(),
                }),
            };
            let positions = records
                .iter()
                .enumerate()
                .map(|(ordinal, row)| {
                    CapturedSnapshotPosition::from_export(
                        &ExportedRecord::new(&source, &capture, row.clone()).unwrap(),
                        ordinal as u32,
                    )
                    .unwrap()
                })
                .collect();
            let observations = records
                .into_iter()
                .map(|row| {
                    let TelemetryRecord::Operational(row) = row else { panic!() };
                    row
                })
                .collect();
            let reference = DiagnosticCaptureRef {
                capture,
                manifest_sha256: digest(format!("controlled-cpu-manifest-{index}").as_bytes()),
                source_scope_id: source.captured_range().unwrap().scope_id,
            };
            CapturedOperationalRead::captured(source, reference, positions, observations, true).unwrap()
        })
        .collect::<Vec<_>>()
        .try_into()
        .unwrap()
}
