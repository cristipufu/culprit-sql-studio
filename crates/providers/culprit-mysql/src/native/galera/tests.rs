use super::*;
use crate::{native::error_log::context::tests as instances, read::test_support::Read};
use mysql_async::Value;
pub(crate) const UID: &str = "iFL88RG7XPipmQ5HOnA9iLqJw6w=";
pub(crate) fn resource() -> SharedResourceIdentity {
    instance::resource(Flavor::Mariadb, UID).unwrap()
}
pub(crate) fn context_rows(at: &str) -> Vec<Result<Vec<Rec>>> {
    vec![
        Ok(vec![instances::platform_at("11.8.3-MariaDB", false, at)]),
        Ok(vec![Rec::from_values(vec![
            ("server_uid", Value::from(UID)),
            ("wsrep_on", Value::from("ON")),
            ("observed_at", Value::from(at)),
        ])]),
        Ok(vec![instances::process(10000, 100)]),
    ]
}
pub(crate) fn status_rows() -> Vec<Rec> {
    MARIADB_GALERA_FIELDS
        .into_iter()
        .map(|name| {
            Rec::from_values(vec![
                ("variable_name", Value::from(name)),
                (
                    "value",
                    Value::from(match name {
                        "wsrep_ready" | "wsrep_connected" => "ON",
                        "wsrep_cluster_status" => "Primary",
                        "wsrep_local_state_comment" => "Synced",
                        "wsrep_cluster_state_uuid" | "wsrep_local_state_uuid" | "wsrep_gcomm_uuid" => {
                            "aaaaaaaa-0000-4000-8000-000000000001"
                        }
                        "wsrep_flow_control_paused" => "0.125000",
                        "wsrep_local_cert_failures" => "9007199254740993",
                        "wsrep_local_commits" => "18446744073709551615",
                        "wsrep_local_send_queue" => "0",
                        _ => "3",
                    }),
                ),
            ])
        })
        .collect()
}
pub(crate) fn frame() -> Vec<Result<Vec<Rec>>> {
    let mut replies = context_rows("2026-11-13T10:00:00.123456Z");
    replies.push(Ok(status_rows()));
    replies.extend(context_rows("2026-11-13T10:00:00.123458Z"));
    replies
}
#[tokio::test]
async fn native_mariadb_owner_uses_published_uid_and_process_without_performance_schema() {
    let mut reader = Read::with(context_rows("2026-11-13T10:00:00.123456Z"));
    let context = super::context(&mut reader).await.unwrap();
    assert_eq!(context.instance.resource, resource());
    assert_eq!(context.instance.started_second, 9900);
    assert_eq!(reader.queries.len(), 3);
    assert!(reader.queries[1].0.contains("information_schema.GLOBAL_VARIABLES"));
    assert!(reader.queries[1].0.contains("WSREP_ON"));
    assert!(!reader.queries[1].0.contains("SESSION_VARIABLES"));
    assert!(reader.queries[2].0.contains("information_schema.GLOBAL_STATUS"));
    assert!(reader.queries.iter().all(|(sql, _, _)| !sql.contains("server_uuid")));
    assert_eq!(
        instance::configuration_query(Flavor::Mariadb, "11.8.3-MariaDB"),
        Some("SELECT @@GLOBAL.server_uid AS server_uid")
    );
    assert_eq!(
        instance::configuration_query(Flavor::Mysql, "8.4.7"),
        Some("SELECT @@GLOBAL.server_uuid AS server_uuid")
    );
}
#[tokio::test]
async fn mysql_and_older_mariadb_stop_before_product_specific_configuration() {
    for version in ["8.4.7", "10.11.8-MariaDB", "10.6.18-MariaDB"] {
        let mut reader = Read::with(vec![Ok(vec![instances::platform(version, true)])]);
        assert_eq!(super::context(&mut reader).await.err().unwrap().code, ErrorCode::Unsupported);
        assert_eq!(reader.queries.len(), 1, "{version}");
    }
}
#[tokio::test]
async fn one_fixed_status_read_keeps_exact_gauges_totals_native_null_and_missing_distinct() {
    let mut rows = status_rows();
    rows[0] = Rec::from_values(vec![("variable_name", Value::from("wsrep_ready")), ("value", Value::NULL)]);
    rows.retain(|row| row.text("variable_name").as_deref() != Some("wsrep_local_send_queue"));
    let mut reader = Read::with(vec![Ok(rows)]);
    let status = capture_status(&mut reader).await.unwrap();
    let point = status.point("2026-11-13T10:00:00Z", "2026-11-13T10:00:01Z").unwrap();
    assert_eq!(point.certification_failures.as_deref(), Some("9007199254740993"));
    assert_eq!(point.local_commits.as_deref(), Some("18446744073709551615"));
    assert_eq!(point.ready, None);
    assert!(point.has("wsrep_ready"));
    assert!(!point.has("wsrep_local_send_queue"));
    assert!(!status.complete());
    assert_eq!(reader.queries.len(), 1);
    let (sql, parameters, bound) = &reader.queries[0];
    assert!(sql.starts_with("SHOW GLOBAL STATUS WHERE Variable_name IN ("));
    assert!(parameters.is_empty());
    assert_eq!(*bound, 18);
    for forbidden in [
        "FLUSH",
        "wsrep_provider_options",
        "incoming_addresses",
        "SELECT *",
        "wsrep_flow_control_recv",
        "wsrep_local_recv_queue_avg",
    ] {
        assert!(!sql.contains(forbidden), "{forbidden}");
    }
}
#[test]
fn duplicate_unexpected_malformed_fraction_negative_and_overflow_status_do_not_publish() {
    for (name, value) in [
        ("wsrep_local_recv_queue", "-1"),
        ("wsrep_local_commits", "18446744073709551616"),
        ("wsrep_flow_control_paused", "NaN"),
        ("wsrep_flow_control_paused", "1.1"),
        ("wsrep_ready", "maybe"),
        ("wsrep_local_state", "not-a-native-code"),
    ] {
        let rows = vec![Rec::from_values(vec![("variable_name", Value::from(name)), ("value", Value::from(value))])];
        let status = Status::decode(rows, &MARIADB_GALERA_FIELDS).unwrap();
        assert!(status.point("2026-11-13T10:00:00Z", "2026-11-13T10:00:01Z").is_err(), "{name}:{value}");
    }
    for name in ["wsrep_ready", "foreign"] {
        let rows = (0..2)
            .map(|_| Rec::from_values(vec![("variable_name", Value::from(name)), ("value", Value::from("ON"))]))
            .collect();
        assert!(Status::decode(rows, &MARIADB_GALERA_FIELDS).is_err());
    }
}
#[tokio::test]
async fn ordinary_scan_reuses_the_decoder_without_widening_its_five_variable_read() {
    let rows = status_rows()
        .into_iter()
        .filter(|row| SCAN_FIELDS.contains(&row.text("variable_name").unwrap().as_str()))
        .collect();
    let mut reader = Read::with(vec![Ok(rows)]);
    let scan = super::scan(&mut reader).await.unwrap();
    assert_eq!(scan.ready, Some(true));
    assert_eq!(scan.flow_control_paused, Some(0.125));
    assert_eq!(reader.queries.len(), 1);
    assert_eq!(reader.queries[0].2, 6);
    assert!(!reader.queries[0].0.contains("wsrep_local_commits"));
}
