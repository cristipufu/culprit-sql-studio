use super::*;
pub(super) fn request() -> CollectionRequest {
    CollectionRequest {
        subject: CollectionSubject::Shared {
            resource: SharedResourceIdentity {
                engine: culprit_core::Engine::Mssql,
                kind: SharedResourceKind::Instance,
                namespace: NativeIdentity::new("fixture-machine").qualified("kind", "sql-server-network-machine"),
                native: NativeIdentity::new("fixture-machine\\NATIVE").qualified("kind", "sql-server-instance"),
            },
        },
        selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Jobs },
        budget: bounded_budget(&CollectionBudget::upper_limits()),
    }
}
pub(super) fn replies() -> Vec<ReadResult<Vec<Rec>>> {
    vec![
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
        Ok(vec![instance_pressure_row()]),
        Ok(layout_rows()),
        Ok(vec![instance_pressure_row()]),
        Ok(vec![context_row()]),
        Ok(vec![job_row()]),
        Ok(vec![step_row()]),
        Ok(vec![history_row()]),
        Ok(layout_rows()),
        Ok(vec![context_row()]),
        Ok(vec![metadata()]),
        Ok(vec![epoch(100)]),
    ]
}
pub(super) fn layout_rows() -> Vec<Rec> {
    let groups: &[&[(&str, i64)]] = &[
        &[
            ("job_id", 36),
            ("name", 231),
            ("enabled", 48),
            ("start_step_id", 56),
            ("version_number", 56),
            ("date_created", 61),
            ("date_modified", 61),
        ],
        &[("job_id", 36), ("step_id", 56), ("step_name", 231), ("subsystem", 231), ("database_name", 231)],
        &[
            ("instance_id", 56),
            ("job_id", 36),
            ("step_id", 56),
            ("step_name", 231),
            ("run_status", 56),
            ("run_date", 56),
            ("run_time", 56),
            ("run_duration", 56),
            ("sql_message_id", 56),
            ("sql_severity", 56),
            ("retries_attempted", 56),
            ("server", 231),
        ],
        &[
            ("session_id", 56),
            ("job_id", 36),
            ("run_requested_date", 61),
            ("run_requested_source", 231),
            ("queued_date", 61),
            ("start_execution_date", 61),
            ("stop_execution_date", 61),
            ("last_executed_step_id", 56),
            ("last_executed_step_date", 61),
            ("next_scheduled_run_date", 61),
        ],
        &[("session_id", 56), ("agent_start_date", 61)],
        &[("servicename", 231), ("status", 56), ("last_startup_time", 43)],
    ];
    groups
        .iter()
        .enumerate()
        .flat_map(|(kind, fields)| {
            fields.iter().map(move |(column, native_type)| {
                Rec::of(&[
                    ("kind", Val::Int(kind as i64)),
                    ("column_name", Val::Text((*column).into())),
                    ("object_id", Val::Int(100 + kind as i64)),
                    ("is_sysadmin", Val::Int(1)),
                    ("msdb_access", Val::Int(1)),
                    ("object_select", Val::Int(1)),
                    ("service_visibility", Val::Int(1)),
                    ("native_type", Val::Int(*native_type)),
                    ("native_length", Val::Int(256)),
                    ("native_nullable", Val::Int(1)),
                ])
            })
        })
        .collect()
}
const AT: &str = "2020-01-01T06:00:00Z";
const JOB: &str = "aaaaaaaa-1111-2222-3333-bbbbbbbbbbbb";
pub(super) fn context_row() -> Rec {
    Rec::of(&[
        ("observed_at", Val::Text(AT.into())),
        ("session_id", Val::Int(7)),
        ("agent_started_without_offset", Val::Text("2020-01-01T07:59:00.000".into())),
        ("servicename", Val::Text("SQL Server Agent (MSSQLSERVER)".into())),
        ("service_status", Val::Int(4)),
        ("started_with_offset", Val::Text("2020-01-01T07:59:00.000+02:00".into())),
    ])
}
pub(super) fn job_row() -> Rec {
    let mut fields = vec![
        ("observed_at", Val::Text(AT.into())),
        ("job_id", Val::Text(JOB.into())),
        ("name", Val::Text("current job".into())),
        ("enabled", Val::Int(1)),
        ("start_step_id", Val::Int(1)),
        ("version_number", Val::Int(2)),
    ];
    for field in [
        "created_without_offset",
        "modified_without_offset",
        "activity_session_id",
        "requested_without_offset",
        "request_source",
        "queued_without_offset",
        "started_without_offset",
        "stopped_without_offset",
        "last_executed_step_id",
        "last_step_without_offset",
        "next_scheduled_without_offset",
    ] {
        fields.push((field, Val::Null));
    }
    Rec::of(&fields)
}
pub(super) fn step_row() -> Rec {
    Rec::of(&[
        ("observed_at", Val::Text(AT.into())),
        ("job_id", Val::Text(JOB.into())),
        ("step_id", Val::Int(1)),
        ("step_name", Val::Text("current step".into())),
        ("subsystem", Val::Text("TSQL".into())),
        ("database_name", Val::Text(String::new())),
    ])
}
pub(super) fn history_row() -> Rec {
    Rec::of(&[
        ("observed_at", Val::Text(AT.into())),
        ("job_id", Val::Text(JOB.into())),
        ("instance_id", Val::Int(2147483647)),
        ("step_id", Val::Int(1)),
        ("step_name", Val::Text("stored step".into())),
        ("run_status", Val::Int(0)),
        ("run_date", Val::Int(20200101)),
        ("run_time", Val::Int(0)),
        ("run_duration", Val::Int(250101)),
        ("sql_message_id", Val::Int(0)),
        ("sql_severity", Val::Int(0)),
        ("retries_attempted", Val::Int(0)),
        ("server", Val::Text(String::new())),
    ])
}
