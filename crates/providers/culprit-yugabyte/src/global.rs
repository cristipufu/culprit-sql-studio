//! Optional native global views are read only when already configured. Culprit never enables the preview or creates
//! foreign views while collecting a production database.
use crate::probe::node_id;
use crate::read::{QueryRead, query_one};
use culprit_core::{Engine, Result};
#[cfg(test)]
use culprit_postgres::wire::ObservedSession;
use std::collections::BTreeSet;

pub(crate) async fn configured(client: &mut impl QueryRead, base: &str) -> Result<Option<String>> {
    // Only internal callers select a known catalog name. Qualified names come from the catalog and are quoted.
    let base = base.replace('\'', "''");
    Ok(query_one(
        client,
        &format!(
            "SELECT n.nspname AS schema,c.relname AS name FROM pg_class c
        JOIN pg_namespace n ON n.oid=c.relnamespace JOIN pg_foreign_table f ON f.ftrelid=c.oid
        JOIN pg_foreign_server s ON s.oid=f.ftserver JOIN pg_roles r ON r.oid=c.relowner
        WHERE c.relname='gv${base}' AND s.srvname='yb_global_views_server' AND r.rolsuper
        ORDER BY n.nspname LIMIT 1"
        ),
    )
    .await?
    .map(|r| Engine::Yugabyte.dialect().qualified(&[&r.string("schema"), &r.string("name")])))
}

#[cfg(test)]
pub(crate) async fn nodes(session: &ObservedSession) -> Result<BTreeSet<String>> {
    nodes_from(&mut culprit_postgres::wire::BorrowedRead::new(session)).await
}
pub(crate) const MAX_FANOUT_NODES: usize = 16;
pub(crate) async fn nodes_from(reader: &mut impl culprit_postgres::wire::NoticeRead) -> Result<BTreeSet<String>> {
    let before = reader.notices();
    let rows = reader
        .query(
            &format!("SELECT uuid::text AS node FROM pg_catalog.yb_servers() LIMIT {}", MAX_FANOUT_NODES + 1),
            MAX_FANOUT_NODES + 1,
        )
        .await?;
    if rows.len() > MAX_FANOUT_NODES {
        return Err(culprit_core::telemetry::CollectionStop::RowLimit.error());
    }
    let ids: Option<BTreeSet<_>> = rows.iter().map(|row| node_id(&row.string("node"))).collect();
    ids.filter(|ids| !ids.is_empty() && reader.notices() == before)
        .ok_or_else(|| culprit_core::Error::invalid(culprit_core::t!("provider.yugabyte.error.node_inventory")))
}
