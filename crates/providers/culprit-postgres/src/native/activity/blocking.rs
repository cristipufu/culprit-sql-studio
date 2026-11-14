//! Follow only native blocker identities reached from this database's observed waiters.
use super::*;

const MAX_BLOCKING_LOOKUPS: usize = 64;
const MAX_BLOCKING_DEPTH: usize = 8;

pub(crate) struct BlockingReadOptions<'a> {
    pub minimum_request_age_ms: u32,
    pub version: u32,
    pub database_oid: &'a str,
    pub max_snapshot_rows: usize,
}

struct Closure {
    by_pid: BTreeMap<u32, usize>,
    pending: BTreeSet<usize>,
    visited: BTreeSet<usize>,
    looked_up: BTreeSet<u32>,
}
struct OwnerContext {
    record: ActivityRecord,
    waiters: BTreeSet<u32>,
}
type Edges = BTreeMap<u32, BTreeSet<u32>>;

pub(crate) fn unqualified_owner_ids(frame: &ActivityFrame) -> Vec<String> {
    frame
        .records
        .iter()
        .filter_map(|record| record.facts.as_ref())
        .flat_map(|facts| facts.wait.iter().flat_map(|wait| &wait.owners))
        .filter_map(|owner| match owner {
            ActivityWaitOwner::Session { id, generation: None } => Some(id.clone()),
            _ => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

impl Closure {
    fn new(frame: &ActivityFrame, options: &BlockingReadOptions<'_>) -> Result<Self> {
        let mut by_pid = BTreeMap::new();
        let mut pending = BTreeSet::new();
        for (index, row) in frame.records.iter().enumerate() {
            if by_pid.insert(pid(&row.row)?, index).is_some() {
                return Err(invalid());
            }
            if row.row.text("wait_event_type").as_deref() == Some("Lock")
                && row.facts.as_ref().is_some_and(|facts| {
                    facts.scope != ActivityScopeBasis::OtherDatabase
                        && facts
                            .elapsed_ms
                            .as_deref()
                            .and_then(|age| age.parse::<u64>().ok())
                            .is_some_and(|age| age >= options.minimum_request_age_ms.into())
                })
            {
                pending.insert(index);
            }
        }
        Ok(Self { by_pid, pending, visited: BTreeSet::new(), looked_up: BTreeSet::new() })
    }

    fn attach(
        &mut self,
        frame: &mut ActivityFrame,
        edges: Edges,
        contexts: BTreeMap<u32, OwnerContext>,
        max_rows: usize,
    ) -> Result<()> {
        self.visited.append(&mut self.pending);
        for (waiter, owners) in edges {
            let index = *self.by_pid.get(&waiter).ok_or_else(invalid)?;
            let mut waits = Vec::new();
            for owner in owners {
                if owner == 0 {
                    waits.push(ActivityWaitOwner::Transaction {
                        native_id: "0".into(),
                        owner: TransactionWaitOwner::Prepared,
                    });
                    continue;
                }
                let mut generation = None;
                if let Some(context) = contexts.get(&owner).filter(|context| context.waiters.contains(&waiter)) {
                    let observed = context.record.facts.as_ref();
                    if let Some(existing) = self.by_pid.get(&owner) {
                        if let Some(current) = observed
                            && frame.records[*existing].facts.as_ref().is_some_and(|before| before.key == current.key)
                        {
                            generation = Some(current.key.session.generation.clone());
                        }
                    } else if let Some(current) = observed {
                        if frame.records.len() >= max_rows {
                            return Err(CollectionStop::RowLimit.error());
                        }
                        generation = Some(current.key.session.generation.clone());
                        let next = frame.records.len();
                        frame.records.push(ActivityRecord {
                            facts: context.record.facts.clone(),
                            row: context.record.row.clone(),
                            sql: None,
                        });
                        self.by_pid.insert(owner, next);
                    }
                }
                if generation.is_some()
                    && let Some(owner_index) = self.by_pid.get(&owner).filter(|index| !self.visited.contains(index))
                    && frame.records[*owner_index].row.text("wait_event_type").as_deref() == Some("Lock")
                {
                    self.pending.insert(*owner_index);
                }
                waits.push(ActivityWaitOwner::Session { id: owner.to_string(), generation });
            }
            if let Some(facts) = frame.records[index].facts.as_mut() {
                facts.blocking_status = ActivityBlockingStatus::Known;
                facts.wait.as_mut().ok_or_else(invalid)?.owners = waits;
            }
        }
        Ok(())
    }
}

pub(crate) async fn blockers<R: QueryRead>(
    reader: &mut R,
    frame: &mut ActivityFrame,
    options: BlockingReadOptions<'_>,
) -> Result<()> {
    let mut closure = Closure::new(frame, &options)?;
    for _ in 0..MAX_BLOCKING_DEPTH {
        if closure.pending.is_empty() {
            break;
        }
        if closure.pending.len() + closure.visited.len() > MAX_BLOCKING_LOOKUPS {
            return Err(CollectionStop::RowLimit.error());
        }
        let targets = selectors(frame, &closure.pending)?;
        let expected =
            closure.pending.iter().map(|index| pid(&frame.records[*index].row)).collect::<Result<BTreeSet<_>>>()?;
        for index in &closure.pending {
            if let Some(facts) = frame.records[*index].facts.as_mut() {
                facts.blocking_status = ActivityBlockingStatus::Unknown;
            }
        }
        let sql = format!(
            "WITH targets(pid,backend_start,query_start) AS (VALUES {targets}) SELECT a.pid::text AS pid,pg_catalog.array_to_json(pg_catalog.pg_blocking_pids(a.pid))::text AS owners FROM targets t JOIN pg_catalog.pg_stat_activity a ON a.pid=t.pid AND a.backend_start=t.backend_start AND a.query_start IS NOT DISTINCT FROM t.query_start WHERE a.wait_event_type='Lock'"
        );
        let edges = decode_edges(reader.query(&sql, expected.len()).await?, &expected)?;
        let owners = edges.values().flatten().copied().filter(|id| *id != 0).collect::<BTreeSet<_>>();
        closure.looked_up.extend(&owners);
        if closure.looked_up.len() > MAX_BLOCKING_LOOKUPS {
            return Err(CollectionStop::RowLimit.error());
        }
        let contexts = owner_context(reader, frame, &targets, &edges, &owners, &options).await?;
        closure.attach(frame, edges, contexts, options.max_snapshot_rows)?;
    }
    if !closure.pending.is_empty() {
        return Err(CollectionStop::RowLimit.error());
    }
    ActivityEvidence::validate(&frame.observations())?;
    if !closure.visited.is_empty() && options.version >= 140_000 {
        lock_resources(reader, frame, &closure.by_pid, &closure.visited).await?;
    }
    Ok(())
}

fn pid(row: &Row) -> Result<u32> {
    text(row, "pid")?.parse().map_err(|_| invalid())
}
fn selectors(frame: &ActivityFrame, indices: &BTreeSet<usize>) -> Result<String> {
    indices.iter().map(|index| selector(&frame.records[*index])).collect::<Result<Vec<_>>>().map(|rows| rows.join(","))
}
fn decode_edges(rows: Vec<Row>, expected: &BTreeSet<u32>) -> Result<Edges> {
    let mut edges = BTreeMap::new();
    for row in rows {
        let waiter = pid(&row)?;
        let owners: Vec<u32> = serde_json::from_str(&text(&row, "owners")?).map_err(|_| invalid())?;
        if owners.len() > MAX_BLOCKING_LOOKUPS {
            return Err(CollectionStop::RowLimit.error());
        }
        if !expected.contains(&waiter) || edges.insert(waiter, owners.into_iter().collect()).is_some() {
            return Err(invalid());
        }
    }
    Ok(edges)
}
async fn owner_context<R: QueryRead>(
    reader: &mut R,
    frame: &ActivityFrame,
    targets: &str,
    edges: &Edges,
    owners: &BTreeSet<u32>,
    options: &BlockingReadOptions<'_>,
) -> Result<BTreeMap<u32, OwnerContext>> {
    if owners.is_empty() {
        return Ok(BTreeMap::new());
    }
    let ids = owners.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
    let ctes = format!(
        "targets(pid,backend_start,query_start) AS (VALUES {targets}),
         native_owners AS MATERIALIZED (
            SELECT a.pid AS waiter_pid,unnest(pg_catalog.pg_blocking_pids(a.pid)) AS owner_pid
            FROM targets t JOIN pg_catalog.pg_stat_activity a
            ON a.pid=t.pid AND a.backend_start=t.backend_start AND a.query_start IS NOT DISTINCT FROM t.query_start
            WHERE a.wait_event_type='Lock'),
         owner_context AS (SELECT owner_pid,array_agg(DISTINCT waiter_pid) AS blocking_waiters
            FROM native_owners WHERE owner_pid IN ({ids}) GROUP BY owner_pid),"
    );
    let sql = metadata_query(
        options.version >= 140_000,
        ",pg_catalog.array_to_json(a.blocking_waiters)::text AS blocking_waiters",
        "SELECT a.*,c.blocking_waiters FROM pg_catalog.pg_stat_activity a JOIN owner_context c ON c.owner_pid=a.pid WHERE a.pid<>pg_catalog.pg_backend_pid()",
        owners.len() + 1,
        &ctes,
    );
    let observed = observe_limited(reader, &sql, options.database_oid, owners.len()).await?;
    if canonical_time(&observed.at)? < canonical_time(&frame.at)? {
        return Err(CollectionStop::SourceChanged.error());
    }
    let mut result = BTreeMap::new();
    for record in observed.records {
        let id = pid(&record.row)?;
        let waiters: Vec<u32> = serde_json::from_str(&text(&record.row, "blocking_waiters")?).map_err(|_| invalid())?;
        if !owners.contains(&id)
            || waiters.is_empty()
            || waiters.len() > MAX_BLOCKING_LOOKUPS
            || waiters.iter().any(|waiter| !edges.get(waiter).is_some_and(|owners| owners.contains(&id)))
            || result.insert(id, OwnerContext { record, waiters: waiters.into_iter().collect() }).is_some()
        {
            return Err(invalid());
        }
    }
    Ok(result)
}

async fn lock_resources<R: QueryRead>(
    reader: &mut R,
    frame: &mut ActivityFrame,
    by_pid: &BTreeMap<u32, usize>,
    visited: &BTreeSet<usize>,
) -> Result<()> {
    let lock_targets = selectors(frame, visited)?;
    // One global lock snapshot per troubled poll, never one scan per waiter. Healthy polls do not read pg_locks.
    let sql = format!("WITH targets(pid,backend_start,query_start) AS (VALUES {lock_targets}),
        current_waiters AS MATERIALIZED (SELECT a.pid FROM targets t JOIN pg_catalog.pg_stat_activity a ON a.pid=t.pid AND a.backend_start=t.backend_start AND a.query_start IS NOT DISTINCT FROM t.query_start WHERE a.wait_event_type='Lock'),
        waits AS MATERIALIZED (SELECT w.pid,w.waitstart,w.mode,w.locktype,w.relation,w.database FROM pg_catalog.pg_locks w WHERE NOT w.granted AND w.pid IN (SELECT pid FROM current_waiters)),
        stamp AS (SELECT pg_catalog.clock_timestamp() AS at)
        SELECT w.pid::text AS pid,CASE WHEN stamp.at>=w.waitstart THEN floor(extract(epoch FROM stamp.at-w.waitstart)*1000)::text END AS elapsed_ms,
        w.mode,w.locktype,n.nspname||'.'||o.relname AS object,CASE WHEN c.relkind IN ('i','I') THEN c.relname END AS index_name
        FROM waits w CROSS JOIN stamp
        LEFT JOIN pg_catalog.pg_class c ON c.oid=w.relation AND (w.database=0 OR w.database=(SELECT oid FROM pg_catalog.pg_database WHERE datname=pg_catalog.current_database()))
        LEFT JOIN pg_catalog.pg_index ix ON ix.indexrelid=c.oid LEFT JOIN pg_catalog.pg_class o ON o.oid=coalesce(ix.indrelid,c.oid)
        LEFT JOIN pg_catalog.pg_namespace n ON n.oid=o.relnamespace");
    for row in reader.query(&sql, visited.len()).await? {
        let pid = pid(&row)?;
        let index = *by_pid.get(&pid).filter(|index| visited.contains(index)).ok_or_else(invalid)?;
        let resource = culprit_core::LockWait {
            session: pid.to_string(),
            mode: row.text("mode"),
            resource: row.text("locktype"),
            object: row.text("object"),
            index: row.text("index_name"),
        };
        if let Some(wait) = frame.records[index].facts.as_mut().and_then(|facts| facts.wait.as_mut()) {
            wait.elapsed_ms = optional_counter(&row, "elapsed_ms")?;
            wait.resources.get_or_insert_with(Vec::new).push(resource.clone());
        }
        frame.locks.push(resource);
    }
    Ok(())
}

#[cfg(test)]
mod native_tests;
#[cfg(test)]
mod tests;
