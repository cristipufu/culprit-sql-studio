//! Deadlocks, explained: the processes and locks in the cycle, which pattern it is, and the fixes that make it go away.
//! Engines parse their own formats (SQL Server deadlock XML, InnoDB status, Postgres log lines) into this model;
//! `classify` names the pattern and writes the plain-language story.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::model::Engine;
use crate::{format, t};

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Default)]
#[ts(export)]
pub struct DeadlockProcess {
    /// The engine's id for the participant (`process1a2b`, `(1)`, a Postgres pid).
    pub id: String,
    /// The session (SPID, thread id, pid) when known.
    pub session: Option<String>,
    pub victim: bool,
    pub statement: Option<String>,
    /// Stored procedure or function the statement ran in.
    pub procedure: Option<String>,
    pub login: Option<String>,
    pub host: Option<String>,
    pub application: Option<String>,
    pub database: Option<String>,
    pub isolation: Option<String>,
    /// The lock mode it was waiting for.
    pub wait_mode: Option<String>,
    /// Transaction log the victim had written (SQL Server): bigger means more work rolled back.
    pub log_used: Option<f64>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Default)]
#[ts(export)]
pub struct LockHolder {
    pub process: String,
    pub mode: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Default)]
#[ts(export)]
pub struct DeadlockResource {
    pub id: String,
    /// `keylock`, `pagelock`, `objectlock`, `ridlock`, `exchangeEvent`, `RECORD`, `TABLE`, `transaction`.
    pub kind: String,
    /// `schema.table`, when known.
    pub object: Option<String>,
    pub index: Option<String>,
    pub holders: Vec<LockHolder>,
    pub waiters: Vec<LockHolder>,
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum DeadlockPattern {
    /// A read through a nonclustered (secondary) index plus a lookup, against an update of the same rows.
    KeyLookup,
    /// Both read the same rows, then both tried to change them.
    LockConversion,
    /// Each locked one thing, then wanted what the other had: opposite order.
    OppositeOrder,
    /// Row locks escalated to a table lock that collided with another transaction.
    LockEscalation,
    /// The threads of one parallel query waited on each other.
    Parallelism,
    /// A cycle that matches none of the above.
    Cycle,
}

impl DeadlockPattern {
    pub fn label(self) -> String {
        match self {
            DeadlockPattern::KeyLookup => t!("core.deadlock.pattern.key_lookup"),
            DeadlockPattern::LockConversion => t!("core.deadlock.pattern.lock_conversion"),
            DeadlockPattern::OppositeOrder => t!("core.deadlock.pattern.opposite_order"),
            DeadlockPattern::LockEscalation => t!("core.deadlock.pattern.lock_escalation"),
            DeadlockPattern::Parallelism => t!("core.deadlock.pattern.parallelism"),
            DeadlockPattern::Cycle => t!("core.deadlock.pattern.cycle"),
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Default)]
#[ts(export)]
pub struct DeadlockFix {
    pub title: String,
    pub detail: String,
    /// Copy-only SQL; Culprit never runs it.
    pub sql: Option<String>,
    /// A finding id that is the same fix, when the scan found it (e.g. `config:read-committed-snapshot`).
    pub finding: Option<String>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct Deadlock {
    /// Stable for the same event: time plus participants.
    pub id: String,
    /// When the engine detected it (RFC 3339 UTC), when known.
    pub at: Option<String>,
    /// Where it came from: `system_health`, `InnoDB status`, `pasted`.
    pub source: String,
    pub processes: Vec<DeadlockProcess>,
    pub resources: Vec<DeadlockResource>,
    pub pattern: DeadlockPattern,
    /// Tables involved, `schema.table`.
    pub objects: Vec<String>,
    pub headline: String,
    pub explanation: String,
    pub fixes: Vec<DeadlockFix>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Default)]
#[ts(export)]
pub struct DeadlockReport {
    /// Most recent first.
    pub deadlocks: Vec<Deadlock>,
    /// Where they were read from, in words.
    pub source: String,
    pub since_hours: f64,
    /// Deadlocks the server counted, when it keeps a counter but not the graphs (Postgres).
    pub counted: Option<f64>,
    /// Since when `counted` counts (statistics reset), RFC 3339.
    pub counted_since: Option<String>,
    /// Why there are no graphs, or what unlocks more.
    pub note: Option<String>,
}

impl Deadlock {
    /// A deadlock as an engine parsed it, before `classify` names it.
    pub fn new(
        source: &str,
        at: Option<String>,
        processes: Vec<DeadlockProcess>,
        resources: Vec<DeadlockResource>,
    ) -> Deadlock {
        let mut participants: Vec<String> =
            processes.iter().map(|p| p.session.clone().unwrap_or_else(|| p.id.clone())).collect();
        participants.sort();
        let id = format!("{:016x}", fnv(&format!("{}|{}", at.as_deref().unwrap_or(""), participants.join(","))));
        Deadlock {
            id,
            at,
            source: source.to_string(),
            processes,
            resources,
            pattern: DeadlockPattern::Cycle,
            objects: Vec::new(),
            headline: String::new(),
            explanation: String::new(),
            fixes: Vec::new(),
        }
    }

    pub fn victim(&self) -> Option<&DeadlockProcess> {
        self.processes.iter().find(|p| p.victim)
    }

