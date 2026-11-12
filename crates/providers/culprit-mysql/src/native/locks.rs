//! Native lock-view layouts shared by interactive activity and persistent collection.
use crate::connect::Flavor;

pub(crate) struct RowLockLayout {
    pub sql: &'static str,
    pub session_column: &'static str,
}
pub(crate) fn row_waits(flavor: Flavor, version: &str) -> RowLockLayout {
    if flavor == Flavor::Mysql && culprit_core::version_at_least(version, [8, 0, 0]) {
        RowLockLayout { sql: LOCK_WAITS_SQL, session_column: "t.PROCESSLIST_ID" }
    } else {
        RowLockLayout { sql: INNODB_LOCK_WAITS_SQL, session_column: "r.trx_mysql_thread_id" }
    }
}

/// MySQL 8.0+: performance_schema's lock views.
pub(crate) const LOCK_WAITS_SQL: &str =
    "SELECT t.PROCESSLIST_ID AS session, b.PROCESSLIST_ID AS blocking, rl.LOCK_MODE AS mode,
       rl.LOCK_TYPE AS resource, CONCAT(rl.OBJECT_SCHEMA, '.', rl.OBJECT_NAME) AS object, rl.INDEX_NAME AS index_name
  FROM performance_schema.data_lock_waits w
  JOIN performance_schema.data_locks rl ON rl.ENGINE_LOCK_ID = w.REQUESTING_ENGINE_LOCK_ID
  JOIN performance_schema.threads t ON t.THREAD_ID = w.REQUESTING_THREAD_ID
  JOIN performance_schema.threads b ON b.THREAD_ID = w.BLOCKING_THREAD_ID";

/// MariaDB (and MySQL 5.7): InnoDB's own lock views in information_schema, which need PROCESS.
///
/// A transaction that hasn't written yet has no id of its own: MariaDB reports it as trx 0 in INNODB_LOCK_WAITS (and in
/// INNODB_TRX on 10.6+, while 10.5 shows a different internal number there). So the waiter is found by the lock it
/// asked for, which is unique; a blocker reported as trx 0 can't be told apart from other readers and is left out
/// rather than pinned on the wrong session.
pub(crate) const INNODB_LOCK_WAITS_SQL: &str = "SELECT r.trx_mysql_thread_id AS session, b.trx_mysql_thread_id AS blocking,
       l.lock_mode AS mode, l.lock_type AS resource, REPLACE(l.lock_table, '`', '') AS object, l.lock_index AS index_name
  FROM information_schema.INNODB_LOCK_WAITS w
  JOIN information_schema.INNODB_TRX r ON r.trx_requested_lock_id = w.requested_lock_id
  JOIN information_schema.INNODB_TRX b ON b.trx_id = w.blocking_trx_id AND w.blocking_trx_id <> 0
  LEFT JOIN information_schema.INNODB_LOCKS l ON l.lock_id = w.requested_lock_id";

/// sys reports candidate metadata-lock pairs. Keep only exclusive conflicts: compatible readers can appear in the
/// view too. Other lock modes and queue-priority effects remain unresolved rather than naming a false blocker.
pub(crate) const METADATA_LOCK_WAITS_SQL: &str = "SELECT CAST(waiting_pid AS CHAR) AS session,
       CAST(blocking_pid AS CHAR) AS blocking, waiting_lock_type AS mode, 'METADATA' AS resource,
       CONCAT(object_schema, '.', object_name) AS object, NULL AS index_name
  FROM sys.schema_table_lock_waits WHERE waiting_pid <> blocking_pid
   AND (waiting_lock_type = 'EXCLUSIVE' OR blocking_lock_type = 'EXCLUSIVE')";
