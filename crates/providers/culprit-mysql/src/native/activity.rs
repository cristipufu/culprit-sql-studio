//! Persistent native request identities; healthy reads omit SQL, transaction scans and lock-manager detail.
use super::locks;
use super::*;
use culprit_core::{Engine, Error, ErrorCode, telemetry::*};
use mysql_async::Value;
use std::collections::{BTreeMap, BTreeSet};

const MAX_BLOCKER_DEPTH: usize = 8;
const MAX_BLOCKER_LOOKUPS: usize = 64;
pub(crate) struct Frame {
    pub observations: Vec<ActivityObservation>,
    pub native_rows: u32,
    pub details: ActivityDetailStatus,
    pub complete: bool,
}
pub(crate) async fn observe<R: QueryRead>(
    reader: &mut R,
    meta: &metadata::Metadata,
    budget: &CollectionBudget,
    policy: &ActivityPolicy,
) -> Result<Frame> {
    if !meta.connection.performance_schema || !meta.process_wide {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    let consumers = reader
        .read(
            "SELECT NAME AS name, ENABLED AS enabled FROM performance_schema.setup_consumers
        WHERE NAME IN ('global_instrumentation','thread_instrumentation','events_statements_current')",
            vec![],
            4,
        )
        .await?;
    if consumers.len() != 3 || consumers.iter().any(|row| row.text("enabled").as_deref() != Some("YES")) {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    let mut rows =
        sessions(reader, Some(&meta.connection.database_name), &[], budget.max_snapshot_rows as usize + 1).await?;
    if rows.len() > budget.max_snapshot_rows as usize {
        return Err(CollectionStop::RowLimit.error());
    }
    let mut pending = rows
        .iter()
        .filter(|row| {
            row.key.request.is_some()
                && row
                    .elapsed_ms
                    .as_ref()
                    .and_then(|value| value.parse::<u64>().ok())
                    .is_some_and(|value| value >= u64::from(policy.blocked_after_ms))
        })
        .map(|row| row.key.session.id.clone())
        .collect::<BTreeSet<_>>();
    let mut seen = BTreeSet::new();
    let mut details = ActivityDetailStatus::NotNeeded;
    let mut complete = true;
    for _ in 0..MAX_BLOCKER_DEPTH {
        if pending.is_empty() {
            break;
        }
        if seen.len() + pending.len() > MAX_BLOCKER_LOOKUPS {
            return Err(CollectionStop::RowLimit.error());
        }
        let ids: Vec<_> = pending.iter().cloned().collect();
        seen.extend(pending.iter().cloned());
        let pairs = lock_pairs(reader, meta, &ids, budget.max_snapshot_rows as usize + 1).await;
        let pairs = match pairs {
            Ok(pairs) => pairs,
            Err(error) if error.code == ErrorCode::PermissionDenied => {
                complete = false;
                details = ActivityDetailStatus::Unavailable { code: error.code };
                break;
            }
            Err(error) => return Err(error),
        };
        details = ActivityDetailStatus::Collected;
        let owners = pairs.iter().map(|row| row.blocking.to_string()).collect::<BTreeSet<_>>();
        let missing: Vec<_> =
            owners.iter().filter(|id| !rows.iter().any(|row| row.key.session.id == **id)).cloned().collect();
        if !missing.is_empty() {
            let additional = sessions(reader, None, &missing, missing.len() + 1).await?;
            if additional.len() > missing.len() {
                return Err(invalid());
            }
            rows.extend(additional);
            if rows.len() > budget.max_snapshot_rows as usize {
                return Err(CollectionStop::RowLimit.error());
            }
        }
        let transactions = transaction_context(reader, &ids, &owners).await?;
        for row in &mut rows {
            if let Some(transaction) = transactions.get(&row.key.session.id) {
                row.open_transactions = 1;
                row.transaction_age_ms = transaction.unsigned("age_ms")?.map(|age| age.to_string());
                if row.idle {
                    let id = integer(transaction, "trx_id")?;
                    if id > 0 {
                        row.key.transaction = Some(id.to_string());
                    }
                }
            }
        }
        let generations: BTreeMap<_, _> =
            rows.iter().map(|row| (row.key.session.id.clone(), row.key.session.generation.clone())).collect();
        for pair in pairs {
            let waiter = pair.session.to_string();
            let owner = pair.blocking.to_string();
            if waiter == owner {
                continue;
            }
            if let Some(row) = rows.iter_mut().find(|row| row.key.session.id == waiter && row.key.request.is_some()) {
                let state_age = row.wait.as_ref().and_then(|wait| wait.elapsed_ms.clone());
                let elapsed_ms = transactions
                    .get(&waiter)
                    .map(|row| row.unsigned("wait_ms"))
                    .transpose()?
                    .flatten()
                    .map(|age| age.to_string())
                    .or(state_age);
                let wait = row.wait.get_or_insert(ActivityWait {
                    identified_resources: vec![],
                    native_type: None,
                    elapsed_ms: None,
                    owners: vec![],
                    resources: Some(vec![]),
                });
                wait.elapsed_ms = elapsed_ms;
                if wait.owners.len() >= 64 {
                    return Err(CollectionStop::RowLimit.error());
                }
                wait.owners.push(ActivityWaitOwner::Session {
                    id: owner.clone(),
                    generation: generations.get(&owner).cloned(),
                });
                wait.resources.get_or_insert_default().push(pair.lock_wait());
                if let Some(resource) = pair.identified_resource
                    && !wait.identified_resources.contains(&resource)
                {
                    wait.identified_resources.push(resource);
                }
            }
        }
        pending = owners.difference(&seen).cloned().collect();
    }
    if !pending.is_empty() {
        return Err(CollectionStop::RowLimit.error());
    }
    ActivityEvidence::validate(&rows)?;
    Ok(Frame { native_rows: rows.len() as u32, observations: rows, details, complete })
}

async fn sessions<R: QueryRead>(
    reader: &mut R,
    database: Option<&str>,
    ids: &[String],
    limit: usize,
) -> Result<Vec<ActivityObservation>> {
    let (filter, parameters) = if let Some(database) = database {
        ("t.PROCESSLIST_DB=?".into(), vec![Value::from(database)])
    } else {
        (format!("t.PROCESSLIST_ID IN ({})", placeholders(ids.len())?), numeric_ids(ids)?)
    };
    let sql = format!("SELECT t.PROCESSLIST_ID AS id, t.THREAD_ID AS thread_id, t.PROCESSLIST_DB AS db,
        t.PROCESSLIST_USER AS user_name, t.PROCESSLIST_HOST AS host, t.PROCESSLIST_COMMAND AS command,
        t.PROCESSLIST_STATE AS state, t.PROCESSLIST_TIME AS seconds, t.INSTRUMENTED AS instrumented,
        e.EVENT_ID AS event_id, e.TIMER_START AS timer_start, e.TIMER_WAIT AS elapsed_ps, e.END_EVENT_ID AS end_event_id,
        e.CURRENT_SCHEMA AS request_db, e.DIGEST AS query_hash
        FROM performance_schema.threads t LEFT JOIN performance_schema.events_statements_current e ON e.THREAD_ID=t.THREAD_ID
        WHERE t.TYPE='FOREGROUND' AND t.PROCESSLIST_ID<>CONNECTION_ID() AND {filter}
          AND t.PROCESSLIST_COMMAND NOT IN ('Daemon','Binlog Dump','Binlog Dump GTID') LIMIT {limit}");
    reader.read(&sql, parameters, limit).await?.iter().map(|row| decode(row, database)).collect()
}
fn decode(row: &Rec, database: Option<&str>) -> Result<ActivityObservation> {
    let command = text(row, "command")?;
    let idle = command == "Sleep";
    if !idle && row.unsigned("elapsed_ps")?.is_none() {
        return Err(CollectionStop::SourceUnavailable.error());
    }
    let request = if !idle {
        if text(row, "instrumented")? != "YES" || row.unsigned("end_event_id")?.is_some() {
            return Err(CollectionStop::SourceUnavailable.error());
        }
        Some(NativeActivityIdentity {
            id: integer(row, "event_id")?.to_string(),
            generation: integer(row, "timer_start")?.to_string(),
        })
    } else {
        None
    };
    let state = row.native_text("state")?.filter(|value| !value.is_empty()).unwrap_or_else(|| command.clone());
    let metadata_wait = state.eq_ignore_ascii_case("Waiting for table metadata lock");
    let wait_elapsed = if metadata_wait {
        row.unsigned("seconds")?.and_then(|seconds| seconds.checked_mul(1000)).map(|age| age.to_string())
    } else {
        None
    };
    let request_db = row.native_text("request_db")?;
    Ok(ActivityObservation {
        blocking_status: culprit_core::telemetry::ActivityBlockingStatus::Known,
        key: ActivityKey {
            session: NativeActivityIdentity {
                id: integer(row, "id")?.to_string(),
                generation: integer(row, "thread_id")?.to_string(),
            },
            request,
            transaction: None,
        },
        idle,
        scope: if database.is_none() {
            ActivityScopeBasis::BlockingReference
        } else if request_db.as_deref() == database {
            ActivityScopeBasis::RequestDatabase
        } else {
            ActivityScopeBasis::SessionDatabase
        },
        state,
        database: request_db.or(row.native_text("db")?),
        command: Some(command),
        last_request_age_ms: None,
        open_transactions: 0,
        elapsed_ms: if idle {
            None
        } else {
            row.unsigned("elapsed_ps")?.map(|value| (value / 1_000_000_000).to_string())
        },
        transaction_age_ms: None,
        session_counters: Default::default(),
        request_counters: (!idle).then(ActivityCounters::default),
        wait: metadata_wait.then(|| ActivityWait {
            identified_resources: vec![],
            native_type: Some("metadata-lock".into()),
            elapsed_ms: wait_elapsed,
            owners: vec![],
            resources: None,
        }),
        query_hash: row.native_text("query_hash")?,
        client: Some(ActivityClient {
            application: None,
            user: row.native_text("user_name")?,
            host: row.native_text("host")?,
        }),
        statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
    })
}
async fn lock_pairs<R: QueryRead>(
    reader: &mut R,
    meta: &metadata::Metadata,
    ids: &[String],
    limit: usize,
) -> Result<Vec<locks::LockPair>> {
    let layout = locks::row_waits(meta.connection.flavor, &meta.connection.version);
    let (sql, alias) = (layout.sql, layout.session_column);
    let mut rows = reader
        .read(&format!("{sql} WHERE {alias} IN ({}) LIMIT {limit}", placeholders(ids.len())?), numeric_ids(ids)?, limit)
        .await?;
    let metadata = reader
        .read(
            &format!(
                "{} AND waiting_pid IN ({}) LIMIT {limit}",
                locks::METADATA_LOCK_WAITS_SQL,
                placeholders(ids.len())?
            ),
            numeric_ids(ids)?,
            limit,
        )
        .await?;
    rows.extend(metadata);
    if rows.len() >= limit {
        return Err(CollectionStop::RowLimit.error());
    }
    locks::decode(rows)
}
async fn transaction_context<R: QueryRead>(
    reader: &mut R,
    ids: &[String],
    owners: &BTreeSet<String>,
) -> Result<BTreeMap<String, Rec>> {
    let ids: Vec<_> = ids.iter().chain(owners).cloned().collect::<BTreeSet<_>>().into_iter().collect();
    if ids.len() > MAX_BLOCKER_LOOKUPS {
        return Err(CollectionStop::RowLimit.error());
    }
    let rows = reader
        .read(
            &format!(
                "SELECT trx_mysql_thread_id AS id, trx_id AS trx_id,
        TIMESTAMPDIFF(MICROSECOND,trx_started,NOW(6)) DIV 1000 AS age_ms,
        TIMESTAMPDIFF(MICROSECOND,trx_wait_started,NOW(6)) DIV 1000 AS wait_ms
        FROM information_schema.INNODB_TRX WHERE trx_mysql_thread_id IN ({})",
                placeholders(ids.len())?
            ),
            numeric_ids(&ids)?,
            ids.len() + 1,
        )
        .await?;
    let mut out = BTreeMap::new();
    for row in rows {
        if out.insert(integer(&row, "id")?.to_string(), row).is_some() {
            return Err(invalid());
        }
    }
    Ok(out)
}
pub(crate) async fn enrich<R: QueryRead>(
    reader: &mut R,
    rows: &mut [ActivityObservation],
    sql_limit: usize,
) -> Result<()> {
    for row in rows {
        let Some(request) = &row.key.request else { continue };
        let ids = numeric_ids(&[
            row.key.session.id.clone(),
            row.key.session.generation.clone(),
            request.id.clone(),
            request.generation.clone(),
        ])?;
        let native = reader.read(&format!("SELECT CASE WHEN OCTET_LENGTH(e.SQL_TEXT)<={sql_limit} THEN e.SQL_TEXT ELSE NULL END AS sql_text,
            OCTET_LENGTH(e.SQL_TEXT) AS text_bytes, a.ATTR_VALUE AS application FROM performance_schema.threads t
            JOIN performance_schema.events_statements_current e ON e.THREAD_ID=t.THREAD_ID
            LEFT JOIN performance_schema.session_connect_attrs a ON a.PROCESSLIST_ID=t.PROCESSLIST_ID AND a.ATTR_NAME='program_name'
            WHERE t.PROCESSLIST_ID=? AND t.THREAD_ID=? AND e.EVENT_ID=? AND e.TIMER_START=? AND e.END_EVENT_ID IS NULL"), ids, 2).await?;
        if native.len() > 1 {
            return Err(invalid());
        }
        if let Some(native) = native.first() {
            row.statement = match native.native_text("sql_text")? {
                Some(sql) => DefinitionPayload::sql(&sql, Engine::Mysql),
                None => DefinitionPayload::Unavailable {
                    reason: if native.unsigned("text_bytes")?.is_some_and(|bytes| bytes > sql_limit as u64) {
                        PayloadUnavailable::Oversized
                    } else {
                        PayloadUnavailable::Missing
                    },
                },
            };
            if let Some(client) = &mut row.client {
                client.application = native.native_text("application")?;
            }
        }
    }
    Ok(())
}
fn placeholders(count: usize) -> Result<String> {
    if count == 0 || count > MAX_BLOCKER_LOOKUPS {
        return Err(CollectionStop::RowLimit.error());
    }
    Ok(vec!["?"; count].join(","))
}
fn numeric_ids(ids: &[String]) -> Result<Vec<Value>> {
    ids.iter().map(|id| id.parse::<u64>().map(Value::UInt).map_err(|_| invalid())).collect()
}
