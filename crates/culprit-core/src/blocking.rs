//! Blocking chains: who holds the locks everyone else waits for, why, and what to do about it. A pure function of the
//! sessions Live already reads plus the lock each waiter wants, so Live, the CLI and MCP tell the same story.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::{Engine, Session};
use crate::{format, t};

/// The lock a waiting session asked for and didn't get.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq, Default)]
#[ts(export)]
pub struct LockWait {
    /// The waiting session.
    pub session: String,
    /// The lock mode it asked for, as the engine names it: `X`, `U`, `S`, `ShareLock`, `X,REC_NOT_GAP`.
    pub mode: Option<String>,
    /// What is locked: `KEY`, `PAGE`, `OBJECT`, `transactionid`, `RECORD`, `TABLE`.
    pub resource: Option<String>,
    /// `schema.table`, when the engine can tell.
    pub object: Option<String>,
    pub index: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum HeadKind {
    /// Holds a transaction open and sends nothing: the classic cause.
    IdleInTransaction,
    /// Still running a statement while holding locks.
    Running,
    /// Connected, not running anything, no transaction age known.
    Idle,
    /// Not visible to this login (another database, or not permitted).
    Unknown,
}

/// One blocked session (or the head) and who waits behind it.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct BlockNode {
    pub session: String,
    /// The lock this session waits for; `None` for the head.
    pub lock: Option<LockWait>,
    /// That lock in words: "an update lock on shop.orders".
    pub waits_for: Option<String>,
    pub children: Vec<BlockNode>,
}

/// Everyone waiting, directly or through others, on one head blocker.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct BlockingChain {
    pub head: String,
    pub head_kind: HeadKind,
    /// How long the head has been idle (idle heads) or running (running heads).
    pub head_ms: Option<f64>,
    /// Age of the head's open transaction.
    pub transaction_ms: Option<f64>,
    /// Sessions waiting behind the head, directly or through others.
    pub waiters: u32,
    pub longest_wait_ms: Option<f64>,
    /// The wait most of the head's direct waiters report, e.g. `LCK_M_X` or `Lock: transactionid`.
    pub wait_type: Option<String>,
    /// The lock most of the head's direct waiters want.
    pub lock: Option<LockWait>,
    /// One sentence: who, for how long, how many wait, on what.
    pub headline: String,
    /// What this kind of head usually means and what to do, in plain language.
    pub advice: String,
    pub tree: BlockNode,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Blocking {
    /// Biggest chain first.
    pub chains: Vec<BlockingChain>,
    /// Sessions waiting on another session, across all chains.
    pub waiting: u32,
}

/// The blocking chains in `sessions`, with lock details from `locks` where the engine provided them. `None` when
/// nobody waits on anybody.
pub fn analyze(engine: Engine, sessions: &[Session], locks: &[LockWait]) -> Option<Blocking> {
    let by_id: HashMap<&str, &Session> = sessions.iter().map(|s| (s.id.as_str(), s)).collect();
    let lock_of: HashMap<&str, &LockWait> = locks.iter().map(|l| (l.session.as_str(), l)).collect();
    let wait_of: HashMap<&str, &str> =
        sessions.iter().filter_map(|s| Some((s.id.as_str(), s.wait.as_deref()?))).collect();
    let mut children: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for s in sessions {
        if let Some(blocker) = s.blocked_by.as_deref().filter(|b| *b != s.id) {
            children.entry(blocker).or_default().push(&s.id);
        }
    }
    if children.is_empty() {
        return None;
    }

    // Heads hold locks and wait on nobody. A cycle (a deadlock being resolved right now) has no such session, so the
    // smallest id in it stands in as the head.
    let mut heads: Vec<&str> = children
        .keys()
        .copied()
        .filter(|id| by_id.get(id).and_then(|s| s.blocked_by.as_deref()).is_none_or(|b| b == *id))
        .collect();
    let mut placed: BTreeSet<&str> = BTreeSet::new();
    for head in &heads {
        collect(head, &children, &mut placed);
    }
    for id in children.keys() {
        if !placed.contains(id) {
            heads.push(id);
            collect(id, &children, &mut placed);
        }
    }

    let mut chains: Vec<BlockingChain> = heads
        .into_iter()
        .map(|head| {
            let mut seen = BTreeSet::new();
            let tree = node(head, &children, &lock_of, &wait_of, &mut seen, true);
            chain(engine, head, by_id.get(head).copied(), tree, &by_id, &lock_of)
        })
        .filter(|c| c.waiters > 0)
        .collect();
    chains.sort_by(|a, b| b.waiters.cmp(&a.waiters).then_with(|| a.head.cmp(&b.head)));
    let waiting = chains.iter().map(|c| c.waiters).sum();
    Some(Blocking { chains, waiting })
}

fn collect<'a>(id: &'a str, children: &BTreeMap<&'a str, Vec<&'a str>>, placed: &mut BTreeSet<&'a str>) {
    if !placed.insert(id) {
        return;
    }
    for child in children.get(id).into_iter().flatten() {
        collect(child, children, placed);
    }
}

fn node<'a>(
    id: &'a str,
    children: &BTreeMap<&'a str, Vec<&'a str>>,
    lock_of: &HashMap<&str, &LockWait>,
    wait_of: &HashMap<&str, &str>,
    seen: &mut BTreeSet<&'a str>,
    head: bool,
) -> BlockNode {
    seen.insert(id);
    let kids = children.get(id).into_iter().flatten().filter(|c| !seen.contains(**c)).copied().collect::<Vec<_>>();
    let children_nodes = kids.into_iter().map(|c| node(c, children, lock_of, wait_of, seen, false)).collect();
    let lock = if head { None } else { lock_of.get(id).map(|l| (*l).clone()) };
    let waits_for = if head { None } else { lock_phrase(wait_of.get(id).copied(), lock.as_ref()) };
    BlockNode { session: id.to_string(), lock, waits_for, children: children_nodes }
}

