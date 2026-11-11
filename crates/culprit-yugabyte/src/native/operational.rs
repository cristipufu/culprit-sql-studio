//! Bounded native observations share their application readers. Samples and retained records never become rates.
use super::{metadata::Metadata, *};
use culprit_core::{MeasurementScope, SnapshotOptions};
use culprit_postgres::wire::{QueryRead, ReadSession};
use std::collections::BTreeMap;
#[derive(Clone, Copy)]
pub(crate) enum Kind {
    Database,
    Resources,
    Activity,
    Terminations,
}
impl Kind {
    pub(crate) const ALL: &[Self] = &[Self::Database, Self::Resources, Self::Activity, Self::Terminations];
    pub(crate) fn source(self) -> &'static str {
        match self {
            Self::Database => "pg_stat_database",
            Self::Resources => "yb_servers_metrics",
            Self::Activity => "yb_active_session_history",
            Self::Terminations => "yb_terminated_queries",
        }
    }
    pub(crate) async fn read(
        self,
        io: &mut ReadSession,
        meta: &Metadata,
        budget: &CollectionBudget,
    ) -> Result<(Vec<TelemetryRecord>, bool)> {
        let source = self.source();
        let mut records = vec![];
        let mut complete = true;
        let mut probe = meta.probe.clone();
        // Historical buffers are read locally. Global FDW views can materialize unbounded remote buffers before
        // applying LIMIT. Node identity remains explicit; no local poll claims all cluster history.
        probe.global = false;
        let options = SnapshotOptions { window_minutes: 5, plans: 0, ..Default::default() };
        match self {
            Self::Database => {
                let rows=io.query(&format!("SELECT datid::text AS id,numbackends::text AS sessions,xact_commit::text AS commits,xact_rollback::text AS rollbacks,deadlocks::text AS deadlocks,conflicts::text AS conflicts,{} AS reset FROM pg_catalog.pg_stat_database WHERE datid={}",crate::probe::timestamp("stats_reset"),probe.oid),2).await?;
                for row in rows {
                    let mut counters = vec![];
                    let reset = row
                        .text("reset")
                        .filter(|time| canonical_time(time).is_ok())
                        .map(culprit_core::ResetMarker::At)
                        .unwrap_or(culprit_core::ResetMarker::Unknown);
                    for (name, total) in [
                        ("sessions", false),
                        ("commits", true),
                        ("rollbacks", true),
                        ("deadlocks", true),
                        ("conflicts", true),
                    ] {
                        let mut counter = NativeCounterSample::gauge(
                            name,
                            integer(&row, name)?,
                            CounterUnit::Count,
                            MeasurementScope::Node,
                            source,
                            name,
                        );
                        if total {
                            counter.nature = CounterNature::Cumulative;
                            counter.reset = reset.clone();
                        }
                        counters.push(counter);
                    }
                    records.push(record(
                        source,
                        identity(meta, "database-counters"),
                        MeasurementScope::Node,
                        counters,
                        BTreeMap::new(),
                    )?);
                }
            }
            Self::Resources => {
                let reading = crate::resources::read_native(
                    io,
                    crate::global::MAX_FANOUT_NODES.min(budget.max_snapshot_rows as usize),
                )
                .await?;
                complete = !reading.view.partial;
                for mut counters in reading.counters {
                    counters.properties.insert("observed-from".into(), reading.view.observed_from.clone());
                    counters.properties.insert("observed-until".into(), reading.view.observed_until.clone());
                    records.push(record_set(source, counters)?);
                }
            }
            Self::Activity => {
                let history = crate::ash::read_from(io, &probe, &options, budget.max_snapshot_rows as usize).await?;
                complete = !history.period.partial;
                for event in history.events {
                    let id = serde_json::to_string(&(
                        &event.node,
                        &event.query_id,
                        &event.component,
                        &event.category,
                        &event.wait_event,
                    ))
                    .map_err(|_| invalid())?;
                    let mut properties = BTreeMap::from([
                        ("category".into(), event.category),
                        ("component".into(), event.component),
                        ("wait-event".into(), event.wait_event),
                        ("requested-start".into(), history.period.requested_start.clone()),
                        ("requested-end".into(), history.period.requested_end.clone()),
                        ("observation-kind".into(), "sampled-tasks".into()),
                    ]);
                    if let Some(query) = event.query_id {
                        properties.insert("query-id".into(), query);
                    }
                    records.push(record(
                        source,
                        identity(meta, &id),
                        MeasurementScope::Node,
                        vec![
                            real(
                                "observations",
                                event.observations,
                                CounterUnit::Count,
                                MeasurementScope::Node,
                                source,
                            )?,
                            real(
                                "weighted-task-observations",
                                event.weighted_observations,
                                CounterUnit::Count,
                                MeasurementScope::Node,
                                source,
                            )?,
                        ],
                        properties,
                    )?);
                }
            }
            Self::Terminations => {
                let history =
                    crate::terminations::read_from(io, &probe, &options, budget.max_snapshot_rows as usize).await?;
                complete = !history.partial;
                for event in history.events {
                    let id = serde_json::to_string(&(
                        &event.query_id,
                        &event.node,
                        &event.started_at,
                        &event.ended_at,
                        event.reason,
                    ))
                    .map_err(|_| invalid())?;
                    let mut properties = BTreeMap::from([
                        ("ended-at".into(), event.ended_at),
                        (
                            "reason".into(),
                            serde_json::to_string(&event.reason).map_err(|_| invalid())?.trim_matches('"').to_string(),
                        ),
                        ("observation-kind".into(), "retained-record".into()),
                        ("history-complete".into(), "false".into()),
                    ]);
                    if let Some(start) = event.started_at {
                        properties.insert("started-at".into(), start);
                    }
                    if let Some(query) = event.query_id {
                        properties.insert("query-id".into(), query);
                    }
                    records.push(record(source, identity(meta, &id), MeasurementScope::Node, vec![], properties)?);
                }
            }
        }
        Ok((records, complete))
    }
}
fn identity(meta: &Metadata, id: &str) -> NativeIdentity {
    NativeIdentity::new(id).qualified("node", &meta.node).qualified("database_oid", meta.probe.oid.to_string())
}
fn real(
    name: &str,
    value: f64,
    unit: CounterUnit,
    scope: MeasurementScope,
    source: &str,
) -> Result<NativeCounterSample> {
    if !value.is_finite() || value < 0.0 {
        return Err(invalid());
    }
    Ok(NativeCounterSample {
        value: MeasurementValue::Real(value),
        ..NativeCounterSample::gauge(name, 0, unit, scope, source, name)
    })
}
fn record(
    source: &str,
    identity: NativeIdentity,
    scope: MeasurementScope,
    counters: Vec<NativeCounterSample>,
    properties: BTreeMap<String, String>,
) -> Result<TelemetryRecord> {
    let counters = NativeCounterSet { identity, scope, counters, properties };
    record_set(source, counters)
}
fn record_set(source: &str, counters: NativeCounterSet) -> Result<TelemetryRecord> {
    Ok(TelemetryRecord::Operational(OperationalObservation {
        source: source.into(),
        key: counters.key()?,
        fact: OperationalFact::CounterSet(Box::new(counters)),
    }))
}
