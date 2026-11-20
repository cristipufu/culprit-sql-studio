//! Persisted catalog columns only. No event SQL, definer, comments, user data or scheduler execution.
use crate::connect::Flavor;
pub(super) const COLUMNS: &[(&str, bool)] = &[
    ("EVENT_SCHEMA", false),
    ("EVENT_NAME", false),
    ("STATUS", false),
    ("EVENT_TYPE", false),
    ("TIME_ZONE", false),
    ("INTERVAL_VALUE", false),
    ("INTERVAL_FIELD", false),
    ("EXECUTE_AT", true),
    ("STARTS", true),
    ("ENDS", true),
    ("LAST_EXECUTED", true),
    ("CREATED", true),
    ("LAST_ALTERED", true),
];
pub(super) fn layout() -> String {
    let columns = COLUMNS.iter().map(|(name, _)| format!("'{name}'")).collect::<Vec<_>>().join(",");
    format!(
        "SELECT COLUMN_NAME AS column_name,DATA_TYPE AS data_type FROM information_schema.COLUMNS WHERE TABLE_SCHEMA='information_schema' AND TABLE_NAME='EVENTS' AND COLUMN_NAME IN ({columns}) LIMIT {}",
        COLUMNS.len() + 1
    )
}
pub(super) fn configuration(flavor: Flavor) -> String {
    let member = match flavor {
        Flavor::Mysql => "@@GLOBAL.server_uuid AS server_uuid",
        Flavor::Mariadb => "@@GLOBAL.server_uid AS server_uid",
    };
    format!(
        "SELECT {member},@@GLOBAL.event_scheduler AS event_scheduler,@@SESSION.time_zone AS session_time_zone,DATE_FORMAT(UTC_TIMESTAMP(6),'%Y-%m-%dT%H:%i:%s.%fZ') AS observed_at,(EXISTS(SELECT 1 FROM information_schema.USER_PRIVILEGES WHERE GRANTEE=? AND PRIVILEGE_TYPE='EVENT') OR EXISTS(SELECT 1 FROM information_schema.SCHEMA_PRIVILEGES WHERE GRANTEE=? AND TABLE_SCHEMA=? AND PRIVILEGE_TYPE='EVENT')) AS event_visibility"
    )
}
pub(super) fn events(flavor: Flavor) -> String {
    let member = match flavor {
        Flavor::Mysql => "@@GLOBAL.server_uuid",
        Flavor::Mariadb => "@@GLOBAL.server_uid",
    };
    format!(
        "SELECT CONNECTION_ID() AS source_connection_id,{member} AS source_member,@@GLOBAL.read_only AS source_read_only,@@SESSION.time_zone AS source_time_zone,@@GLOBAL.event_scheduler AS source_scheduler,DATE_FORMAT(UTC_TIMESTAMP(6),'%Y-%m-%dT%H:%i:%s.%fZ') AS observed_at,e.EVENT_SCHEMA AS event_schema,e.EVENT_NAME AS event_name,e.STATUS AS status,e.EVENT_TYPE AS event_type,e.TIME_ZONE AS time_zone,CAST(e.INTERVAL_VALUE AS CHAR) AS interval_value,e.INTERVAL_FIELD AS interval_field,DATE_FORMAT(e.EXECUTE_AT,'%Y-%m-%dT%H:%i:%s.%f') AS execute_at,DATE_FORMAT(e.STARTS,'%Y-%m-%dT%H:%i:%s.%f') AS starts,DATE_FORMAT(e.ENDS,'%Y-%m-%dT%H:%i:%s.%f') AS ends,DATE_FORMAT(e.LAST_EXECUTED,'%Y-%m-%dT%H:%i:%s.%f') AS last_started,DATE_FORMAT(e.CREATED,'%Y-%m-%dT%H:%i:%s.%f') AS created,DATE_FORMAT(e.LAST_ALTERED,'%Y-%m-%dT%H:%i:%s.%f') AS last_altered FROM (SELECT 1 AS witness) source_owner LEFT JOIN (SELECT EVENT_SCHEMA,EVENT_NAME,STATUS,EVENT_TYPE,TIME_ZONE,INTERVAL_VALUE,INTERVAL_FIELD,EXECUTE_AT,STARTS,ENDS,LAST_EXECUTED,CREATED,LAST_ALTERED FROM information_schema.EVENTS WHERE EVENT_SCHEMA=? LIMIT ?) e ON 1=1"
    )
}
