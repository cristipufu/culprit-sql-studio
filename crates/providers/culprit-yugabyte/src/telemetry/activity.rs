//! Node-scoped YSQL requests and targeted DocDB blocking. A partial lock graph never proves resolution.
use super::*;
use culprit_core::MeasurementScope;
use culprit_postgres::wire::{QueryRead, activity as sql_activity};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};
use std::collections::{BTreeMap, BTreeSet};
const MAX_LOCK_LOOKUPS: usize = 16;
struct ActivityReader {
    io: ReadSession,
    host: String,
    port: u16,
    metadata: Option<Metadata>,
    notices: u64,
}
pub(super) fn source(connector: Connector, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    Ok(Box::new(FrozenSource::new(
        ActivityReader {
            host: connector.host.clone(),
            port: connector.port,
            io: ReadSession::for_engine(connector, request.budget.clone(), Engine::Yugabyte),
            metadata: None,
            notices: 0,
        },
        request,
    )?))
}
#[async_trait::async_trait]
impl ObservationReader for ActivityReader {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let policy = request.activity_policy().ok_or_else(native::invalid)?;
        let meta = metadata::read(&mut self.io, &self.host, self.port).await?;
        if meta.probe.statement_role.is_some() {
            return Err(Error::new(ErrorCode::PermissionDenied, culprit_core::t!("collection.invalid_source")));
        }
        self.notices = self.io.notices();
        let sql = sql_activity::query_layout(
            false,
            "'client backend','yb-conn-mgr worker connection'",
            ",a.yb_backend_xid::text AS transaction_id",
            request.budget.max_snapshot_rows as usize + 1,
        );
        let mut frame =
            sql_activity::observe_rows(&mut self.io, &sql, &meta.probe.oid.to_string(), &request.budget).await?;
        if !frame.fully_visible_for(&meta.probe.oid.to_string()) || self.io.notices() != self.notices {
            return Err(CollectionStop::SourceUnavailable.error());
        }
        blocking(&mut self.io, &mut frame, &meta, policy, &request.budget).await?;
        let native_rows = frame.records.len() as u32;
        let selected = ActivityEvidence::select(&frame.observations(), policy)?;
        let keys: BTreeSet<_> = selected.observations.iter().map(|row| row.key.clone()).collect();
        let cap = (request.budget.page_bytes as usize / keys.len().max(1))
            .saturating_sub(1024)
            .min(request.budget.max_sql_bytes as usize)
            .min(16 * 1024);
        let details = sql_activity::enrich_for(&mut self.io, &mut frame, &keys, cap, false, Engine::Yugabyte).await?;
        let mut observations: Vec<_> =
            frame.records.into_iter().filter_map(|row| row.facts).filter(|row| keys.contains(&row.key)).collect();
        for row in &mut observations {
            row.key.session.id = format!("{}/{}", meta.node, row.key.session.id);
            if let Some(wait) = &mut row.wait {
                for owner in &mut wait.owners {
                    if let ActivityWaitOwner::Session { id, .. } = owner {
                        *id = format!("{}/{id}", meta.node);
                    }
                }
                if let Some(resources) = &mut wait.resources {
                    for resource in resources {
                        resource.session = row.key.session.id.clone();
                    }
                }
            }
        }
        // Select again after native identity qualification so edges and observations share the same namespace.
        let selected = ActivityEvidence::select(&observations, policy)?;
        let description = CollectionDescription {
            database: meta.database.clone(),
            source: format!("ysql-activity-{}", meta.node),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Activity(ActivitySnapshotDescription {
                epoch: digest(
                    format!(
                        "{}|{}|{}|ysql-activity-v1",
                        meta.node,
                        meta.probe.oid,
                        meta.probe.started.as_deref().unwrap_or_default()
                    )
                    .as_bytes(),
                ),
                at: frame.at,
                scope: MeasurementScope::Node,
                complete_scope: self.io.notices() == self.notices,
                policy: policy.clone(),
                native_rows,
                qualifying: selected.conditions.len() as u32,
                uncertain: selected.uncertain.len() as u32,
                observations: observations.len() as u32,
                blocking_edges: selected.blocking.len() as u32,
                details,
            }),
        };
        self.metadata = Some(meta);
        Ok(FrozenObservation {
            description,
            records: observations
                .into_iter()
                .map(TelemetryRecord::Activity)
                .chain(selected.blocking.into_iter().map(TelemetryRecord::Blocking))
                .collect(),
        })
    }
    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        let after = metadata::read(&mut self.io, &self.host, self.port).await?;
        Ok(self.metadata.as_ref().is_some_and(|before| before.same_source(&after)) && self.io.notices() == self.notices)
    }
    fn query_count(&self) -> u32 {
        self.io.queries()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}
