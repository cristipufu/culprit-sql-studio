//! What blocked sessions wait for, and recent deadlocks: from the built-in system_health session, or on Azure SQL
//! Database from the deadlock telemetry it keeps per database. Both read DMVs and event targets only. The parser also
//! reads saved .xdl files; XML with a DTD is refused, so entities never resolve.

use culprit_core::deadlock::classify;
use culprit_core::{
    Deadlock, DeadlockProcess, DeadlockReport, DeadlockResource, Engine, Error, LockHolder, LockWait, Result, t,
};
use roxmltree::{Document, Node};

use crate::read::rows as query;
use crate::read::{NativeRead, ReadError, integer, invalid_data, text};
pub(crate) mod native;

/// The lock each waiting task asked for. Objects in other databases are named only for whole-object locks; row and
/// page locks resolve through this database's partitions.
const LOCK_WAITS_SQL: &str = r#"
SELECT
  CAST(wt.session_id AS varchar(10)) AS session_id,
  l.request_mode AS mode,
  l.resource_type AS resource,
  CASE
    WHEN l.resource_type = 'OBJECT'
      THEN OBJECT_SCHEMA_NAME(l.resource_associated_entity_id, l.resource_database_id) + '.'
         + OBJECT_NAME(l.resource_associated_entity_id, l.resource_database_id)
    WHEN p.object_id IS NOT NULL THEN OBJECT_SCHEMA_NAME(p.object_id) + '.' + OBJECT_NAME(p.object_id)
  END AS object_name,
  i.name AS index_name
FROM sys.dm_os_waiting_tasks wt
JOIN sys.dm_tran_locks l ON l.lock_owner_address = wt.resource_address
LEFT JOIN sys.partitions p
  ON l.resource_type IN ('KEY', 'PAGE', 'RID', 'HOBT') AND l.resource_database_id = DB_ID()
 AND p.hobt_id = l.resource_associated_entity_id
LEFT JOIN sys.indexes i ON i.object_id = p.object_id AND i.index_id = p.index_id
WHERE wt.session_id IS NOT NULL AND wt.session_id <> @@SPID
"#;

pub(crate) async fn lock_waits(client: &mut impl NativeRead) -> Result<Vec<LockWait>> {
    let rows = query(client, LOCK_WAITS_SQL).await?;
    let mut out: Vec<LockWait> = Vec::new();
    for r in rows {
        let session = r.string("session_id");
        // A parallel request has one waiting task per thread; one lock per session is enough to explain it.
        if out.iter().any(|l| l.session == session) {
            continue;
        }
        out.push(LockWait {
            session,
            mode: r.str("mode"),
            resource: r.str("resource"),
            object: r.str("object_name"),
            index: r.str("index_name"),
        });
    }
    Ok(out)
}

const READ_LIMIT: usize = 50;

/// Where Azure SQL Database's deadlocks come from, as a person reads it.
pub(crate) fn telemetry() -> String {
    t!("provider.mssql.deadlocks.telemetry")
}

/// Azure SQL Database's deadlock telemetry couldn't be read (`error`): where else its deadlocks are.
pub(crate) fn telemetry_unreadable(error: &dyn std::fmt::Display) -> String {
    t!("provider.mssql.deadlocks.telemetry_unreadable", error = error.to_string())
}

/// Where system_health's deadlocks were read from (its event files, its in-memory ring buffer, either or neither), and
/// what the reader should know about that.
pub(crate) fn origin(files: bool, ring: bool) -> (String, Option<String>) {
    let read = match (files, ring) {
        (true, true) => "both",
        (true, false) => "files",
        (false, true) => "ring",
        (false, false) => "none",
    };
    let note = match (files, ring) {
        (false, false) => Some(t!("provider.mssql.deadlocks.needs_permission")),
        (false, true) => Some(t!("provider.mssql.deadlocks.ring_only")),
        (true, _) => None,
    };
    (t!("provider.mssql.deadlocks.origin", read = read), note)
}