fn descendants(node: &BlockNode) -> Vec<&BlockNode> {
    let mut out = Vec::new();
    for child in &node.children {
        out.push(child);
        out.extend(descendants(child));
    }
    out
}

fn most_common<T: Clone + Ord>(items: impl Iterator<Item = T>) -> Option<T> {
    let mut counts: BTreeMap<T, usize> = BTreeMap::new();
    for item in items {
        *counts.entry(item).or_default() += 1;
    }
    counts.into_iter().max_by(|a, b| a.1.cmp(&b.1).then_with(|| b.0.cmp(&a.0))).map(|(k, _)| k)
}

/// The kind of statement a session runs ("SELECT", "UPDATE"): SQL Server's command says it; MySQL's says only "Query",
/// so the statement's first keyword does.
fn statement_verb(session: &Session) -> Option<String> {
    const VERBS: [&str; 12] = [
        "SELECT", "INSERT", "UPDATE", "DELETE", "MERGE", "REPLACE", "CREATE", "ALTER", "DROP", "TRUNCATE", "CALL",
        "WITH",
    ];
    let verb = |word: &str| {
        let upper = word.trim_start_matches('(').to_ascii_uppercase();
        VERBS.contains(&upper.as_str()).then(|| if upper == "WITH" { "SELECT".to_string() } else { upper })
    };
    session
        .command
        .as_deref()
        .and_then(|c| c.split_whitespace().next())
        .and_then(verb)
        .or_else(|| session.sql.as_deref().and_then(|t| t.split_whitespace().next()).and_then(verb))
}

fn head_kind(session: Option<&Session>) -> HeadKind {
    let Some(s) = session else { return HeadKind::Unknown };
    let state = s.state.to_ascii_lowercase();
    if state.contains("idle in transaction") {
        return HeadKind::IdleInTransaction;
    }
    // SQL Server: a sleeping session with an open transaction.
    if (state == "sleeping" || state == "idle") && s.transaction_ms.is_some() {
        return HeadKind::IdleInTransaction;
    }
    if matches!(state.as_str(), "sleeping" | "idle" | "dormant") {
        return HeadKind::Idle;
    }
    HeadKind::Running
}

/// What kind of lock, in words a non-DBA reads: `LCK_M_U` → "an update lock", `X,REC_NOT_GAP` → "an exclusive row lock",
/// a Postgres `ShareLock` on a transaction id → "a row lock".
pub fn lock_kind(wait_type: Option<&str>, lock: Option<&LockWait>) -> Option<String> {
    // A lock of `strength` over `scope` (`none` when the mode says no more): "an exclusive row lock".
    let mode_lock = |strength: &str, scope: &str| match strength {
        "X" => Some(t!("core.blocking.lock.exclusive", scope = scope)),
        "S" => Some(t!("core.blocking.lock.shared", scope = scope)),
        "IX" => Some(t!("core.blocking.lock.intent_exclusive", scope = scope)),
        "IS" => Some(t!("core.blocking.lock.intent_shared", scope = scope)),
        "U" => Some(t!("core.blocking.lock.update", scope = scope)),
        _ => None,
    };
    if let Some(w) = wait_type.filter(|w| w.starts_with("LCK_M_")) {
        let mode = &w["LCK_M_".len()..];
        return Some(match mode {
            "S" | "U" | "X" | "IS" | "IX" => mode_lock(mode, "none").unwrap_or_default(),
            "IU" => t!("core.blocking.lock.intent_update"),
            "SIX" | "SIU" | "UIX" => t!("core.blocking.lock.shared_and_intent"),
            "SCH_S" => t!("core.blocking.lock.schema_stability"),
            "SCH_M" => t!("core.blocking.lock.schema_change"),
            "BU" => t!("core.blocking.lock.bulk_load"),
            m if m.starts_with("R") => t!("core.blocking.lock.key_range"),
            _ => t!("core.blocking.lock.any"),
        });
    }
    let lock = lock?;
    if matches!(lock.resource.as_deref(), Some("transactionid") | Some("tuple")) {
        return Some(t!("core.blocking.lock.row"));
    }
    let mode = lock.mode.as_deref()?;
    // MySQL performance_schema modes: X, S, IX, IS, plus REC_NOT_GAP / GAP / INSERT_INTENTION.
    let parts: Vec<&str> = mode.split(',').map(str::trim).collect();
    let base = parts.first().copied().unwrap_or("");
    let has = |p: &str| parts.iter().any(|x| x.eq_ignore_ascii_case(p));
    let scope = if has("INSERT_INTENTION") {
        "insert_intention"
    } else if has("GAP") {
        "gap"
    } else if has("REC_NOT_GAP") {
        "row"
    } else if lock
        .resource
        .as_deref()
        .is_some_and(|r| r.eq_ignore_ascii_case("TABLE") || r.eq_ignore_ascii_case("OBJECT"))
    {
        "table"
    } else {
        "none"
    };
    if let Some(words) = mode_lock(base, scope) {
        return Some(words);
    }
    // Postgres table locks: AccessExclusiveLock, ShareRowExclusiveLock, …
    Some(match mode {
        "AccessExclusiveLock" => t!("core.blocking.lock.table_wide_exclusive"),
        m if m.ends_with("Lock") => t!("core.blocking.lock.named", mode = m),
        m => t!("core.blocking.lock.named_mode", mode = m),
    })
}

/// "an update lock on shop.orders", "a row lock on public.orders", "an exclusive row lock on shop.orders".
pub fn lock_phrase(wait_type: Option<&str>, lock: Option<&LockWait>) -> Option<String> {
    let object = lock.and_then(|l| l.object.as_deref());
    match (lock_kind(wait_type, lock), object) {
        (Some(kind), Some(object)) => Some(t!("core.blocking.lock.on", lock = kind, object = object)),
        (Some(kind), None) => Some(kind),
        (None, Some(object)) => Some(t!("core.blocking.lock.any_on", object = object)),
        (None, None) => wait_type.map(str::to_string),
    }
}

