use crate::read::{NoticeRead, query};
use crate::{
    global,
    probe::{Probe, node_id},
    sources,
};
use culprit_core::{Capability, Coverage, Pressure, Result, Snapshot, SourceState, t};
use std::collections::{BTreeMap, BTreeSet};

type Epochs = BTreeMap<String, Option<String>>;
async fn epochs(session: &mut impl NoticeRead, probe: &Probe) -> Result<Epochs> {
    let rows = query(
        session,
        &format!(
            "SELECT server_uuid::text AS node,{} AS reset
        FROM pg_catalog.\"gv$pg_stat_database\" WHERE datid={}",
            crate::probe::timestamp("stats_reset"),
            probe.oid
        ),
    )
    .await?;
    rows.into_iter()
        .map(|r| {
            Ok((
                node_id(&r.string("node"))
                    .ok_or_else(|| culprit_core::Error::invalid(t!("provider.yugabyte.error.unknown_node")))?,
                r.text("reset"),
            ))
        })
        .collect()
}

pub(crate) async fn read(
    session: &mut impl NoticeRead,
    probe: &Probe,
    snapshot: &mut Snapshot,
    coverage: &mut Coverage,
) {
    if !probe.global {
        coverage.recorded_message(
            &sources::INDEX_USAGE,
            SourceState::Unavailable,
            culprit_core::message!("provider.yugabyte.coverage.index_usage_local"),
        );
        coverage.lacking(
            Capability::IndexUsage,
            culprit_core::message!("provider.yugabyte.capability.index_usage.local"),
            Some(t!("provider.yugabyte.capability.index_usage.global_views")),
        );
        return;
    }
    let notices = session.notices();
    let inventory = global::nodes_from(session).await;
    if let Err(error) = &inventory {
        coverage.read::<()>(&sources::INDEX_USAGE, Err(error.clone()));
        coverage.lacking(
            Capability::IndexUsage,
            culprit_core::message!("provider.yugabyte.capability.index_usage.contributors"),
            None,
        );
        return;
    }
    let before = epochs(session, probe).await;
    let rows = coverage.read_rows(
        &sources::INDEX_USAGE,
        query(
            session,
            "SELECT schemaname,relname,indexrelname,sum(idx_scan)::float8 AS reads,
            count(DISTINCT server_uuid)::text AS nodes FROM pg_catalog.\"gv$pg_stat_user_indexes\"
         GROUP BY schemaname,relname,indexrelname",
        )
        .await,
    );
    let after = epochs(session, probe).await;
    let last_inventory = global::nodes_from(session).await;
    let complete = before
        .as_ref()
        .ok()
        .zip(after.as_ref().ok())
        .zip(inventory.as_ref().ok())
        .zip(last_inventory.as_ref().ok())
        .is_some_and(|(((a, b), expected), last)| {
            a == b && expected == last && &a.keys().cloned().collect::<BTreeSet<_>>() == expected
        })
        && session.notices() == notices;
    let Some(rows) = rows.filter(|_| complete) else {
        coverage
            .partial(&sources::INDEX_USAGE, culprit_core::message!("provider.yugabyte.coverage.index_usage_changed"));
        coverage.lacking(
            Capability::IndexUsage,
            culprit_core::message!("provider.yugabyte.capability.index_usage.contributors"),
            None,
        );
        return;
    };
    let epochs = before.unwrap_or_default();
    let reset = epochs
        .values()
        .map(|at| at.as_deref().filter(|at| culprit_core::format::epoch_seconds(at).is_some()))
        .collect::<Option<Vec<_>>>()
        .and_then(|dates| dates.into_iter().max());
    let count = epochs.len();
    let mut usage = BTreeMap::new();
    let mut partial = false;
    for row in rows {
        if row.string("nodes").parse::<usize>().ok() != Some(count) {
            partial = true;
            continue;
        }
        usage.insert(
            (row.string("schemaname"), row.string("relname"), row.string("indexrelname")),
            row.number("reads").filter(|n| *n >= 0.0),
        );
    }
    for index in &mut snapshot.indexes {
        let key = (index.schema.clone(), index.table.clone(), index.name.clone());
        if let Some(reads) = usage.get(&key) {
            index.reads = *reads;
            index.counters_since = reset.map(str::to_string);
        } else {
            partial = true;
        }
    }
    if partial {
        coverage
            .partial(&sources::INDEX_USAGE, culprit_core::message!("provider.yugabyte.coverage.index_usage_partial"));
        coverage.lacking(
            Capability::IndexUsage,
            culprit_core::message!("provider.yugabyte.capability.index_usage.partial"),
            None,
        );
    } else {
        coverage.delivered(Capability::IndexUsage);
    }
    let common_reset = reset.filter(|at| epochs.values().all(|value| value.as_deref() == Some(*at)));
    if let Some(hours) = common_reset
        .and_then(culprit_core::format::epoch_seconds)
        .zip(culprit_core::format::epoch_seconds(&probe.now))
        .map(|(a, b)| (b - a) / 3600.0)
        .filter(|h| *h >= 0.0)
    {
        snapshot.pressure.get_or_insert_with(Pressure::default).uptime_hours = Some(hours);
    } else {
        coverage.note("YSQL index history", culprit_core::message!("provider.yugabyte.coverage.index_usage_epochs"));
    }
}