/// Deadlocks in the last `since_hours`. On SQL Server from system_health: its event files first (they keep days), the
/// in-memory ring buffer when files can't be read. On Azure SQL Database from its deadlock telemetry. Permission
/// problems come back as a note, not an error.
pub(crate) async fn recent<R: NativeRead>(io: &mut R, since_hours: f64) -> Result<DeadlockReport> {
    let hours = since_hours.clamp(1.0, 24.0 * 30.0).round() as u32;
    let mut report = DeadlockReport { since_hours: hours as f64, ..Default::default() };
    let server = io.query(native::SERVER, vec![], 1).await.map_err(ReadError::into_error)?;
    let server = server.first().ok_or_else(invalid_data)?;
    let database = text(server, "db")?;
    let until = culprit_core::telemetry::canonical_time(&text(server, "observed_at")?)?;
    let now = chrono::DateTime::parse_from_rfc3339(&until).map_err(|_| invalid_data())?;
    let cutoff = now - chrono::Duration::hours(i64::from(hours));
    let from = cutoff.to_rfc3339();
    if integer(server, "edition")? == 5 {
        report.source = telemetry();
        match native::read(io, native::Origin::AzureTelemetry, &database, &from, &until, READ_LIMIT, true).await {
            Ok(events) => report.deadlocks.extend(events.into_iter().flat_map(|event| event.graphs)),
            Err(error) => {
                report.source = "Azure SQL Database".into();
                report.note = Some(telemetry_unreadable(&error));
            }
        }
        report.deadlocks.sort_by(|a, b| b.at.cmp(&a.at));
        report.recent_only = true;
        return Ok(report);
    }
    let (mut files, mut ring) = (false, false);
    if let Ok(events) = native::read(io, native::Origin::SystemHealth, &database, &from, &until, READ_LIMIT, true).await
    {
        files = true;
        report.deadlocks.extend(events.into_iter().flat_map(|event| event.graphs));
    }
    if let Ok(rows) = io.query(native::RING, vec![], 1).await {
        ring = true;
        for row in rows {
            let Some(xml) = row.str("target_xml") else { continue };
            for graph in parse_deadlock_xml(&xml, "system_health").unwrap_or_default() {
                let recent = graph
                    .at
                    .as_deref()
                    .and_then(|time| chrono::DateTime::parse_from_rfc3339(time).ok())
                    .is_none_or(|time| time >= cutoff);
                if recent && !report.deadlocks.iter().any(|known| known.id == graph.id) {
                    report.deadlocks.push(graph);
                }
            }
        }
    }
    report.deadlocks.retain(|graph| involves(graph, &database));
    report.deadlocks.sort_by(|a, b| b.at.cmp(&a.at));
    report.deadlocks.truncate(READ_LIMIT);
    (report.source, report.note) = origin(files, ring);
    report.recent_only = !files && ring;
    Ok(report)
}

/// Whether a deadlock from the instance-wide system_health session involves `database`: one of its processes ran in
/// it. A graph that names no database for any process can't be placed and is kept.
fn involves(d: &Deadlock, database: &str) -> bool {
    let mut named = d.processes.iter().filter_map(|p| p.database.as_deref()).peekable();
    named.peek().is_none() || named.any(|db| db.eq_ignore_ascii_case(database))
}

/// `2026-10-11T12:05:01.1230000` (UTC, no zone) → `2026-10-11T12:05:01Z`.
fn normalize_time(text: &str) -> String {
    let t = text.trim().trim_end_matches('Z');
    let t = t.split('.').next().unwrap_or(t);
    format!("{t}Z")
}

/// The deadlocks in one telemetry event, unless it names another database: the telemetry may be the logical server's,
/// and deadlock graphs there name databases by their physical name, so the event's `database_name` is what tells.
fn telemetry_deadlocks(xml: &str, database: &str, at: Option<String>) -> Result<Vec<Deadlock>> {
    culprit_core::xml::read(xml, |doc| {
        let doc = doc.map_err(|_| invalid_data())?;
        telemetry_event(&doc, xml, database, at)
    })
}