async fn blocking(
    io: &mut ReadSession,
    frame: &mut sql_activity::ActivityFrame,
    meta: &Metadata,
    policy: &ActivityPolicy,
    budget: &CollectionBudget,
) -> Result<()> {
    for row in frame.records.iter_mut().filter_map(|row| row.facts.as_mut()) {
        if row.scope == ActivityScopeBasis::SessionDatabase
            && row.key.request.is_some()
            && row
                .elapsed_ms
                .as_deref()
                .and_then(|age| age.parse::<u64>().ok())
                .is_some_and(|age| age >= policy.blocked_after_ms.into())
        {
            row.blocking_status = ActivityBlockingStatus::Unknown;
        }
    }
    // YSQL pg_stat_activity does not consistently expose DocDB waits. Probe lock metadata for aged
    // distributed transactions, including those whose SQL wait label is absent.
    let targets: Vec<_> = frame
        .records
        .iter()
        .filter(|row| {
            row.facts.as_ref().is_some_and(|facts| {
                facts.scope == ActivityScopeBasis::SessionDatabase
                    && facts.key.request.is_some()
                    && facts
                        .elapsed_ms
                        .as_deref()
                        .and_then(|n| n.parse::<u64>().ok())
                        .is_some_and(|n| n >= policy.blocked_after_ms.into())
            })
        })
        .filter_map(|row| row.native_text("transaction_id"))
        .map(|id| transaction(&id))
        .collect::<Result<_>>()?;
    if targets.is_empty() {
        return Ok(());
    }
    // Reject oversized topology before issuing a native lock RPC. SQL LIMIT cannot bound the function's fanout.
    crate::global::nodes_from(io).await?;
    // This changes only the owned metadata session filter; it never enables cluster instrumentation.
    io.query("SET yb_locks_min_txn_age=0", 0).await?;
    let transactions: BTreeMap<_, _> = frame
        .records
        .iter()
        .filter_map(|row| {
            Some((transaction(&row.native_text("transaction_id")?).ok()?, row.facts.as_ref()?.key.session.clone()))
        })
        .collect();
    for tx in targets.into_iter().take(MAX_LOCK_LOOKUPS) {
        let sql = crate::sessions::lock_query(meta.probe.oid, Some(&tx), budget.max_snapshot_rows as usize + 1)?;
        for row in io.query(&sql, budget.max_snapshot_rows as usize + 1).await? {
            // A transaction-filtered native request omits node/PID, even on current YSQL. Its globally unique
            // transaction UUID binds the lock to the already observed session; a supplied conflicting node is rejected.
            let node =
                row.text("node").map(|value| crate::probe::node_id(&value).ok_or_else(native::invalid)).transpose()?;
            if node.as_ref().is_some_and(|node| node != &meta.node) {
                continue;
            }
            let waiter_tx = transaction(&native::text(&row, "transaction")?)?;
            let Some(waiter) = transactions.get(&waiter_tx) else {
                continue;
            };
            let Some(facts) = frame
                .records
                .iter_mut()
                .filter_map(|row| row.facts.as_mut())
                .find(|facts| facts.key.session == *waiter)
            else {
                continue;
            };
            let owners = parse_owners(row.text("blocked_by").as_deref().unwrap_or("{}"), &transactions)?;
            let elapsed = row
                .text("wait_ms")
                .map(|value| value.parse::<u64>().map(|n| n.to_string()).map_err(|_| native::invalid()))
                .transpose()?;
            let resource = culprit_core::LockWait {
                session: facts.key.session.id.clone(),
                resource: row.text("locktype"),
                object: row.text("object"),
                index: None,
                mode: row.text("mode"),
            };
            let wait = facts.wait.get_or_insert(ActivityWait {
                native_type: Some("DocDB lock".into()),
                elapsed_ms: None,
                owners: vec![],
                resources: Some(vec![]),
            });
            wait.owners.extend(owners);
            wait.owners.sort();
            wait.owners.dedup();
            wait.elapsed_ms = elapsed;
            if wait.elapsed_ms.is_some() && wait.owners.iter().any(ActivityWaitOwner::is_blocker) {
                facts.blocking_status = ActivityBlockingStatus::Known;
            }
            let resources = wait.resources.get_or_insert_with(Vec::new);
            resources.push(resource);
            if resources.len() > 64 || wait.owners.len() > 64 {
                return Err(CollectionStop::RowLimit.error());
            }
        }
    }
    // A missing/capped result leaves the affected request Unknown. The shared incident tracker retains its
    // previous proof and timestamp until a complete request frame establishes that the request ended.
    Ok(())
}
fn transaction(value: &str) -> Result<String> {
    let value = value.replace('-', "");
    if value.len() != 32 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(native::invalid());
    }
    Ok(value.to_ascii_lowercase())
}
fn parse_owners(value: &str, known: &BTreeMap<String, NativeActivityIdentity>) -> Result<Vec<ActivityWaitOwner>> {
    let mut owners = vec![];
    for value in value.trim_matches(['{', '}']).split(',').filter(|value| !value.is_empty()) {
        let native_id = transaction(value.trim().trim_matches('"'))?;
        owners.push(match known.get(&native_id) {
            Some(session) => {
                ActivityWaitOwner::Session { id: session.id.clone(), generation: Some(session.generation.clone()) }
            }
            None => ActivityWaitOwner::Transaction { native_id, owner: TransactionWaitOwner::Active },
        });
        if owners.len() > 64 {
            return Err(CollectionStop::RowLimit.error());
        }
    }
    Ok(owners)
}
