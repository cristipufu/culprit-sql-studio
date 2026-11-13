use crate::value::{Rec, Val};

pub(crate) fn identity_row() -> Rec {
    Rec::of(&[
        ("server_name", Val::Text("fixture-server".into())),
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