fn telemetry_event(doc: &Document, xml: &str, database: &str, at: Option<String>) -> Result<Vec<Deadlock>> {
    let event_database = doc
        .descendants()
        .find(|node| node.has_tag_name("data") && node.attribute("name") == Some("database_name"))
        .and_then(|data| child(data, "value"))
        .and_then(text_of)
        .ok_or_else(invalid_data)?;
    if !event_database.eq_ignore_ascii_case(database) {
        return Ok(Vec::new());
    }
    let mut found = parse_with_time(xml, &telemetry(), at)?;
    if found.is_empty() {
        return Err(invalid_data());
    }
    // A fix that names the database (ALTER DATABASE) needs the name people use.
    for p in found.iter_mut().flat_map(|d| d.processes.iter_mut()) {
        p.database = Some(database.to_string());
    }
    Ok(found)
}

fn parse_with_time(xml: &str, source: &str, at: Option<String>) -> Result<Vec<Deadlock>> {
    let mut found = parse_deadlock_xml(xml, source)?;
    for d in &mut found {
        if d.at.is_none() {
            d.at = at.clone();
        }
    }
    Ok(found)
}

/// Every deadlock in `xml`: a saved .xdl (`<deadlock-list>` or `<deadlock>`), a system_health event, or a ring buffer.
pub fn parse_deadlock_xml(xml: &str, source: &str) -> Result<Vec<Deadlock>> {
    culprit_core::xml::read(xml.trim_start_matches('\u{feff}'), |doc| {
        let doc = doc.map_err(|e| Error::invalid(t!("provider.mssql.deadlocks.unreadable", error = e.to_string())))?;
        deadlock_graphs(&doc, source)
    })
}

fn deadlock_graphs(doc: &Document, source: &str) -> Result<Vec<Deadlock>> {
    let mut out = Vec::new();
    for node in doc.descendants().filter(|n| n.has_tag_name("deadlock")) {
        // Ring buffers hold every kind of event; only deadlock reports carry a <deadlock>.
        let at = node
            .ancestors()
            .find(|a| a.has_tag_name("event"))
            .and_then(|e| e.attribute("timestamp"))
            .map(normalize_time);
        if let Some(d) = deadlock(node, source, at) {
            out.push(d);
        }
    }
    if out.is_empty() && !doc.descendants().any(|n| n.has_tag_name("RingBufferTarget") || n.has_tag_name("event")) {
        return Err(Error::invalid(t!("provider.mssql.deadlocks.no_graph"))
            .with_hint(t!("provider.mssql.deadlocks.no_graph.hint")));
    }
    Ok(out)
}

fn child<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    node.children().find(|c| c.has_tag_name(name))
}

fn text_of(node: Node) -> Option<String> {
    let text: String = node.descendants().filter(|n| n.is_text()).filter_map(|n| n.text()).collect();
    let text = text.trim();
    (!text.is_empty() && !text.eq_ignore_ascii_case("unknown")).then(|| clip(text))
}

fn clip(text: &str) -> String {
    const MAX: usize = 4000;
    if text.chars().count() <= MAX {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(MAX).collect::<String>())
    }
}

/// `db.schema.table` → `schema.table`.
fn object_of(name: &str) -> String {
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() >= 2 { parts[parts.len() - 2..].join(".") } else { name.to_string() }
}

fn attr(node: Node, name: &str) -> Option<String> {
    node.attribute(name).map(str::trim).filter(|v| !v.is_empty()).map(str::to_string)
}