    /// "session 61", or "process p1" without a session; `opening` at the start of a sentence ("Session 61").
    fn label(&self, process: &str, opening: bool) -> String {
        match (self.processes.iter().find(|p| p.id == process).and_then(|p| p.session.clone()), opening) {
            (Some(session), false) => t!("core.deadlock.participant.session", id = session),
            (Some(session), true) => t!("core.deadlock.participant.session_opening", id = session),
            (None, false) => t!("core.deadlock.participant.process", id = process),
            (None, true) => t!("core.deadlock.participant.process_opening", id = process),
        }
    }
}

fn fnv(text: &str) -> u64 {
    text.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

fn is_intent(mode: &str) -> bool {
    matches!(mode, "IS" | "IX" | "IU" | "SIX" | "SIU" | "UIX")
}

fn table_level(kind: &str) -> bool {
    let k = kind.to_ascii_lowercase();
    k == "objectlock" || k == "table" || k == "hobtlock"
}

fn row_level(kind: &str) -> bool {
    let k = kind.to_ascii_lowercase();
    k == "keylock" || k == "ridlock" || k == "pagelock" || k == "record" || k == "transaction" || k == "tuple"
}

/// Name the pattern and write the headline, explanation and fixes.
pub fn classify(engine: Engine, deadlock: &mut Deadlock) {
    let d = &*deadlock;
    let objects: Vec<String> =
        d.resources.iter().filter_map(|r| r.object.clone()).collect::<BTreeSet<_>>().into_iter().collect();
    let sessions: BTreeSet<&str> = d.processes.iter().filter_map(|p| p.session.as_deref()).collect();

    let parallel = d.resources.iter().any(|r| {
        let k = r.kind.to_ascii_lowercase();
        k == "exchangeevent" || k == "threadpool" || k == "resourcewait"
    }) || (sessions.len() == 1 && d.processes.len() > 1);
    let conversion = d.resources.iter().any(|r| {
        let holders: BTreeSet<&str> = r.holders.iter().map(|h| h.process.as_str()).collect();
        let waiters: BTreeSet<&str> = r.waiters.iter().map(|h| h.process.as_str()).collect();
        holders.len() >= 2 && holders.is_subset(&waiters)
    });
    let escalation = d.resources.iter().any(|r| {
        table_level(&r.kind)
            && r.holders.iter().chain(&r.waiters).any(|h| h.mode.as_deref().is_some_and(|m| !is_intent(m)))
    });
    let rows: Vec<&DeadlockResource> = d.resources.iter().filter(|r| row_level(&r.kind)).collect();
    let indexes_per_object: BTreeMap<&str, BTreeSet<&str>> = rows.iter().fold(BTreeMap::new(), |mut acc, r| {
        if let (Some(o), Some(i)) = (r.object.as_deref(), r.index.as_deref()) {
            acc.entry(o).or_insert_with(BTreeSet::new).insert(i);
        }
        acc
    });
    let key_lookup = indexes_per_object.values().any(|ixs| ixs.len() >= 2);

    let pattern = if parallel {
        DeadlockPattern::Parallelism
    } else if conversion {
        DeadlockPattern::LockConversion
    } else if key_lookup {
        DeadlockPattern::KeyLookup
    } else if escalation {
        DeadlockPattern::LockEscalation
    } else if d.resources.len() >= 2 && d.processes.len() >= 2 {
        DeadlockPattern::OppositeOrder
    } else {
        DeadlockPattern::Cycle
    };

    let victim = d.victim().map(|v| d.label(&v.id, false));
    let tables = if objects.is_empty() { t!("core.deadlock.tables.unknown") } else { format::list(&objects) };
    let (lookup_object, lookup_indexes) = indexes_per_object
        .iter()
        .find(|(_, ixs)| ixs.len() >= 2)
        .map(|(o, ixs)| (o.to_string(), ixs.iter().map(|s| s.to_string()).collect::<Vec<_>>()))
        .unwrap_or_default();
    let secondary = lookup_indexes
        .iter()
        .find(|i| {
            let l = i.to_ascii_lowercase();
            !(l.starts_with("pk") || l == "primary" || l.contains("clust"))
        })
        .cloned();

    let n = d.processes.len().max(2);
    let headline = match (pattern, victim) {
        (DeadlockPattern::KeyLookup, None) => match &secondary {
            Some(index) => t!("core.deadlock.headline.key_lookup", table = &lookup_object, index = index),
            None => t!("core.deadlock.headline.key_lookup_unnamed", table = &lookup_object),
        },
        (DeadlockPattern::KeyLookup, Some(victim)) => match &secondary {
            Some(index) => {
                t!("core.deadlock.headline.key_lookup_victim", table = &lookup_object, index = index, victim = victim)
            }
            None => t!("core.deadlock.headline.key_lookup_unnamed_victim", table = &lookup_object, victim = victim),
        },
        (DeadlockPattern::LockConversion, None) => t!("core.deadlock.headline.lock_conversion", tables = &tables),
        (DeadlockPattern::LockConversion, Some(victim)) => {
            t!("core.deadlock.headline.lock_conversion_victim", tables = &tables, victim = victim)
        }
        (DeadlockPattern::OppositeOrder, None) => t!("core.deadlock.headline.opposite_order", tables = &tables),
        (DeadlockPattern::OppositeOrder, Some(victim)) => {
            t!("core.deadlock.headline.opposite_order_victim", tables = &tables, victim = victim)
        }
        (DeadlockPattern::LockEscalation, None) => t!("core.deadlock.headline.lock_escalation", tables = &tables),
        (DeadlockPattern::LockEscalation, Some(victim)) => {
            t!("core.deadlock.headline.lock_escalation_victim", tables = &tables, victim = victim)
        }
        (DeadlockPattern::Parallelism, _) => t!("core.deadlock.headline.parallelism"),
        (DeadlockPattern::Cycle, None) => {
            t!("core.deadlock.headline.cycle", n = n, count = n.to_string(), tables = &tables)
        }
        (DeadlockPattern::Cycle, Some(victim)) => {
            t!("core.deadlock.headline.cycle_victim", n = n, count = n.to_string(), tables = &tables, victim = victim)
        }
    };

    let explanation = match pattern {
        DeadlockPattern::KeyLookup => match &secondary {
            Some(index) => t!("core.deadlock.explanation.key_lookup", index = index),
            None => t!("core.deadlock.explanation.key_lookup_unnamed"),
        },
        DeadlockPattern::LockConversion => t!("core.deadlock.explanation.lock_conversion"),
        DeadlockPattern::OppositeOrder => match who_held_what(d).or_else(|| who_ran_what(d)) {
            Some(story) => t!("core.deadlock.explanation.opposite_order_story", story = story),
            None => t!("core.deadlock.explanation.opposite_order"),
        },
        DeadlockPattern::LockEscalation => t!("core.deadlock.explanation.lock_escalation"),
        DeadlockPattern::Parallelism => t!("core.deadlock.explanation.parallelism"),
        DeadlockPattern::Cycle => t!("core.deadlock.explanation.cycle"),
    };

    let mut fixes = match (pattern, engine) {
        (DeadlockPattern::KeyLookup, Engine::Mssql) => vec![
            DeadlockFix {
                title: t!("core.deadlock.fix.skip_lookup.title"),
                detail: match &secondary {
                    Some(index) => t!("core.deadlock.fix.skip_lookup.detail", index = index),
                    None => t!("core.deadlock.fix.skip_lookup.detail_unnamed"),
                },
                sql: None,
                finding: None,
            },
            rcsi(d),
        ],
        (DeadlockPattern::KeyLookup, _) => vec![DeadlockFix {
            title: t!("core.deadlock.fix.primary_key.title"),
            detail: t!("core.deadlock.fix.primary_key.detail"),
            sql: None,
            finding: None,
        }],
        (DeadlockPattern::LockConversion, engine) => vec![DeadlockFix {
            title: t!("core.deadlock.fix.write_lock.title"),
            detail: t!("core.deadlock.fix.write_lock.detail"),
            sql: Some(match engine {
                Engine::Mssql => "SELECT … FROM … WITH (UPDLOCK, ROWLOCK) WHERE …;".into(),
                _ => "SELECT … FROM … WHERE … FOR UPDATE;".into(),
            }),
            finding: None,
        }],
        (DeadlockPattern::OppositeOrder | DeadlockPattern::Cycle, _) => vec![
            DeadlockFix {
                title: t!("core.deadlock.fix.same_order.title"),
                detail: t!("core.deadlock.fix.same_order.detail", tables = &tables),
                sql: None,
                finding: None,
            },
            DeadlockFix {
                title: t!("core.deadlock.fix.short_transactions.title"),
                detail: t!("core.deadlock.fix.short_transactions.detail"),
                sql: None,
                finding: None,
            },
        ],
        (DeadlockPattern::LockEscalation, _) => vec![DeadlockFix {
            title: t!("core.deadlock.fix.fewer_rows.title"),
            detail: t!("core.deadlock.fix.fewer_rows.detail"),
            sql: None,
            finding: None,
        }],
        (DeadlockPattern::Parallelism, _) => vec![DeadlockFix {
            title: t!("core.deadlock.fix.parallel_plan.title"),
            detail: t!("core.deadlock.fix.parallel_plan.detail"),
            sql: None,
            finding: Some("config:cost-threshold-for-parallelism".into()),
        }],
    };
    if pattern != DeadlockPattern::Parallelism {
        // What the application catches: the engine's error number, or the code it reports a deadlock with.
        let detail = match engine {
            Engine::Mssql => t!("core.deadlock.fix.retry.detail_error", error = "1205"),
            Engine::Mysql => t!("core.deadlock.fix.retry.detail_error", error = "1213"),
            Engine::Postgres | Engine::Yugabyte => t!("core.deadlock.fix.retry.detail_code", code = "SQLSTATE 40P01"),
            Engine::Sqlite => t!("core.deadlock.fix.retry.detail_code", code = "SQLITE_BUSY"),
        };
        fixes.push(DeadlockFix { title: t!("core.deadlock.fix.retry.title"), detail, sql: None, finding: None });
    }

    deadlock.pattern = pattern;
    deadlock.objects = objects;
    deadlock.headline = headline;
    deadlock.explanation = explanation;
    deadlock.fixes = fixes;
}

/// "Session 73 held shop.orders and wanted shop.customers; session 75 held shop.customers and wanted shop.orders".
fn who_held_what(d: &Deadlock) -> Option<String> {
    // Without table names (Postgres names row locks by transaction id) the statements tell it better.
    if d.resources.iter().any(|r| r.object.is_none()) {
        return None;
    }
    // What a lock is on: the table, and the index when the table has more than one locked resource.
    let thing = |r: &DeadlockResource| -> (Option<String>, Option<String>) {
        match (&r.object, &r.index) {
            (Some(o), Some(i)) if d.resources.iter().filter(|x| x.object.as_ref() == Some(o)).count() > 1 => {
                (Some(o.clone()), Some(i.clone()))
            }
            (object, _) => (object.clone(), None),
        }
    };
    let words = |(object, index): (Option<String>, Option<String>), r: &DeadlockResource| match (object, index) {
        (Some(table), Some(index)) => t!("core.deadlock.story.row_of", table = table, index = index),
        (Some(table), None) => table,
        (None, _) => r.kind.clone(),
    };
    let mut parts = Vec::new();
    for p in &d.processes {
        let held = d.resources.iter().find(|r| r.holders.iter().any(|h| h.process == p.id))?;
        let wanted = d.resources.iter().find(|r| r.waiters.iter().any(|h| h.process == p.id))?;
        if thing(held) == thing(wanted) {
            return None;
        }
        parts.push(t!(
            "core.deadlock.story.held",
            who = d.label(&p.id, parts.is_empty()),
            held = words(thing(held), held),
            wanted = words(thing(wanted), wanted)
        ));
    }
    // "Session 61 held …; session 64 held …".
    let last = parts.pop()?;
    Some(parts.into_iter().rev().fold(last, |rest, first| t!("core.deadlock.story.then", first = first, rest = rest)))
}

/// "Each transaction had already changed rows the other one wanted next: session 539 was running “UPDATE customers …”
/// and session 545 “UPDATE orders …”".
fn who_ran_what(d: &Deadlock) -> Option<String> {
    let clip = |sql: &str| {
        let flat = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        if flat.chars().count() > 60 { format!("{}…", flat.chars().take(59).collect::<String>()) } else { flat }
    };
    let [a, b] = d.processes.as_slice() else { return None };
    let (sa, sb) = (a.statement.as_deref()?, b.statement.as_deref()?);
    Some(t!(
        "core.deadlock.story.ran",
        first = d.label(&a.id, false),
        first_sql = clip(sa),
        second = d.label(&b.id, false),
        second_sql = clip(sb)
    ))
}

fn rcsi(d: &Deadlock) -> DeadlockFix {
    let database = d.processes.iter().find_map(|p| p.database.clone());
    DeadlockFix {
        title: t!("core.deadlock.fix.rcsi.title"),
        detail: t!("core.deadlock.fix.rcsi.detail"),
        sql: database.map(|db| {
            format!(
                "ALTER DATABASE [{}] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE;",
                db.replace(']', "]]")
            )
        }),
        finding: Some("config:read-committed-snapshot".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(id: &str, session: &str, victim: bool) -> DeadlockProcess {
        DeadlockProcess {
            id: id.into(),
            session: Some(session.into()),
            victim,
            database: Some("shop".into()),
            ..Default::default()
        }
    }

    fn resource(kind: &str, object: &str, index: &str, holder: (&str, &str), waiter: (&str, &str)) -> DeadlockResource {
        DeadlockResource {
            id: format!("{kind}-{object}-{index}"),
            kind: kind.into(),
            object: Some(object.into()),
            index: Some(index.into()),
            holders: vec![LockHolder { process: holder.0.into(), mode: Some(holder.1.into()) }],
            waiters: vec![LockHolder { process: waiter.0.into(), mode: Some(waiter.1.into()) }],
        }
    }

    #[test]
    fn a_key_lookup_deadlock_is_named_and_offers_a_covering_index_and_rcsi() {
        let mut d = Deadlock::new(
            "pasted",
            Some("2026-10-11T12:00:00Z".into()),
            vec![process("p1", "61", true), process("p2", "64", false)],
            vec![
                resource("keylock", "shop.customers", "ix_customers_email", ("p1", "S"), ("p2", "X")),
                resource("keylock", "shop.customers", "PK_customers", ("p2", "X"), ("p1", "S")),
            ],
        );
        classify(Engine::Mssql, &mut d);
        assert_eq!(d.pattern, DeadlockPattern::KeyLookup);
        assert!(d.headline.starts_with("Key-lookup deadlock on shop.customers: a read through ix_customers_email"));
        assert!(d.headline.contains("session 61's transaction was rolled back"));
        assert!(d.fixes.iter().any(|f| f.finding.as_deref() == Some("config:read-committed-snapshot")));
        assert!(d.fixes.iter().any(|f| f.sql.as_deref().is_some_and(|s| s.contains("[shop]"))));
    }

    #[test]
    fn opposite_order_conversion_and_parallelism() {
        let mut d = Deadlock::new(
            "pasted",
            None,
            vec![process("p1", "61", true), process("p2", "64", false)],
            vec![
                resource("keylock", "shop.orders", "PK_orders", ("p1", "X"), ("p2", "X")),
                resource("keylock", "shop.customers", "PK_customers", ("p2", "X"), ("p1", "X")),
            ],
        );
        classify(Engine::Mssql, &mut d);
        assert_eq!(d.pattern, DeadlockPattern::OppositeOrder);
        assert!(d.headline.contains("shop.customers and shop.orders in opposite order"));

        let shared = DeadlockResource {
            id: "r".into(),
            kind: "keylock".into(),
            object: Some("shop.orders".into()),
            index: Some("PK_orders".into()),
            holders: vec![
                LockHolder { process: "p1".into(), mode: Some("S".into()) },
                LockHolder { process: "p2".into(), mode: Some("S".into()) },
            ],
            waiters: vec![
                LockHolder { process: "p1".into(), mode: Some("X".into()) },
                LockHolder { process: "p2".into(), mode: Some("X".into()) },
            ],
        };
        let mut c =
            Deadlock::new("pasted", None, vec![process("p1", "61", false), process("p2", "64", true)], vec![shared]);
        classify(Engine::Mysql, &mut c);
        assert_eq!(c.pattern, DeadlockPattern::LockConversion);
        assert!(c.fixes[0].sql.as_deref().unwrap().contains("FOR UPDATE"));

        let mut p = Deadlock::new(
            "pasted",
            None,
            vec![process("p1", "70", false), process("p2", "70", false)],
            vec![DeadlockResource { id: "e".into(), kind: "exchangeEvent".into(), ..Default::default() }],
        );
        classify(Engine::Mssql, &mut p);
        assert_eq!(p.pattern, DeadlockPattern::Parallelism);
    }

    /// Every pattern, with and without a victim, a session or tables, as each engine words it.
    fn every_case() -> Vec<(&'static str, Deadlock)> {
        let held =
            |kind: &str, object: Option<&str>, index: Option<&str>, holder: &str, waiter: &str| DeadlockResource {
                id: format!("{kind}{object:?}{index:?}{holder}"),
                kind: kind.into(),
                object: object.map(str::to_string),
                index: index.map(str::to_string),
                holders: vec![LockHolder { process: holder.into(), mode: Some("X".into()) }],
                waiters: vec![LockHolder { process: waiter.into(), mode: Some("X".into()) }],
            };
        let two = |victim: bool| vec![process("p1", "61", victim), process("p2", "64", false)];
        let mut cases = vec![
            (
                "key lookup",
                Deadlock::new(
                    "pasted",
                    None,
                    two(true),
                    vec![
                        held("keylock", Some("shop.customers"), Some("ix_email"), "p1", "p2"),
                        held("keylock", Some("shop.customers"), Some("PK_customers"), "p2", "p1"),
                    ],
                ),
            ),
            (
                "key lookup, no secondary named, no victim",
                Deadlock::new(
                    "pasted",
                    None,
                    two(false),
                    vec![
                        held("keylock", Some("shop.customers"), Some("PK_customers"), "p1", "p2"),
                        held("keylock", Some("shop.customers"), Some("ix_clustered"), "p2", "p1"),
                    ],
                ),
            ),
            (
                "conversion",
                Deadlock::new(
                    "pasted",
                    None,
                    two(true),
                    vec![DeadlockResource {
                        id: "r".into(),
                        kind: "keylock".into(),
                        object: Some("shop.orders".into()),
                        index: Some("PK_orders".into()),
                        holders: vec![
                            LockHolder { process: "p1".into(), mode: Some("S".into()) },
                            LockHolder { process: "p2".into(), mode: Some("S".into()) },
                        ],
                        waiters: vec![
                            LockHolder { process: "p1".into(), mode: Some("X".into()) },
                            LockHolder { process: "p2".into(), mode: Some("X".into()) },
                        ],
                    }],
                ),
            ),
            (
                "opposite order, who held what",
                Deadlock::new(
                    "pasted",
                    None,
                    two(true),
                    vec![
                        held("keylock", Some("shop.orders"), Some("PK_orders"), "p1", "p2"),
                        held("keylock", Some("shop.customers"), Some("PK_customers"), "p2", "p1"),
                    ],
                ),
            ),
            (
                "opposite order, rows of one table",
                Deadlock::new(
                    "pasted",
                    None,
                    vec![
                        DeadlockProcess { session: None, ..process("p1", "", true) },
                        DeadlockProcess { session: None, ..process("p2", "", false) },
                    ],
                    vec![
                        held("metadata", Some("shop.orders"), Some("ix_a"), "p1", "p2"),
                        held("metadata", Some("shop.orders"), Some("ix_b"), "p2", "p1"),
                    ],
                ),
            ),
            (
                "opposite order, who ran what",
                Deadlock::new(
                    "pasted",
                    None,
                    vec![
                        DeadlockProcess {
                            statement: Some("UPDATE customers SET name = $1 WHERE id = $2".into()),
                            ..process("p1", "539", true)
                        },
                        DeadlockProcess {
                            statement: Some(format!("UPDATE orders SET note = '{}' WHERE id = $1", "x".repeat(80))),
                            ..process("p2", "545", false)
                        },
                    ],
                    vec![held("transaction", None, None, "p1", "p2"), held("transaction", None, None, "p2", "p1")],
                ),
            ),
            (
                "opposite order, nothing named",
                Deadlock::new(
                    "pasted",
                    None,
                    two(false),
                    vec![held("transaction", None, None, "p1", "p2"), held("transaction", None, None, "p2", "p1")],
                ),
            ),
            (
                "escalation",
                Deadlock::new(
                    "pasted",
                    None,
                    two(true),
                    vec![held("objectlock", Some("shop.orders"), None, "p1", "p2")],
                ),
            ),
            (
                "parallelism",
                Deadlock::new(
                    "pasted",
                    None,
                    vec![process("p1", "70", false), process("p2", "70", false)],
                    vec![DeadlockResource { id: "e".into(), kind: "exchangeEvent".into(), ..Default::default() }],
                ),
            ),
            (
                "cycle of three on three tables",
                Deadlock::new(
                    "pasted",
                    None,
                    vec![process("p1", "61", true), process("p2", "64", false), process("p3", "66", false)],
                    vec![held("keylock", Some("shop.a"), Some("PK_a"), "p1", "p2")],
                ),
            ),
        ];
        let mut three = cases.last().unwrap().1.clone();
        three.resources[0].object = None;
        three.processes[0].victim = false;
        cases.push(("cycle, nothing named", three));
        let tables = Deadlock::new(
            "pasted",
            None,
            vec![
                process("p1", "61", true),
                DeadlockProcess { session: None, ..process("p2", "", false) },
                process("p3", "66", false),
            ],
            vec![
                held("keylock", Some("shop.a"), Some("PK_a"), "p1", "p2"),
                held("keylock", Some("shop.b"), Some("PK_b"), "p2", "p3"),
                held("keylock", Some("shop.c"), Some("PK_c"), "p3", "p1"),
            ],
        );
        cases.push(("three tables, a process with no session", tables));
        cases
    }

    // What a deadlock says, word for word, for every pattern on every engine: its messages come from locales/en.json.
    #[test]
    fn every_pattern_says_what_happened_in_english() {
        let mut out = Vec::new();
        for pattern in [
            DeadlockPattern::KeyLookup,
            DeadlockPattern::LockConversion,
            DeadlockPattern::OppositeOrder,
            DeadlockPattern::LockEscalation,
            DeadlockPattern::Parallelism,
            DeadlockPattern::Cycle,
        ] {
            out.push(format!("{pattern:?}: {}", pattern.label()));
        }
        for engine in Engine::ALL {
            // Every pattern on SQL Server; elsewhere the cases whose fixes are the engine's own.
            for (name, case) in every_case()
                .into_iter()
                .filter(|(name, _)| engine == Engine::Mssql || matches!(*name, "key lookup" | "conversion"))
            {
                let mut d = case.clone();
                classify(engine, &mut d);
                out.push(format!("\n## {engine:?} {name}: {:?}\n{}\n{}", d.pattern, d.headline, d.explanation));
                for fix in &d.fixes {
                    out.push(format!(
                        "- {} | {} | {} | {}",
                        fix.title,
                        fix.detail,
                        fix.sql.as_deref().unwrap_or("-"),
                        fix.finding.as_deref().unwrap_or("-")
                    ));
                }
            }
        }
        let got = out.join("\n");
        assert_eq!(
            got,
            r#"KeyLookup: key-lookup deadlock
LockConversion: read-then-update
OppositeOrder: opposite access order
LockEscalation: lock escalation
Parallelism: intra-query parallelism
Cycle: lock cycle

## Mssql key lookup: KeyLookup
Key-lookup deadlock on shop.customers: a read through ix_email and a write to the same rows took their locks in opposite order; session 61's transaction was rolled back.
The read finds rows through ix_email and then looks each one up in the table's main index; the write changes the main index first and then ix_email. Each ends up holding the lock the other needs next. It recurs whenever the two statements touch the same rows at the same time.
- Make the read skip the lookup | Add the columns the read needs to ix_email (as INCLUDE columns) so it never touches the main index the write locks. The query's plan in Queries shows the Key Lookup and its output columns. | - | -
- Let reads stop taking shared locks | With READ_COMMITTED_SNAPSHOT on, reads use row versions instead of shared locks, so a read can't be part of this cycle. It costs some tempdb space for the versions. | ALTER DATABASE [shop] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE; | config:read-committed-snapshot
- Retry the victim | A deadlock victim can be retried safely; the application should catch error 1205 and run the transaction again. | - | -

## Mssql key lookup, no secondary named, no victim: KeyLookup
Key-lookup deadlock on shop.customers: a read through a secondary index and a write to the same rows took their locks in opposite order.
The read finds rows through a secondary index and then looks each one up in the table's main index; the write changes the main index first and then the secondary index. Each ends up holding the lock the other needs next. It recurs whenever the two statements touch the same rows at the same time.
- Make the read skip the lookup | Add the columns the read needs to the index it seeks (as INCLUDE columns) so it never touches the main index the write locks. The query's plan in Queries shows the Key Lookup and its output columns. | - | -
- Let reads stop taking shared locks | With READ_COMMITTED_SNAPSHOT on, reads use row versions instead of shared locks, so a read can't be part of this cycle. It costs some tempdb space for the versions. | ALTER DATABASE [shop] SET READ_COMMITTED_SNAPSHOT ON WITH ROLLBACK IMMEDIATE; | config:read-committed-snapshot
- Retry the victim | A deadlock victim can be retried safely; the application should catch error 1205 and run the transaction again. | - | -

## Mssql conversion: LockConversion
Two transactions read the same rows of shop.orders, then both tried to change them; session 61's transaction was rolled back.
Both took a shared lock to read, then asked for an exclusive lock to write. A shared lock lets the other reader in, so neither can upgrade: whoever asks second deadlocks. Typical of 'read, check, then update' code.
- Take the write lock when you read | Read the rows you are about to change with an update lock, or do the check and the change in one UPDATE … WHERE statement. | SELECT … FROM … WITH (UPDLOCK, ROWLOCK) WHERE …; | -
- Retry the victim | A deadlock victim can be retried safely; the application should catch error 1205 and run the transaction again. | - | -

## Mssql opposite order, who held what: OppositeOrder
Two transactions locked shop.customers and shop.orders in opposite order and waited on each other; session 61's transaction was rolled back.
Session 61 held shop.orders and wanted shop.customers; session 64 held shop.customers and wanted shop.orders. With the same order in both, the second would simply wait instead of deadlocking.
- Touch tables and rows in the same order everywhere | Make every code path that changes shop.customers and shop.orders do it in one order (for example, always the parent row first, and rows sorted by key). Then the second transaction waits instead of deadlocking. | - | -
- Keep transactions short | Do reads, validation and outside calls before the transaction starts; hold locks only for the writes. | - | -
- Retry the victim | A deadlock victim can be retried safely; the application should catch error 1205 and run the transaction again. | - | -

## Mssql opposite order, rows of one table: OppositeOrder
Two transactions locked shop.orders in opposite order and waited on each other; process p1's transaction was rolled back.
Process p1 held a row of shop.orders (ix_a) and wanted a row of shop.orders (ix_b); process p2 held a row of shop.orders (ix_b) and wanted a row of shop.orders (ix_a). With the same order in both, the second would simply wait instead of deadlocking.
- Touch tables and rows in the same order everywhere | Make every code path that changes shop.orders do it in one order (for example, always the parent row first, and rows sorted by key). Then the second transaction waits instead of deadlocking. | - | -
- Keep transactions short | Do reads, validation and outside calls before the transaction starts; hold locks only for the writes. | - | -
- Retry the victim | A deadlock victim can be retried safely; the application should catch error 1205 and run the transaction again. | - | -

## Mssql opposite order, who ran what: OppositeOrder
Two transactions locked the same data in opposite order and waited on each other; session 539's transaction was rolled back.
Each transaction had already changed rows the other one wanted next: session 539 was running “UPDATE customers SET name = $1 WHERE id = $2” and session 545 “UPDATE orders SET note = 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx…”. With the same order in both, the second would simply wait instead of deadlocking.
- Touch tables and rows in the same order everywhere | Make every code path that changes the same data do it in one order (for example, always the parent row first, and rows sorted by key). Then the second transaction waits instead of deadlocking. | - | -
- Keep transactions short | Do reads, validation and outside calls before the transaction starts; hold locks only for the writes. | - | -
- Retry the victim | A deadlock victim can be retried safely; the application should catch error 1205 and run the transaction again. | - | -

## Mssql opposite order, nothing named: OppositeOrder
Two transactions locked the same data in opposite order and waited on each other.
Each transaction locked one thing and then wanted what the other had already locked. With the same order in both, the second would simply wait instead of deadlocking.
- Touch tables and rows in the same order everywhere | Make every code path that changes the same data do it in one order (for example, always the parent row first, and rows sorted by key). Then the second transaction waits instead of deadlocking. | - | -
- Keep transactions short | Do reads, validation and outside calls before the transaction starts; hold locks only for the writes. | - | -
- Retry the victim | A deadlock victim can be retried safely; the application should catch error 1205 and run the transaction again. | - | -

## Mssql escalation: LockEscalation
A table-wide lock on shop.orders (lock escalation) collided with another transaction; session 61's transaction was rolled back.
SQL Server turned thousands of row locks into one lock on the whole table to save memory. That table lock blocks everyone touching the table, so an otherwise harmless overlap became a cycle.
- Change fewer rows per statement | Modify in batches (for example 2,000 rows at a time) and index the WHERE columns, so SQL Server keeps row locks instead of locking the whole table. | - | -
- Retry the victim | A deadlock victim can be retried safely; the application should catch error 1205 and run the transaction again. | - | -

## Mssql parallelism: Parallelism
A parallel query deadlocked with its own threads; SQL Server broke the cycle, but the query lost time.
Threads of the same parallel plan exchange rows and waited on each other. These are usually a symptom of a plan that goes parallel when it shouldn't (a scan that an index would avoid).
- Fix the plan that goes parallel | Find the query in Queries: usually a scan an index would avoid. As a stopgap, OPTION (MAXDOP 1) on that query, or raise cost threshold for parallelism if it is still 5. | - | config:cost-threshold-for-parallelism

## Mssql cycle of three on three tables: Cycle
3 transactions waited on each other in a cycle on shop.a; session 61's transaction was rolled back.
Each transaction holds a lock the next one needs. Consistent access order and shorter transactions break most cycles.
- Touch tables and rows in the same order everywhere | Make every code path that changes shop.a do it in one order (for example, always the parent row first, and rows sorted by key). Then the second transaction waits instead of deadlocking. | - | -
- Keep transactions short | Do reads, validation and outside calls before the transaction starts; hold locks only for the writes. | - | -
- Retry the victim | A deadlock victim can be retried safely; the application should catch error 1205 and run the transaction again. | - | -

## Mssql cycle, nothing named: Cycle
3 transactions waited on each other in a cycle on the same data.
Each transaction holds a lock the next one needs. Consistent access order and shorter transactions break most cycles.
- Touch tables and rows in the same order everywhere | Make every code path that changes the same data do it in one order (for example, always the parent row first, and rows sorted by key). Then the second transaction waits instead of deadlocking. | - | -
- Keep transactions short | Do reads, validation and outside calls before the transaction starts; hold locks only for the writes. | - | -
- Retry the victim | A deadlock victim can be retried safely; the application should catch error 1205 and run the transaction again. | - | -

## Mssql three tables, a process with no session: OppositeOrder
Two transactions locked shop.a, shop.b and shop.c in opposite order and waited on each other; session 61's transaction was rolled back.
Session 61 held shop.a and wanted shop.c; process p2 held shop.b and wanted shop.a; session 66 held shop.c and wanted shop.b. With the same order in both, the second would simply wait instead of deadlocking.
- Touch tables and rows in the same order everywhere | Make every code path that changes shop.a, shop.b and shop.c do it in one order (for example, always the parent row first, and rows sorted by key). Then the second transaction waits instead of deadlocking. | - | -
- Keep transactions short | Do reads, validation and outside calls before the transaction starts; hold locks only for the writes. | - | -
- Retry the victim | A deadlock victim can be retried safely; the application should catch error 1205 and run the transaction again. | - | -

## Postgres key lookup: KeyLookup
Key-lookup deadlock on shop.customers: a read through ix_email and a write to the same rows took their locks in opposite order; session 61's transaction was rolled back.
The read finds rows through ix_email and then looks each one up in the table's main index; the write changes the main index first and then ix_email. Each ends up holding the lock the other needs next. It recurs whenever the two statements touch the same rows at the same time.
- Change rows through the primary key | Look up the primary key values first, then update or lock by primary key, so both statements lock the main index in the same order. | - | -
- Retry the victim | A deadlock victim can be retried safely; the application should catch SQLSTATE 40P01 and run the transaction again. | - | -

## Postgres conversion: LockConversion
Two transactions read the same rows of shop.orders, then both tried to change them; session 61's transaction was rolled back.
Both took a shared lock to read, then asked for an exclusive lock to write. A shared lock lets the other reader in, so neither can upgrade: whoever asks second deadlocks. Typical of 'read, check, then update' code.
- Take the write lock when you read | Read the rows you are about to change with an update lock, or do the check and the change in one UPDATE … WHERE statement. | SELECT … FROM … WHERE … FOR UPDATE; | -
- Retry the victim | A deadlock victim can be retried safely; the application should catch SQLSTATE 40P01 and run the transaction again. | - | -

## Yugabyte key lookup: KeyLookup
Key-lookup deadlock on shop.customers: a read through ix_email and a write to the same rows took their locks in opposite order; session 61's transaction was rolled back.
The read finds rows through ix_email and then looks each one up in the table's main index; the write changes the main index first and then ix_email. Each ends up holding the lock the other needs next. It recurs whenever the two statements touch the same rows at the same time.
- Change rows through the primary key | Look up the primary key values first, then update or lock by primary key, so both statements lock the main index in the same order. | - | -
- Retry the victim | A deadlock victim can be retried safely; the application should catch SQLSTATE 40P01 and run the transaction again. | - | -

## Yugabyte conversion: LockConversion
Two transactions read the same rows of shop.orders, then both tried to change them; session 61's transaction was rolled back.
Both took a shared lock to read, then asked for an exclusive lock to write. A shared lock lets the other reader in, so neither can upgrade: whoever asks second deadlocks. Typical of 'read, check, then update' code.
- Take the write lock when you read | Read the rows you are about to change with an update lock, or do the check and the change in one UPDATE … WHERE statement. | SELECT … FROM … WHERE … FOR UPDATE; | -
- Retry the victim | A deadlock victim can be retried safely; the application should catch SQLSTATE 40P01 and run the transaction again. | - | -

## Mysql key lookup: KeyLookup
Key-lookup deadlock on shop.customers: a read through ix_email and a write to the same rows took their locks in opposite order; session 61's transaction was rolled back.
The read finds rows through ix_email and then looks each one up in the table's main index; the write changes the main index first and then ix_email. Each ends up holding the lock the other needs next. It recurs whenever the two statements touch the same rows at the same time.
- Change rows through the primary key | Look up the primary key values first, then update or lock by primary key, so both statements lock the main index in the same order. | - | -
- Retry the victim | A deadlock victim can be retried safely; the application should catch error 1213 and run the transaction again. | - | -

## Mysql conversion: LockConversion
Two transactions read the same rows of shop.orders, then both tried to change them; session 61's transaction was rolled back.
Both took a shared lock to read, then asked for an exclusive lock to write. A shared lock lets the other reader in, so neither can upgrade: whoever asks second deadlocks. Typical of 'read, check, then update' code.
- Take the write lock when you read | Read the rows you are about to change with an update lock, or do the check and the change in one UPDATE … WHERE statement. | SELECT … FROM … WHERE … FOR UPDATE; | -
- Retry the victim | A deadlock victim can be retried safely; the application should catch error 1213 and run the transaction again. | - | -

## Sqlite key lookup: KeyLookup
Key-lookup deadlock on shop.customers: a read through ix_email and a write to the same rows took their locks in opposite order; session 61's transaction was rolled back.
The read finds rows through ix_email and then looks each one up in the table's main index; the write changes the main index first and then ix_email. Each ends up holding the lock the other needs next. It recurs whenever the two statements touch the same rows at the same time.
- Change rows through the primary key | Look up the primary key values first, then update or lock by primary key, so both statements lock the main index in the same order. | - | -
- Retry the victim | A deadlock victim can be retried safely; the application should catch SQLITE_BUSY and run the transaction again. | - | -

## Sqlite conversion: LockConversion
Two transactions read the same rows of shop.orders, then both tried to change them; session 61's transaction was rolled back.
Both took a shared lock to read, then asked for an exclusive lock to write. A shared lock lets the other reader in, so neither can upgrade: whoever asks second deadlocks. Typical of 'read, check, then update' code.
- Take the write lock when you read | Read the rows you are about to change with an update lock, or do the check and the change in one UPDATE … WHERE statement. | SELECT … FROM … WHERE … FOR UPDATE; | -
- Retry the victim | A deadlock victim can be retried safely; the application should catch SQLITE_BUSY and run the transaction again. | - | -"#
        );
    }

    #[test]
    fn the_same_event_keeps_its_id() {
        let a =
            Deadlock::new("x", Some("t".into()), vec![process("p1", "61", true), process("p2", "64", false)], vec![]);
        let b =
            Deadlock::new("y", Some("t".into()), vec![process("p2", "64", false), process("p1", "61", true)], vec![]);
        assert_eq!(a.id, b.id);
    }
}
