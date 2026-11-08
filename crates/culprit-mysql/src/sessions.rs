//! Cancel a statement (`KILL QUERY`) or end a connection (`KILL`) from Live. Culprit's own connections are refused.

use culprit_core::{Error, Result, SessionAction, session_number, t};
use mysql_async::Value;
use mysql_async::prelude::Queryable;

use crate::connect::{Connector, Scope, TAG, map_error};
use crate::rows::{query_with, tagged};

pub(crate) async fn interrupt(connector: &Connector, session: &str, action: SessionAction) -> Result<()> {
    let id = session_number(session)?;
    let mut conn = connector.connect(Scope::Server).await?;
    let target = query_with(
        &mut conn,
        "SELECT COMMAND AS command, INFO AS info, ID = CONNECTION_ID() AS own
           FROM information_schema.PROCESSLIST WHERE ID = ?",
        vec![Value::from(id)],
    )
    .await;
    let result = match target {
        Ok(rows) => match rows.first() {
            None => Err(Error::not_found(t!("provider.mysql.session.gone", id = id.to_string()))),
            Some(row) if row.number("own") == Some(1.0) || row.text("info").is_some_and(|i| i.contains(TAG)) => {
                Err(Error::invalid(t!("provider.mysql.session.own", id = id.to_string())))
            }
            Some(row) if matches!(row.string("command").as_str(), "Daemon" | "Binlog Dump" | "Binlog Dump GTID") => {
                Err(Error::invalid(t!("provider.mysql.session.system_thread", id = id.to_string())))
            }
            Some(_) => {
                let statement = match action {
                    SessionAction::Cancel => format!("KILL QUERY {id}"),
                    SessionAction::Terminate => format!("KILL {id}"),
                };
                conn.query_drop(tagged(&statement)).await.map_err(map_error)
            }
        },
        Err(e) => Err(e),
    };
    let _ = conn.disconnect().await;
    result
}