fn chain(
    engine: Engine,
    head: &str,
    session: Option<&Session>,
    tree: BlockNode,
    by_id: &HashMap<&str, &Session>,
    lock_of: &HashMap<&str, &LockWait>,
) -> BlockingChain {
    let kind = head_kind(session);
    let all = descendants(&tree);
    let waiters = all.len() as u32;
    let longest_wait_ms = all
        .iter()
        .filter_map(|n| by_id.get(n.session.as_str()).and_then(|s| s.wait_ms.or(s.elapsed_ms)))
        .fold(None, |acc: Option<f64>, v| Some(acc.map_or(v, |a| a.max(v))));
    let direct: Vec<&str> = tree.children.iter().map(|c| c.session.as_str()).collect();
    // MySQL's "wait" is the processlist state (updating, statistics), not a wait type; the lock says it better.
    let wait_type = if engine == Engine::Mysql {
        None
    } else {
        most_common(direct.iter().filter_map(|id| by_id.get(id).and_then(|s| s.wait.clone())))
    };
    let lock = most_common(
        direct.iter().filter_map(|id| lock_of.get(id)).map(|l| (l.object.clone(), l.mode.clone(), l.resource.clone())),
    )
    .and_then(|(object, mode, resource)| {
        direct
            .iter()
            .filter_map(|id| lock_of.get(id))
            .find(|l| (&l.object, &l.mode, &l.resource) == (&object, &mode, &resource))
            .map(|l| (*l).clone())
    });
    let head_ms = session.and_then(|s| s.elapsed_ms);
    let transaction_ms = session.and_then(|s| s.transaction_ms);
    let behind = match lock_phrase(wait_type.as_deref(), lock.as_ref()) {
        Some(lock) => t!("core.blocking.behind_on", n = waiters, count = waiters.to_string(), lock = lock),
        None => t!("core.blocking.behind", n = waiters, count = waiters.to_string()),
    };
    let wait_type = wait_type.filter(|w| w.starts_with("LCK_M_") || w.starts_with("Lock"));
    let headline = match kind {
        HeadKind::IdleInTransaction => match head_ms {
            Some(ms) => t!(
                "core.blocking.headline.idle_in_transaction_for",
                head = head,
                duration = format::duration(ms),
                behind = behind
            ),
            None => t!("core.blocking.headline.idle_in_transaction", head = head, behind = behind),
        },
        HeadKind::Running => {
            // The verb is an SQL keyword (`SELECT`, `INSERT`), said as it is.
            let statement = match session.and_then(statement_verb) {
                Some(verb) => t!("core.blocking.statement_verb", verb = verb),
                None => t!("core.blocking.statement"),
            };
            match head_ms {
                Some(ms) => t!(
                    "core.blocking.headline.running_for",
                    head = head,
                    statement = statement,
                    duration = format::duration(ms),
                    behind = behind
                ),
                None => t!("core.blocking.headline.running", head = head, statement = statement, behind = behind),
            }
        }
        HeadKind::Idle => t!("core.blocking.headline.idle", head = head, behind = behind),
        HeadKind::Unknown => t!("core.blocking.headline.unknown", head = head, behind = behind),
    };
    let advice = advice(engine, kind, head, wait_type.as_deref());
    BlockingChain {
        head: head.to_string(),
        head_kind: kind,
        head_ms,
        transaction_ms,
        waiters,
        longest_wait_ms,
        wait_type,
        lock,
        headline,
        advice,
        tree,
    }
}

