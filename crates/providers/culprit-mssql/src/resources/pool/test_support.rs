use crate::value::{Rec, Val};

pub(crate) fn identity_row() -> Rec {
    Rec::of(&[
        ("server_name", Val::Text("fixture-server".into())),
        ("database_name", Val::Text("fixture-db".into())),
        ("elastic_pool_visibility", Val::Int(0)),
        ("pool_name", Val::Text("fixture-pool".into())),
        ("incarnation", Val::Text("logical-member".into())),
        ("physical_incarnation", Val::Text("physical-member".into())),
        ("updateability", Val::Text("READ_WRITE".into())),
        ("cpu_limit", Val::Float(4.0)),
        ("dtu_limit", Val::Null),
        ("observed_at", Val::Text("2020-01-01T06:02:00Z".into())),
    ])
}

pub(crate) fn row(at: &str) -> Rec {
    Rec::of(&[
        ("sample_at", Val::Text(at.into())),
        ("avg_cpu_percent", Val::Float(0.0)),
        ("avg_data_io_percent", Val::Null),
        ("avg_log_write_percent", Val::Float(70.0)),
        ("max_worker_percent", Val::Float(35.0)),
        ("max_session_percent", Val::Float(40.0)),
        ("avg_instance_memory_percent", Val::Float(65.0)),
        ("avg_instance_cpu_percent", Val::Float(80.0)),
        ("avg_edtu_percent", Val::Float(0.0)),
        ("instance_vcores", Val::Float(8.0)),
        ("used_storage_mb", Val::Int(512)),
        ("allocated_storage_mb", Val::Int(768)),
        ("storage_limit_mb", Val::Int(1024)),
        ("used_storage_percent", Val::Float(50.0)),
        ("allocated_storage_percent", Val::Float(75.0)),
    ])
}

/// The current-settings fixture uses native-shaped original GUIDs; older resource-history cases stay unchanged.
pub(crate) fn settings_member(at: &str) -> Rec {
    crate::resources::test_support::replace(
        &identity_row(),
        &[
            ("incarnation", Val::Text("11111111-1111-1111-1111-111111111111".into())),
            ("physical_incarnation", Val::Text("22222222-2222-2222-2222-222222222222".into())),
            ("observed_at", Val::Text(at.into())),
        ],
    )
}
pub(crate) fn settings_point(at: &str) -> Rec {
    let extra = [
        ("selected_database_id", Val::Int(7)),
        ("database_id", Val::Int(7)),
        ("min_cpu", Val::Int(0)),
        ("max_cpu", Val::Int(80)),
        ("cap_cpu", Val::Int(100)),
        ("min_memory", Val::Null),
        ("max_memory", Val::Int(75)),
        ("pool_max_io", Val::Int(2048)),
    ];
    let row = settings_member(at);
    Rec::of(
        &row.columns()
            .chain(extra.iter().map(|(name, value)| (*name, value)))
            .map(|(name, value)| (name, value.clone()))
            .collect::<Vec<_>>(),
    )
}
pub(crate) fn settings_layout() -> Vec<Rec> {
    culprit_core::telemetry::MSSQL_POOL_SETTING_NAMES
        .into_iter()
        .map(|name| {
            Rec::of(&[
                ("name", Val::Text(name.into())),
                ("system_type_id", Val::Int(if matches!(name, "min_cpu" | "max_cpu" | "cap_cpu") { 48 } else { 56 })),
            ])
        })
        .collect()
}
