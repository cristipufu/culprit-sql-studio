use super::*;
use crate::progress::test_support::{self as progress, Reader, row};
pub(crate) const AT: &str = progress::AT;
pub(crate) fn catalog() -> Vec<Row> {
    let columns: &[(&str, &[(&str, u32)])] = &[
        (
            "jobs",
            &[
                ("job_id", 23),
                ("proc_schema", 19),
                ("proc_name", 19),
                ("owner", 4096),
                ("scheduled", 16),
                ("schedule_interval", 1186),
                ("max_runtime", 1186),
                ("max_retries", 23),
                ("retry_period", 1186),
                ("fixed_schedule", 16),
                ("check_schema", 19),
                ("check_name", 19),
                ("initial_start", 1184),
                ("next_start", 1184),
                ("hypertable_schema", 19),
                ("hypertable_name", 19),
            ],
        ),
        (
            "job_stats",
            &[
                ("job_id", 23),
                ("job_status", 25),
                ("last_run_status", 25),
                ("last_run_started_at", 1184),
                ("last_successful_finish", 1184),
                ("last_run_duration", 1186),
                ("next_start", 1184),
                ("total_runs", 20),
                ("total_successes", 20),
                ("total_failures", 20),
            ],
        ),
        (
            "job_history",
            &[
                ("id", 20),
                ("job_id", 23),
                ("start_time", 1184),
                ("finish_time", 1184),
                ("succeeded", 16),
                ("pid", 23),
                ("sqlerrcode", 25),
                ("proc_schema", 25),
                ("proc_name", 25),
            ],
        ),
        (
            "job_errors",
            &[
                ("job_id", 23),
                ("start_time", 1184),
                ("finish_time", 1184),
                ("pid", 23),
                ("sqlerrcode", 25),
                ("proc_schema", 25),
                ("proc_name", 25),
            ],
        ),
    ];
    columns
        .iter()
        .enumerate()
        .map(|(index, (name, columns))| {
            let json = serde_json::to_string(
                &columns.iter().map(|(name, oid)| serde_json::json!({"name":name,"oid":oid})).collect::<Vec<_>>(),
            )
            .unwrap();
            row(&[
                ("view_name", Some(name)),
                ("extension_oid", Some("17000")),
                ("extension_version", Some("2.31.0")),
                ("view_oid", Some(&(18000 + index).to_string())),
                ("can_select", Some("t")),
                ("columns", Some(&json)),
                ("role", Some("owned_reader")),
                ("database_owner", Some("f")),
                ("successful_runs_logged", Some("off")),
                ("window_from", Some("2026-11-14T10:00:00.000003Z")),
            ])
        })
        .collect()
}
pub(crate) fn state() -> Row {
    state_with_configuration_check(Some("public"), Some("validate_owned_configuration"))
}
pub(crate) fn state_with_configuration_check(schema: Option<&str>, name: Option<&str>) -> Row {
    row(&[
        ("observed_at", Some(AT)),
        ("job_id", Some("1000")),
        ("proc_schema", Some("public")),
        ("proc_name", Some("refresh_owned")),
        ("owner", Some("owned_role")),
        ("scheduled", Some("t")),
        ("schedule_interval", Some("1 mon")),
        ("max_runtime", Some("01:00:00")),
        ("max_retries", Some("-1")),
        ("retry_period", Some("00:05:00")),
        ("fixed_schedule", Some("t")),
        ("check_schema", schema),
        ("check_name", name),
        ("initial_start", Some("-infinity")),
        ("next_start", Some("infinity")),
        ("hypertable_schema", None),
        ("hypertable_name", None),
        ("stats_present", Some("t")),
        ("stats_job_status", Some("Running")),
        ("stats_last_run_status", Some("Failed")),
        ("stats_last_run_started_at", Some("2026-11-14T09:00:00Z")),
        ("stats_last_successful_finish", None),
        ("stats_last_run_duration", Some("00:01:30.000001")),
        ("stats_next_start", None),
        ("stats_total_runs", Some("9007199254740993")),
        ("stats_total_successes", Some("0")),
        ("stats_total_failures", Some("9007199254740993")),
    ])
}
pub(crate) fn run() -> Row {
    row(&[
        ("observed_at", Some(AT)),
        ("native_run_id", Some("9007199254740993")),
        ("job_id", Some("1000")),
        ("process_id", None),
        ("proc_schema", Some("public")),
        ("proc_name", Some("reported_label")),
        ("started_at", Some("2026-11-15T09:00:00.000002Z")),
        ("finished_at", None),
        ("succeeded", None),
        ("sql_state", Some("53200")),
    ])
}
pub(crate) fn replace(row: &Row, field: &str, value: Option<&str>) -> Row {
    // Row's public text boundary is deliberately used, without changing native decoder behavior.
    let fields = [
        "view_name",
        "extension_oid",
        "extension_version",
        "view_oid",
        "can_select",
        "columns",
        "role",
        "database_owner",
        "successful_runs_logged",
        "window_from",
    ];
    Row::from_fields(
        fields.into_iter().map(|key| (key.into(), if key == field { value.map(str::to_owned) } else { row.text(key) })),
    )
}
pub(crate) fn reader() -> Reader {
    Reader {
        replies: std::collections::VecDeque::from([
            Ok(vec![progress::metadata_with("f", "2026-11-14T00:00:00Z")]),
            Ok(catalog()),
            Ok(vec![state()]),
            Ok(vec![run()]),
            Ok(vec![progress::metadata_with("f", "2026-11-14T00:00:00Z")]),
            Ok(catalog()),
        ]),
        ..Default::default()
    }
}