fn advice(engine: Engine, kind: HeadKind, head: &str, wait_type: Option<&str>) -> String {
    let readers = wait_type.is_some_and(|w| w == "LCK_M_S" || w == "LCK_M_IS");
    match kind {
        HeadKind::IdleInTransaction => {
            // How each engine keeps an idle transaction from holding its locks.
            let prevent = match engine {
                Engine::Mssql => t!("core.blocking.advice.prevent.mssql"),
                Engine::Postgres | Engine::Yugabyte => t!("core.blocking.advice.prevent.postgres"),
                Engine::Mysql => t!("core.blocking.advice.prevent.mysql"),
                Engine::Sqlite => t!("core.blocking.advice.prevent.sqlite"),
            };
            [t!("core.blocking.advice.idle_in_transaction", head = head), prevent].join(" ")
        }
        HeadKind::Running => {
            let mut sentences = vec![t!("core.blocking.advice.running")];
            if engine == Engine::Mssql && readers {
                sentences.push(t!("core.blocking.advice.readers_rcsi"));
            }
            sentences.join(" ")
        }
        HeadKind::Idle => t!("core.blocking.advice.idle", head = head),
        HeadKind::Unknown => t!("core.blocking.advice.unknown"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, state: &str, blocked_by: Option<&str>) -> Session {
        Session {
            id: id.into(),
            user: Some("app".into()),
            application: None,
            host: None,
            database: Some("shop".into()),
            state: state.into(),
            command: None,
            wait: blocked_by.map(|_| "LCK_M_U".into()),
            waiting_on_lock: Some(blocked_by.is_some()),
            wait_resources: None,
            wait_ms: blocked_by.map(|_| 30_000.0),
            blocked_by: blocked_by.map(str::to_string),
            elapsed_ms: Some(48_000.0),
            transaction_ms: Some(49_000.0),
            cpu_ms: None,
            reads: None,
            sql: None,
        }
    }

    fn lock(session: &str) -> LockWait {
        LockWait {
            session: session.into(),
            mode: Some("U".into()),
            resource: Some("KEY".into()),
            object: Some("shop.orders".into()),
            index: Some("PK_orders".into()),
        }
    }

    #[test]
    fn a_running_head_is_named_by_its_statement() {
        // MySQL's process list says "Query" for every statement; the statement's first keyword says what it is.
        let mut head = session("114", "running", None);
        head.command = Some("Query".into());
        head.sql = Some("select sleep(40)".into());
        let waiter = session("115", "suspended", Some("114"));
        let b = analyze(Engine::Mysql, &[head.clone(), waiter.clone()], &[lock("115")]).unwrap();
        assert!(b.chains[0].headline.contains("running a SELECT statement"), "{}", b.chains[0].headline);
        // SQL Server's command is already the verb.
        head.command = Some("UPDATE".into());
        head.sql = None;
        let b = analyze(Engine::Mssql, &[head, waiter], &[lock("115")]).unwrap();
        assert!(b.chains[0].headline.contains("running an UPDATE statement"), "{}", b.chains[0].headline);
    }

    #[test]
    fn nobody_waiting_is_no_blocking() {
        assert!(analyze(Engine::Mssql, &[session("51", "running", None)], &[]).is_none());
    }

    #[test]
    fn an_idle_head_with_a_tree_of_waiters() {
        let sessions = [
            session("57", "sleeping", None),
            session("61", "suspended", Some("57")),
            session("64", "suspended", Some("61")),
            session("70", "suspended", Some("57")),
            session("80", "running", None),
        ];
        let b = analyze(Engine::Mssql, &sessions, &[lock("61"), lock("70")]).unwrap();
        assert_eq!(b.chains.len(), 1);
        let c = &b.chains[0];
        assert_eq!((c.head.as_str(), c.head_kind, c.waiters), ("57", HeadKind::IdleInTransaction, 3));
        assert_eq!(b.waiting, 3);
        assert_eq!(c.tree.children.len(), 2);
        assert_eq!(
            c.headline,
            "Session 57 has been idle in an open transaction for 48.0 s; 3 sessions wait behind it (an update lock on shop.orders)."
        );
        assert!(c.advice.contains("XACT_ABORT"));
    }

    #[test]
    fn a_blocker_this_login_cannot_see() {
        let b = analyze(Engine::Postgres, &[session("61", "active", Some("99"))], &[]).unwrap();
        assert_eq!(b.chains[0].head_kind, HeadKind::Unknown);
        assert!(b.chains[0].headline.contains("can't see"));
    }

    #[test]
    fn a_cycle_still_gets_a_head() {
        let b = analyze(
            Engine::Mssql,
            &[session("61", "suspended", Some("62")), session("62", "suspended", Some("61"))],
            &[],
        )
        .unwrap();
        assert_eq!(b.chains.len(), 1);
        assert_eq!(b.chains[0].waiters, 1);
    }

    #[test]
    fn lock_names_read_as_words() {
        let mysql = LockWait {
            session: "9".into(),
            mode: Some("X,REC_NOT_GAP".into()),
            resource: Some("RECORD".into()),
            object: Some("shop.orders".into()),
            index: Some("PRIMARY".into()),
        };
        assert_eq!(lock_phrase(None, Some(&mysql)).unwrap(), "an exclusive row lock on shop.orders");
        assert_eq!(lock_phrase(Some("LCK_M_SCH_M"), None).unwrap(), "a schema-change lock");
        let gap = LockWait { mode: Some("X,GAP,INSERT_INTENTION".into()), ..mysql.clone() };
        assert_eq!(lock_kind(None, Some(&gap)).unwrap(), "an exclusive insert-intention lock");
        let ddl = LockWait {
            session: "1".into(),
            mode: Some("AccessExclusiveLock".into()),
            resource: Some("relation".into()),
            object: Some("public.orders".into()),
            index: None,
        };
        assert_eq!(lock_phrase(None, Some(&ddl)).unwrap(), "a table-wide exclusive lock on public.orders");
        assert_eq!(lock_phrase(Some("PAGEIOLATCH_SH"), None).unwrap(), "PAGEIOLATCH_SH");
    }

    // Every lock and every kind of head, word for word, on every engine: the messages come from locales/en.json.
    #[test]
    fn chains_say_who_blocks_in_english() {
        let mut out = Vec::new();
        for mode in ["S", "U", "X", "IS", "IU", "IX", "SIX", "SCH_S", "SCH_M", "BU", "RangeS_S", "Z"] {
            let wait = format!("LCK_M_{mode}");
            out.push(format!("{wait}: {}", lock_kind(Some(&wait), None).unwrap()));
        }
        for mode in ["X", "S", "IX", "IS", "U", "AccessExclusiveLock", "RowExclusiveLock", "SIREAD"] {
            for scope in ["", ",INSERT_INTENTION", ",GAP", ",REC_NOT_GAP"] {
                for resource in ["RECORD", "TABLE", "transactionid"] {
                    let l = LockWait {
                        session: "1".into(),
                        mode: Some(format!("{mode}{scope}")),
                        resource: Some(resource.into()),
                        object: None,
                        index: None,
                    };
                    out.push(format!("{mode}{scope} {resource}: {}", lock_kind(None, Some(&l)).unwrap()));
                }
            }
        }
        let on = LockWait { object: Some("shop.orders".into()), mode: None, resource: None, ..lock("1") };
        out.push(format!("object only: {}", lock_phrase(None, Some(&on)).unwrap()));
        for engine in Engine::ALL {
            for state in ["sleeping", "idle in transaction", "running", "dormant"] {
                for (elapsed, waiters, wait) in
                    [(Some(48_000.0), 1, "LCK_M_S"), (None, 2, "LCK_M_X"), (None, 1, "PAGELATCH")]
                {
                    let mut head = session("57", state, None);
                    head.elapsed_ms = elapsed;
                    if state == "dormant" {
                        head.transaction_ms = None;
                    }
                    head.command = (state == "running" && waiters == 2).then(|| "INSERT".into());
                    let mut sessions = vec![head];
                    for w in 0..waiters {
                        let mut s = session(&format!("6{w}"), "suspended", Some("57"));
                        s.wait = Some(wait.into());
                        sessions.push(s);
                    }
                    let locks = if wait == "PAGELATCH" { vec![] } else { vec![lock("60")] };
                    let c = &analyze(engine, &sessions, &locks).unwrap().chains[0];
                    out.push(format!(
                        "\n{engine:?} {state} {elapsed:?} {waiters} {wait}\n{}\n{}",
                        c.headline, c.advice
                    ));
                }
            }
            let c = &analyze(engine, &[session("61", "active", Some("99"))], &[]).unwrap().chains[0];
            out.push(format!("\n{engine:?} unseen\n{}\n{}", c.headline, c.advice));
        }
        let got = out.join("\n");
        assert_eq!(
            got,
            r#"LCK_M_S: a shared lock
LCK_M_U: an update lock
LCK_M_X: an exclusive lock
LCK_M_IS: an intent-shared lock
LCK_M_IU: an intent-update lock
LCK_M_IX: an intent-exclusive lock
LCK_M_SIX: a shared-and-intent lock
LCK_M_SCH_S: a schema-stability lock
LCK_M_SCH_M: a schema-change lock
LCK_M_BU: a bulk-load lock
LCK_M_RangeS_S: a key-range lock
LCK_M_Z: a lock
X RECORD: an exclusive lock
X TABLE: an exclusive table lock
X transactionid: a row lock
X,INSERT_INTENTION RECORD: an exclusive insert-intention lock
X,INSERT_INTENTION TABLE: an exclusive insert-intention lock
X,INSERT_INTENTION transactionid: a row lock
X,GAP RECORD: an exclusive gap lock
X,GAP TABLE: an exclusive gap lock
X,GAP transactionid: a row lock
X,REC_NOT_GAP RECORD: an exclusive row lock
X,REC_NOT_GAP TABLE: an exclusive row lock
X,REC_NOT_GAP transactionid: a row lock
S RECORD: a shared lock
S TABLE: a shared table lock
S transactionid: a row lock
S,INSERT_INTENTION RECORD: a shared insert-intention lock
S,INSERT_INTENTION TABLE: a shared insert-intention lock
S,INSERT_INTENTION transactionid: a row lock
S,GAP RECORD: a shared gap lock
S,GAP TABLE: a shared gap lock
S,GAP transactionid: a row lock
S,REC_NOT_GAP RECORD: a shared row lock
S,REC_NOT_GAP TABLE: a shared row lock
S,REC_NOT_GAP transactionid: a row lock
IX RECORD: an intent-exclusive lock
IX TABLE: an intent-exclusive table lock
IX transactionid: a row lock
IX,INSERT_INTENTION RECORD: an intent-exclusive insert-intention lock
IX,INSERT_INTENTION TABLE: an intent-exclusive insert-intention lock
IX,INSERT_INTENTION transactionid: a row lock
IX,GAP RECORD: an intent-exclusive gap lock
IX,GAP TABLE: an intent-exclusive gap lock
IX,GAP transactionid: a row lock
IX,REC_NOT_GAP RECORD: an intent-exclusive row lock
IX,REC_NOT_GAP TABLE: an intent-exclusive row lock
IX,REC_NOT_GAP transactionid: a row lock
IS RECORD: an intent-shared lock
IS TABLE: an intent-shared table lock
IS transactionid: a row lock
IS,INSERT_INTENTION RECORD: an intent-shared insert-intention lock
IS,INSERT_INTENTION TABLE: an intent-shared insert-intention lock
IS,INSERT_INTENTION transactionid: a row lock
IS,GAP RECORD: an intent-shared gap lock
IS,GAP TABLE: an intent-shared gap lock
IS,GAP transactionid: a row lock
IS,REC_NOT_GAP RECORD: an intent-shared row lock
IS,REC_NOT_GAP TABLE: an intent-shared row lock
IS,REC_NOT_GAP transactionid: a row lock
U RECORD: an update lock
U TABLE: an update table lock
U transactionid: a row lock
U,INSERT_INTENTION RECORD: an update insert-intention lock
U,INSERT_INTENTION TABLE: an update insert-intention lock
U,INSERT_INTENTION transactionid: a row lock
U,GAP RECORD: an update gap lock
U,GAP TABLE: an update gap lock
U,GAP transactionid: a row lock
U,REC_NOT_GAP RECORD: an update row lock
U,REC_NOT_GAP TABLE: an update row lock
U,REC_NOT_GAP transactionid: a row lock
AccessExclusiveLock RECORD: a table-wide exclusive lock
AccessExclusiveLock TABLE: a table-wide exclusive lock
AccessExclusiveLock transactionid: a row lock
AccessExclusiveLock,INSERT_INTENTION RECORD: a AccessExclusiveLock,INSERT_INTENTION lock
AccessExclusiveLock,INSERT_INTENTION TABLE: a AccessExclusiveLock,INSERT_INTENTION lock
AccessExclusiveLock,INSERT_INTENTION transactionid: a row lock
AccessExclusiveLock,GAP RECORD: a AccessExclusiveLock,GAP lock
AccessExclusiveLock,GAP TABLE: a AccessExclusiveLock,GAP lock
AccessExclusiveLock,GAP transactionid: a row lock
AccessExclusiveLock,REC_NOT_GAP RECORD: a AccessExclusiveLock,REC_NOT_GAP lock
AccessExclusiveLock,REC_NOT_GAP TABLE: a AccessExclusiveLock,REC_NOT_GAP lock
AccessExclusiveLock,REC_NOT_GAP transactionid: a row lock
RowExclusiveLock RECORD: a RowExclusiveLock
RowExclusiveLock TABLE: a RowExclusiveLock
RowExclusiveLock transactionid: a row lock
RowExclusiveLock,INSERT_INTENTION RECORD: a RowExclusiveLock,INSERT_INTENTION lock
RowExclusiveLock,INSERT_INTENTION TABLE: a RowExclusiveLock,INSERT_INTENTION lock
RowExclusiveLock,INSERT_INTENTION transactionid: a row lock
RowExclusiveLock,GAP RECORD: a RowExclusiveLock,GAP lock
RowExclusiveLock,GAP TABLE: a RowExclusiveLock,GAP lock
RowExclusiveLock,GAP transactionid: a row lock
RowExclusiveLock,REC_NOT_GAP RECORD: a RowExclusiveLock,REC_NOT_GAP lock
RowExclusiveLock,REC_NOT_GAP TABLE: a RowExclusiveLock,REC_NOT_GAP lock
RowExclusiveLock,REC_NOT_GAP transactionid: a row lock
SIREAD RECORD: a SIREAD lock
SIREAD TABLE: a SIREAD lock
SIREAD transactionid: a row lock
SIREAD,INSERT_INTENTION RECORD: a SIREAD,INSERT_INTENTION lock
SIREAD,INSERT_INTENTION TABLE: a SIREAD,INSERT_INTENTION lock
SIREAD,INSERT_INTENTION transactionid: a row lock
SIREAD,GAP RECORD: a SIREAD,GAP lock
SIREAD,GAP TABLE: a SIREAD,GAP lock
SIREAD,GAP transactionid: a row lock
SIREAD,REC_NOT_GAP RECORD: a SIREAD,REC_NOT_GAP lock
SIREAD,REC_NOT_GAP TABLE: a SIREAD,REC_NOT_GAP lock
SIREAD,REC_NOT_GAP transactionid: a row lock
object only: a lock on shop.orders

Mssql sleeping Some(48000.0) 1 LCK_M_S
Session 57 has been idle in an open transaction for 48.0 s; 1 session waits behind it (a shared lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. SET XACT_ABORT ON in the application makes an error roll the transaction back instead of leaving it open.

Mssql sleeping None 2 LCK_M_X
Session 57 is idle in an open transaction; 2 sessions wait behind it (an exclusive lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. SET XACT_ABORT ON in the application makes an error roll the transaction back instead of leaving it open.

Mssql sleeping None 1 PAGELATCH
Session 57 is idle in an open transaction; 1 session waits behind it (PAGELATCH).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. SET XACT_ABORT ON in the application makes an error roll the transaction back instead of leaving it open.

Mssql idle in transaction Some(48000.0) 1 LCK_M_S
Session 57 has been idle in an open transaction for 48.0 s; 1 session waits behind it (a shared lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. SET XACT_ABORT ON in the application makes an error roll the transaction back instead of leaving it open.

Mssql idle in transaction None 2 LCK_M_X
Session 57 is idle in an open transaction; 2 sessions wait behind it (an exclusive lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. SET XACT_ABORT ON in the application makes an error roll the transaction back instead of leaving it open.

Mssql idle in transaction None 1 PAGELATCH
Session 57 is idle in an open transaction; 1 session waits behind it (PAGELATCH).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. SET XACT_ABORT ON in the application makes an error roll the transaction back instead of leaving it open.

Mssql running Some(48000.0) 1 LCK_M_S
Session 57 has been running a statement for 48.0 s while holding locks; 1 session waits behind it (a shared lock on shop.orders).
A long statement is holding locks the others need. Look at its plan in Queries, and keep the transaction around it short: do reads and outside calls before it starts. The waiters only want to read: with READ_COMMITTED_SNAPSHOT on, readers use row versions and stop waiting on writers.

Mssql running None 2 LCK_M_X
Session 57 is running an INSERT statement while holding locks; 2 sessions wait behind it (an exclusive lock on shop.orders).
A long statement is holding locks the others need. Look at its plan in Queries, and keep the transaction around it short: do reads and outside calls before it starts.

Mssql running None 1 PAGELATCH
Session 57 is running a statement while holding locks; 1 session waits behind it (PAGELATCH).
A long statement is holding locks the others need. Look at its plan in Queries, and keep the transaction around it short: do reads and outside calls before it starts.

Mssql dormant Some(48000.0) 1 LCK_M_S
Session 57 holds locks without running anything; 1 session waits behind it (a shared lock on shop.orders).
Session 57 isn't running anything but still holds locks, typically from a transaction the client never finished. Ending it releases them.

Mssql dormant None 2 LCK_M_X
Session 57 holds locks without running anything; 2 sessions wait behind it (an exclusive lock on shop.orders).
Session 57 isn't running anything but still holds locks, typically from a transaction the client never finished. Ending it releases them.

Mssql dormant None 1 PAGELATCH
Session 57 holds locks without running anything; 1 session waits behind it (PAGELATCH).
Session 57 isn't running anything but still holds locks, typically from a transaction the client never finished. Ending it releases them.

Mssql unseen
Session 99, which this login can't see, holds the locks; 1 session waits behind it (an update lock).
The blocker belongs to another database or a login Culprit can't see; ask someone with VIEW SERVER STATE (or pg_read_all_stats / PROCESS) to look.

Postgres sleeping Some(48000.0) 1 LCK_M_S
Session 57 has been idle in an open transaction for 48.0 s; 1 session waits behind it (a shared lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Set idle_in_transaction_session_timeout so the server ends sessions that sit in a transaction.

Postgres sleeping None 2 LCK_M_X
Session 57 is idle in an open transaction; 2 sessions wait behind it (an exclusive lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Set idle_in_transaction_session_timeout so the server ends sessions that sit in a transaction.

Postgres sleeping None 1 PAGELATCH
Session 57 is idle in an open transaction; 1 session waits behind it (PAGELATCH).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Set idle_in_transaction_session_timeout so the server ends sessions that sit in a transaction.

Postgres idle in transaction Some(48000.0) 1 LCK_M_S
Session 57 has been idle in an open transaction for 48.0 s; 1 session waits behind it (a shared lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Set idle_in_transaction_session_timeout so the server ends sessions that sit in a transaction.

Postgres idle in transaction None 2 LCK_M_X
Session 57 is idle in an open transaction; 2 sessions wait behind it (an exclusive lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Set idle_in_transaction_session_timeout so the server ends sessions that sit in a transaction.

Postgres idle in transaction None 1 PAGELATCH
Session 57 is idle in an open transaction; 1 session waits behind it (PAGELATCH).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Set idle_in_transaction_session_timeout so the server ends sessions that sit in a transaction.

Postgres running Some(48000.0) 1 LCK_M_S
Session 57 has been running a statement for 48.0 s while holding locks; 1 session waits behind it (a shared lock on shop.orders).
A long statement is holding locks the others need. Look at its plan in Queries, and keep the transaction around it short: do reads and outside calls before it starts.

Postgres running None 2 LCK_M_X
Session 57 is running an INSERT statement while holding locks; 2 sessions wait behind it (an exclusive lock on shop.orders).
A long statement is holding locks the others need. Look at its plan in Queries, and keep the transaction around it short: do reads and outside calls before it starts.

Postgres running None 1 PAGELATCH
Session 57 is running a statement while holding locks; 1 session waits behind it (PAGELATCH).
A long statement is holding locks the others need. Look at its plan in Queries, and keep the transaction around it short: do reads and outside calls before it starts.

Postgres dormant Some(48000.0) 1 LCK_M_S
Session 57 holds locks without running anything; 1 session waits behind it (a shared lock on shop.orders).
Session 57 isn't running anything but still holds locks, typically from a transaction the client never finished. Ending it releases them.

Postgres dormant None 2 LCK_M_X
Session 57 holds locks without running anything; 2 sessions wait behind it (an exclusive lock on shop.orders).
Session 57 isn't running anything but still holds locks, typically from a transaction the client never finished. Ending it releases them.

Postgres dormant None 1 PAGELATCH
Session 57 holds locks without running anything; 1 session waits behind it (PAGELATCH).
Session 57 isn't running anything but still holds locks, typically from a transaction the client never finished. Ending it releases them.

Postgres unseen
Session 99, which this login can't see, holds the locks; 1 session waits behind it (an update lock).
The blocker belongs to another database or a login Culprit can't see; ask someone with VIEW SERVER STATE (or pg_read_all_stats / PROCESS) to look.

Yugabyte sleeping Some(48000.0) 1 LCK_M_S
Session 57 has been idle in an open transaction for 48.0 s; 1 session waits behind it (a shared lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Set idle_in_transaction_session_timeout so the server ends sessions that sit in a transaction.

Yugabyte sleeping None 2 LCK_M_X
Session 57 is idle in an open transaction; 2 sessions wait behind it (an exclusive lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Set idle_in_transaction_session_timeout so the server ends sessions that sit in a transaction.

Yugabyte sleeping None 1 PAGELATCH
Session 57 is idle in an open transaction; 1 session waits behind it (PAGELATCH).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Set idle_in_transaction_session_timeout so the server ends sessions that sit in a transaction.

Yugabyte idle in transaction Some(48000.0) 1 LCK_M_S
Session 57 has been idle in an open transaction for 48.0 s; 1 session waits behind it (a shared lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Set idle_in_transaction_session_timeout so the server ends sessions that sit in a transaction.

Yugabyte idle in transaction None 2 LCK_M_X
Session 57 is idle in an open transaction; 2 sessions wait behind it (an exclusive lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Set idle_in_transaction_session_timeout so the server ends sessions that sit in a transaction.

Yugabyte idle in transaction None 1 PAGELATCH
Session 57 is idle in an open transaction; 1 session waits behind it (PAGELATCH).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Set idle_in_transaction_session_timeout so the server ends sessions that sit in a transaction.

Yugabyte running Some(48000.0) 1 LCK_M_S
Session 57 has been running a statement for 48.0 s while holding locks; 1 session waits behind it (a shared lock on shop.orders).
A long statement is holding locks the others need. Look at its plan in Queries, and keep the transaction around it short: do reads and outside calls before it starts.

Yugabyte running None 2 LCK_M_X
Session 57 is running an INSERT statement while holding locks; 2 sessions wait behind it (an exclusive lock on shop.orders).
A long statement is holding locks the others need. Look at its plan in Queries, and keep the transaction around it short: do reads and outside calls before it starts.

Yugabyte running None 1 PAGELATCH
Session 57 is running a statement while holding locks; 1 session waits behind it (PAGELATCH).
A long statement is holding locks the others need. Look at its plan in Queries, and keep the transaction around it short: do reads and outside calls before it starts.

Yugabyte dormant Some(48000.0) 1 LCK_M_S
Session 57 holds locks without running anything; 1 session waits behind it (a shared lock on shop.orders).
Session 57 isn't running anything but still holds locks, typically from a transaction the client never finished. Ending it releases them.

Yugabyte dormant None 2 LCK_M_X
Session 57 holds locks without running anything; 2 sessions wait behind it (an exclusive lock on shop.orders).
Session 57 isn't running anything but still holds locks, typically from a transaction the client never finished. Ending it releases them.

Yugabyte dormant None 1 PAGELATCH
Session 57 holds locks without running anything; 1 session waits behind it (PAGELATCH).
Session 57 isn't running anything but still holds locks, typically from a transaction the client never finished. Ending it releases them.

Yugabyte unseen
Session 99, which this login can't see, holds the locks; 1 session waits behind it (an update lock).
The blocker belongs to another database or a login Culprit can't see; ask someone with VIEW SERVER STATE (or pg_read_all_stats / PROCESS) to look.

Mysql sleeping Some(48000.0) 1 LCK_M_S
Session 57 has been idle in an open transaction for 48.0 s; 1 session waits behind it (an update lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Keep transactions short; waiters give up after innodb_lock_wait_timeout (50 s by default).

Mysql sleeping None 2 LCK_M_X
Session 57 is idle in an open transaction; 2 sessions wait behind it (an update lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Keep transactions short; waiters give up after innodb_lock_wait_timeout (50 s by default).

Mysql sleeping None 1 PAGELATCH
Session 57 is idle in an open transaction; 1 session waits behind it.
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Keep transactions short; waiters give up after innodb_lock_wait_timeout (50 s by default).

Mysql idle in transaction Some(48000.0) 1 LCK_M_S
Session 57 has been idle in an open transaction for 48.0 s; 1 session waits behind it (an update lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Keep transactions short; waiters give up after innodb_lock_wait_timeout (50 s by default).

Mysql idle in transaction None 2 LCK_M_X
Session 57 is idle in an open transaction; 2 sessions wait behind it (an update lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Keep transactions short; waiters give up after innodb_lock_wait_timeout (50 s by default).

Mysql idle in transaction None 1 PAGELATCH
Session 57 is idle in an open transaction; 1 session waits behind it.
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Keep transactions short; waiters give up after innodb_lock_wait_timeout (50 s by default).

Mysql running Some(48000.0) 1 LCK_M_S
Session 57 has been running a statement for 48.0 s while holding locks; 1 session waits behind it (an update lock on shop.orders).
A long statement is holding locks the others need. Look at its plan in Queries, and keep the transaction around it short: do reads and outside calls before it starts.

Mysql running None 2 LCK_M_X
Session 57 is running an INSERT statement while holding locks; 2 sessions wait behind it (an update lock on shop.orders).
A long statement is holding locks the others need. Look at its plan in Queries, and keep the transaction around it short: do reads and outside calls before it starts.

Mysql running None 1 PAGELATCH
Session 57 is running a statement while holding locks; 1 session waits behind it.
A long statement is holding locks the others need. Look at its plan in Queries, and keep the transaction around it short: do reads and outside calls before it starts.

Mysql dormant Some(48000.0) 1 LCK_M_S
Session 57 holds locks without running anything; 1 session waits behind it (an update lock on shop.orders).
Session 57 isn't running anything but still holds locks, typically from a transaction the client never finished. Ending it releases them.

Mysql dormant None 2 LCK_M_X
Session 57 holds locks without running anything; 2 sessions wait behind it (an update lock on shop.orders).
Session 57 isn't running anything but still holds locks, typically from a transaction the client never finished. Ending it releases them.

Mysql dormant None 1 PAGELATCH
Session 57 holds locks without running anything; 1 session waits behind it.
Session 57 isn't running anything but still holds locks, typically from a transaction the client never finished. Ending it releases them.

Mysql unseen
Session 99, which this login can't see, holds the locks; 1 session waits behind it.
The blocker belongs to another database or a login Culprit can't see; ask someone with VIEW SERVER STATE (or pg_read_all_stats / PROCESS) to look.

Sqlite sleeping Some(48000.0) 1 LCK_M_S
Session 57 has been idle in an open transaction for 48.0 s; 1 session waits behind it (a shared lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Keep write transactions short: SQLite allows one writer at a time, and the others wait busy_timeout, then fail with SQLITE_BUSY.

Sqlite sleeping None 2 LCK_M_X
Session 57 is idle in an open transaction; 2 sessions wait behind it (an exclusive lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Keep write transactions short: SQLite allows one writer at a time, and the others wait busy_timeout, then fail with SQLITE_BUSY.

Sqlite sleeping None 1 PAGELATCH
Session 57 is idle in an open transaction; 1 session waits behind it (PAGELATCH).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Keep write transactions short: SQLite allows one writer at a time, and the others wait busy_timeout, then fail with SQLITE_BUSY.

Sqlite idle in transaction Some(48000.0) 1 LCK_M_S
Session 57 has been idle in an open transaction for 48.0 s; 1 session waits behind it (a shared lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Keep write transactions short: SQLite allows one writer at a time, and the others wait busy_timeout, then fail with SQLITE_BUSY.

Sqlite idle in transaction None 2 LCK_M_X
Session 57 is idle in an open transaction; 2 sessions wait behind it (an exclusive lock on shop.orders).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Keep write transactions short: SQLite allows one writer at a time, and the others wait busy_timeout, then fail with SQLITE_BUSY.

Sqlite idle in transaction None 1 PAGELATCH
Session 57 is idle in an open transaction; 1 session waits behind it (PAGELATCH).
The application opened a transaction and stopped sending statements: usually a missing COMMIT, an error path that skips the ROLLBACK, or slow work (an HTTP call, a user prompt) done while the transaction is open. Ending session 57 rolls its transaction back and frees everyone behind it. Keep write transactions short: SQLite allows one writer at a time, and the others wait busy_timeout, then fail with SQLITE_BUSY.

Sqlite running Some(48000.0) 1 LCK_M_S
Session 57 has been running a statement for 48.0 s while holding locks; 1 session waits behind it (a shared lock on shop.orders).
A long statement is holding locks the others need. Look at its plan in Queries, and keep the transaction around it short: do reads and outside calls before it starts.

Sqlite running None 2 LCK_M_X
Session 57 is running an INSERT statement while holding locks; 2 sessions wait behind it (an exclusive lock on shop.orders).
A long statement is holding locks the others need. Look at its plan in Queries, and keep the transaction around it short: do reads and outside calls before it starts.

Sqlite running None 1 PAGELATCH
Session 57 is running a statement while holding locks; 1 session waits behind it (PAGELATCH).
A long statement is holding locks the others need. Look at its plan in Queries, and keep the transaction around it short: do reads and outside calls before it starts.

Sqlite dormant Some(48000.0) 1 LCK_M_S
Session 57 holds locks without running anything; 1 session waits behind it (a shared lock on shop.orders).
Session 57 isn't running anything but still holds locks, typically from a transaction the client never finished. Ending it releases them.

Sqlite dormant None 2 LCK_M_X
Session 57 holds locks without running anything; 2 sessions wait behind it (an exclusive lock on shop.orders).
Session 57 isn't running anything but still holds locks, typically from a transaction the client never finished. Ending it releases them.

Sqlite dormant None 1 PAGELATCH
Session 57 holds locks without running anything; 1 session waits behind it (PAGELATCH).
Session 57 isn't running anything but still holds locks, typically from a transaction the client never finished. Ending it releases them.

Sqlite unseen
Session 99, which this login can't see, holds the locks; 1 session waits behind it (an update lock).
The blocker belongs to another database or a login Culprit can't see; ask someone with VIEW SERVER STATE (or pg_read_all_stats / PROCESS) to look."#
        );
    }

    #[test]
    fn postgres_row_locks_read_as_row_locks() {
        let l = LockWait {
            session: "9".into(),
            mode: Some("ShareLock".into()),
            resource: Some("transactionid".into()),
            object: Some("public.orders".into()),
            index: None,
        };
        assert_eq!(lock_phrase(Some("Lock: transactionid"), Some(&l)).unwrap(), "a row lock on public.orders");
    }
}