fn deadlock(node: Node, source: &str, at: Option<String>) -> Option<Deadlock> {
    let mut victims: Vec<String> = child(node, "victim-list")
        .map(|v| v.children().filter(|c| c.has_tag_name("victimProcess")).filter_map(|c| attr(c, "id")).collect())
        .unwrap_or_default();
    if let Some(v) = attr(node, "victim") {
        victims.push(v);
    }
    let processes: Vec<DeadlockProcess> = child(node, "process-list")?
        .children()
        .filter(|c| c.has_tag_name("process"))
        .map(|p| {
            let id = attr(p, "id").unwrap_or_default();
            let frame = child(p, "executionStack").and_then(|s| {
                s.children().filter(|f| f.has_tag_name("frame")).find_map(|f| text_of(f).map(|t| (f, t)))
            });
            let procedure = frame
                .as_ref()
                .and_then(|(f, _)| attr(*f, "procname"))
                .filter(|n| !matches!(n.to_ascii_lowercase().as_str(), "adhoc" | "unknown"))
                .map(|n| object_of(&n));
            DeadlockProcess {
                victim: victims.contains(&id),
                session: attr(p, "spid"),
                statement: frame.map(|(_, t)| t).or_else(|| child(p, "inputbuf").and_then(text_of)),
                procedure,
                login: attr(p, "loginname"),
                host: attr(p, "hostname"),
                application: attr(p, "clientapp"),
                database: attr(p, "currentdbname"),
                isolation: attr(p, "isolationlevel").map(|i| i.split(" (").next().unwrap_or(&i).to_string()),
                wait_mode: attr(p, "lockMode"),
                log_used: attr(p, "logused").and_then(|v| v.parse().ok()),
                id,
            }
        })
        .collect();
    if processes.is_empty() {
        return None;
    }
    let resources: Vec<DeadlockResource> = child(node, "resource-list")
        .map(|list| {
            list.children()
                .filter(|c| c.is_element())
                .enumerate()
                .map(|(i, r)| {
                    let holders = |list: &str, item: &str| -> Vec<LockHolder> {
                        child(r, list)
                            .map(|l| {
                                l.children()
                                    .filter(|c| c.has_tag_name(item))
                                    .filter_map(|c| Some(LockHolder { process: attr(c, "id")?, mode: attr(c, "mode") }))
                                    .collect()
                            })
                            .unwrap_or_default()
                    };
                    DeadlockResource {
                        id: attr(r, "id").unwrap_or_else(|| format!("{}{i}", r.tag_name().name())),
                        kind: r.tag_name().name().to_string(),
                        object: attr(r, "objectname").map(|o| object_of(&o)),
                        index: attr(r, "indexname"),
                        holders: holders("owner-list", "owner"),
                        waiters: holders("waiter-list", "waiter"),
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let mut d = Deadlock::new(source, at, processes, resources);
    classify(Engine::Mssql, &mut d);
    Some(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::DeadlockPattern;

    pub(crate) const KEY_LOOKUP_XDL: &str = r#"<deadlock-list><deadlock victim="process61">
 <process-list>
  <process id="process61" spid="61" lockMode="S" isolationlevel="read committed (2)" currentdbname="shop" loginname="app" hostname="web-1" clientapp="orders-api" logused="0">
   <executionStack><frame procname="adhoc" line="1">SELECT id, name, segment FROM shop.customers WHERE email = @P1</frame></executionStack>
   <inputbuf>(@P1 nvarchar(200))SELECT id, name, segment FROM shop.customers WHERE email = @P1</inputbuf>
  </process>
  <process id="process64" spid="64" lockMode="X" isolationlevel="read committed (2)" currentdbname="shop" loginname="app" hostname="worker-2" clientapp="billing" logused="1240">
   <executionStack><frame procname="adhoc" line="1">UPDATE shop.customers SET email = @P1 WHERE id = @P2</frame></executionStack>
   <inputbuf />
  </process>
 </process-list>
 <resource-list>
  <keylock hobtid="1" dbid="5" objectname="shop.shop.customers" indexname="ix_customers_email" id="lock1" mode="X">
   <owner-list><owner id="process64" mode="X"/></owner-list>
   <waiter-list><waiter id="process61" mode="S" requestType="wait"/></waiter-list>
  </keylock>
  <keylock hobtid="2" dbid="5" objectname="shop.shop.customers" indexname="PK_customers" id="lock2" mode="S">
   <owner-list><owner id="process61" mode="S"/></owner-list>
   <waiter-list><waiter id="process64" mode="X" requestType="wait"/></waiter-list>
  </keylock>
 </resource-list>
</deadlock></deadlock-list>"#;

    #[test]
    fn a_saved_xdl_is_parsed_and_explained() {
        let found = parse_deadlock_xml(KEY_LOOKUP_XDL, "pasted").unwrap();
        assert_eq!(found.len(), 1);
        let d = &found[0];
        assert_eq!(d.pattern, DeadlockPattern::KeyLookup);
        assert_eq!(d.objects, vec!["shop.customers".to_string()]);
        let victim = d.victim().unwrap();
        assert_eq!((victim.session.as_deref(), victim.application.as_deref()), (Some("61"), Some("orders-api")));
        assert_eq!(victim.isolation.as_deref(), Some("read committed"));
        assert!(victim.statement.as_deref().unwrap().starts_with("SELECT id, name"));
        assert_eq!(d.resources[0].holders[0].mode.as_deref(), Some("X"));
    }

    // system_health records every database's deadlocks; a scan of one database keeps only its own.
    #[test]
    fn instance_wide_deadlocks_are_kept_only_for_this_database() {
        let found = parse_deadlock_xml(KEY_LOOKUP_XDL, "system_health").unwrap();
        assert!(involves(&found[0], "SHOP"), "both processes ran in shop");
        assert!(!involves(&found[0], "billing"), "another database's deadlock");
        let unnamed = parse_deadlock_xml(&KEY_LOOKUP_XDL.replace(r#" currentdbname="shop""#, ""), "pasted").unwrap();
        assert!(involves(&unnamed[0], "billing"), "a graph without databases can't be placed, so it stays");
    }

    #[test]
    fn system_health_events_carry_their_time_and_dtds_are_refused() {
        let event = format!(
            r#"<event name="xml_deadlock_report" timestamp="2026-10-11T12:05:01.123Z"><data name="xml_report"><value>{}</value></data></event>"#,
            KEY_LOOKUP_XDL.replace("<deadlock-list>", "").replace("</deadlock-list>", "")
        );
        let found = parse_deadlock_xml(&event, "system_health").unwrap();
        assert_eq!(found[0].at.as_deref(), Some("2026-10-11T12:05:01Z"));
        let evil =
            r#"<?xml version="1.0"?><!DOCTYPE x [<!ENTITY e SYSTEM "file:///etc/passwd">]><deadlock>&e;</deadlock>"#;
        assert!(parse_deadlock_xml(evil, "pasted").is_err());
        assert!(parse_deadlock_xml("<nothing/>", "pasted").is_err());
    }

    /// One event from Azure SQL Database's deadlock telemetry, as `event_data` holds it.
    fn telemetry_event(database: &str) -> String {
        let graph = KEY_LOOKUP_XDL
            .replace("<deadlock-list>", "")
            .replace("</deadlock-list>", "")
            // The graph names the database by its physical name there.
            .replace(r#"currentdbname="shop""#, r#"currentdbname="5f0c1e9a-7d2b-4c66-9e1f-3a8b2d4c6e10""#);
        format!(
            r#"<event name="database_xml_deadlock_report" package="sqlserver" timestamp="2026-10-11T12:05:01.123Z"><data name="xml_report"><type name="xml" package="package0"/><value>{graph}</value></data><data name="database_name"><type name="unicode_string" package="package0"/><value>{database}</value></data></event>"#
        )
    }

    #[test]
    fn azure_sql_telemetry_events_are_read_for_this_database_only() {
        let found = telemetry_deadlocks(&telemetry_event("shop"), "Shop", Some("2026-10-11T12:05:01Z".into())).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].pattern, DeadlockPattern::KeyLookup);
        assert_eq!(found[0].at.as_deref(), Some("2026-10-11T12:05:01Z"));
        assert_eq!(found[0].source, "Azure SQL deadlock telemetry");
        assert_eq!(found[0].objects, vec!["shop.customers".to_string()]);
        assert!(found[0].processes.iter().all(|p| p.database.as_deref() == Some("Shop")));
        // Another database on the same logical server.
        assert!(telemetry_deadlocks(&telemetry_event("billing"), "shop", None).unwrap().is_empty());
        assert!(telemetry_deadlocks("not xml", "shop", None).is_err());
    }
}

#[cfg(test)]
mod native_tests;
