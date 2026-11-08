//! Changes MySQL and MariaDB date themselves: a table's CREATE_TIME moves whenever ALTER TABLE rebuilds or changes it
//! (adding an index or a column included), routines and triggers keep when they were created or altered, and MySQL 8
//! records when each global setting was last SET. Catalog and performance_schema only.

use culprit_core::changes::epoch;
use culprit_core::{ChangeEvent, EventAction, EventKind, EventSource, Result, t};
use mysql_async::{Conn, Value};

use crate::rows::{query, query_with};

const MAX_ROWS: usize = 300;

fn utc(column: &str) -> String {
    format!("DATE_FORMAT({column}, '%Y-%m-%dT%H:%i:%sZ')")
}

/// Changes to `database` since `since`. The session has no default database, so these reads never count as the
/// application's workload where the server keeps no statement text to tell them apart (MySQL 5.7, MariaDB).
pub async fn changes(conn: &mut Conn, database: &str, since: &str) -> Result<Vec<ChangeEvent>> {
    // The session runs in UTC, so TIMESTAMP columns compare and print in UTC.
    let Some(since) = culprit_core::changes::sql_timestamp(since) else { return Ok(Vec::new()) };
    let schema = || vec![Value::from(database)];
    let mut out = Vec::new();
    let tables = query_with(
        conn,
        &format!(
            "SELECT TABLE_SCHEMA AS s, TABLE_NAME AS n, TABLE_TYPE AS kind, {} AS at FROM information_schema.TABLES
             WHERE TABLE_SCHEMA = ? AND CREATE_TIME > '{since}' ORDER BY CREATE_TIME DESC LIMIT {MAX_ROWS}",
            utc("CREATE_TIME")
        ),
        schema(),
    )
    .await?;
    for r in tables {
        let (Some(at), table) = (r.text("at"), format!("{}.{}", r.string("s"), r.string("n"))) else { continue };
        let view = r.string("kind").eq_ignore_ascii_case("VIEW");
        let event = if view {
            ChangeEvent::new(EventKind::Code, EventSource::Database, &table, &at, table_title(&table, true))
                .object(table)
        } else {
            ChangeEvent::new(EventKind::Table, EventSource::Database, &table, &at, table_title(&table, false))
                .table(table)
        };
        out.push(event.action(EventAction::Changed));
    }
    let routines = query_with(
        conn,
        &format!(
            "SELECT ROUTINE_SCHEMA AS s, ROUTINE_NAME AS n, ROUTINE_TYPE AS kind, {} AS created, {} AS altered
             FROM information_schema.ROUTINES WHERE ROUTINE_SCHEMA = ? AND LAST_ALTERED > '{since}'
             ORDER BY LAST_ALTERED DESC LIMIT {MAX_ROWS}",
            utc("CREATED"),
            utc("LAST_ALTERED")
        ),
        schema(),
    )
    .await
    .unwrap_or_default();
    for r in routines {
        let name = format!("{}.{}", r.string("s"), r.string("n"));
        let procedure = r.string("kind").eq_ignore_ascii_case("PROCEDURE");
        let (created, altered) = (r.text("created"), r.text("altered"));
        let new = created
            .as_deref()
            .and_then(epoch)
            .zip(altered.as_deref().and_then(epoch))
            .is_some_and(|(c, a)| a - c < 2.0);
        let (Some(at), action) = (altered.or(created), if new { EventAction::Created } else { EventAction::Changed })
        else {
            continue;
        };
        out.push(
            ChangeEvent::new(EventKind::Code, EventSource::Database, &name, &at, routine_title(&name, procedure, new))
                .action(action)
                .object(name),
        );
    }
    let triggers = query_with(
        conn,
        &format!(
            "SELECT TRIGGER_NAME AS n, EVENT_OBJECT_SCHEMA AS s, EVENT_OBJECT_TABLE AS t, {} AS at
             FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = ? AND CREATED > '{since}' LIMIT {MAX_ROWS}",
            utc("CREATED")
        ),
        schema(),
    )
    .await
    .unwrap_or_default();
    for r in triggers {
        let (Some(at), table, name) = (r.text("at"), format!("{}.{}", r.string("s"), r.string("t")), r.string("n"))
        else {
            continue;
        };
        out.push(
            ChangeEvent::new(EventKind::Code, EventSource::Database, &name, &at, trigger_title(&name, &table))
                .action(EventAction::Created)
                .table(table)
                .object(name),
        );
    }
    // MySQL 8 dates every runtime SET; MariaDB has no such table. These only date setting changes the scans saw.
    let settings = query(
        conn,
        &format!(
            "SELECT VARIABLE_NAME AS n, {} AS at FROM performance_schema.variables_info
             WHERE VARIABLE_SOURCE IN ('DYNAMIC', 'PERSISTED') AND SET_TIME > '{since}' LIMIT {MAX_ROWS}",
            utc("SET_TIME")
        ),
    )
    .await
    .unwrap_or_default();
    for r in settings {
        let (Some(at), name) = (r.text("at"), r.string("n")) else { continue };
        out.push(
            ChangeEvent::new(EventKind::Setting, EventSource::Database, &name, &at, setting_title(&name))
                .action(EventAction::Changed)
                .object(name),
        );
    }
    Ok(out)
}

/// A table (or `view`) whose CREATE_TIME moved.
fn table_title(table: &str, view: bool) -> String {
    if view { t!("provider.mysql.change.view", view = table) } else { t!("provider.mysql.change.table", table = table) }
}

/// A stored procedure (or function) that was created (`new`) or altered.
fn routine_title(name: &str, procedure: bool, new: bool) -> String {
    match (procedure, new) {
        (true, true) => t!("provider.mysql.change.procedure_created", name = name),
        (true, false) => t!("provider.mysql.change.procedure_altered", name = name),
        (false, true) => t!("provider.mysql.change.function_created", name = name),
        (false, false) => t!("provider.mysql.change.function_altered", name = name),
    }
}

fn trigger_title(name: &str, table: &str) -> String {
    t!("provider.mysql.change.trigger", name = name, table = table)
}

fn setting_title(name: &str) -> String {
    t!("provider.mysql.change.setting", name = name)
}

#[cfg(test)]
mod tests {
    use super::*;

    // What a recorded change says, word for word: its messages come from locales/en.json.
    #[test]
    fn recorded_changes_say_what_changed_in_english() {
        assert_eq!(
            table_title("shop.orders", false),
            "Table shop.orders was created or altered (its columns or indexes)"
        );
        assert_eq!(table_title("shop.open_orders", true), "View shop.open_orders was created or altered");
        assert_eq!(routine_title("shop.archive", true, true), "Procedure shop.archive was created");
        assert_eq!(routine_title("shop.archive", true, false), "Procedure shop.archive was altered");
        assert_eq!(routine_title("shop.total", false, true), "Function shop.total was created");
        assert_eq!(routine_title("shop.total", false, false), "Function shop.total was altered");
        assert_eq!(trigger_title("orders_audit", "shop.orders"), "Trigger orders_audit on shop.orders was created");
        assert_eq!(setting_title("max_connections"), "max_connections was set");
    }
}
