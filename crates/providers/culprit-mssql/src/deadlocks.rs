//! What blocked sessions wait for, and recent deadlocks: from configured system_health event files, or on Azure SQL
//! Database from an explicitly configured database-scoped event session. Server target-data views are never read. The parser also
//! reads saved .xdl files; XML with a DTD is refused, so entities never resolve.

use culprit_core::deadlock::classify;
use culprit_core::{
    Deadlock, DeadlockProcess, DeadlockReport, DeadlockResource, Engine, Error, LockHolder, LockWait, MeasurementScope,
    Result, t, telemetry::canonical_time,
};
use roxmltree::{Document, Node};

use crate::read::rows as query;
use crate::read::{NativeRead, ReadError, integer, invalid_data, text};
mod database;
pub(crate) mod native;
pub(crate) mod system_health;
#[cfg(test)]
pub(crate) use database::tests as database_test_support;

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

/// The explicitly configured, database-scoped source, as a person reads it.
pub(crate) fn database_session() -> String {
    t!("provider.mssql.deadlocks.database_session")
}

pub(crate) fn database_session_unreadable(error: &dyn std::fmt::Display) -> String {
    t!("provider.mssql.deadlocks.database_session_unreadable", error = error.to_string())
}

/// The supported server-scoped configured-file source, named independently of its availability.
pub(crate) fn system_health_files() -> String {
    t!("provider.mssql.deadlocks.origin")
}

/// Retained deadlocks in the last `since_hours`. SQL Server needs an absolute configured system_health file path;
/// Azure SQL Database uses its explicit database event session. Unavailable configuration remains a note and unknown
/// scope, without a runtime-target or server ring fallback. Neither source proves a complete retention horizon.
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
        report.source = database_session();
        match native::read(io, native::Origin::DatabaseSession, &database, &from, &until, READ_LIMIT, true).await {
            Ok(events) => {
                report.scope = Some(MeasurementScope::Database);
                report.deadlocks.extend(events.into_iter().flat_map(|event| event.graphs));
                report.note = Some(t!("provider.mssql.deadlocks.database_session_recent"));
            }
            Err(error) => {
                report.note = Some(database_session_unreadable(&error));
            }
        }
        report.deadlocks.sort_by(|a, b| b.at.cmp(&a.at));
        report.recent_only = true;
        return Ok(report);
    }
    report.source = system_health_files();
    match native::read(io, native::Origin::SystemHealth, &database, &from, &until, READ_LIMIT, true).await {
        Ok(events) => {
            report.scope = Some(MeasurementScope::Server);
            report.deadlocks.extend(events.into_iter().flat_map(|event| event.graphs));
            report.deadlocks.sort_by(|a, b| b.at.cmp(&a.at));
            report.deadlocks.truncate(READ_LIMIT);
            report.note = Some(t!("provider.mssql.deadlocks.configured_file_recent"));
            report.recent_only = true;
        }
        Err(_) => {
            report.note = Some(t!("provider.mssql.deadlocks.configured_file_unavailable"));
        }
    }
    Ok(report)
}

/// Every deadlock in `xml`: a saved .xdl (`<deadlock-list>` or `<deadlock>`), a system_health event, or a ring buffer.
pub fn parse_deadlock_xml(xml: &str, source: &str) -> Result<Vec<Deadlock>> {
    parse_with_time(xml, source, None)
}

fn parse_with_time(xml: &str, source: &str, native_at: Option<&str>) -> Result<Vec<Deadlock>> {
    // The XEL reader supplies its documented UTC clock. Imported XML needs an explicit offset of its own.
    let native_at = native_at.map(canonical_time).transpose()?;
    culprit_core::xml::read(xml.trim_start_matches('\u{feff}'), |doc| {
        let doc = doc.map_err(|e| Error::invalid(t!("provider.mssql.deadlocks.unreadable", error = e.to_string())))?;
        deadlock_graphs(&doc, source, native_at.as_deref())
    })
}

fn deadlock_graphs(doc: &Document, source: &str, native_at: Option<&str>) -> Result<Vec<Deadlock>> {
    require_declared_graphs(doc)?;
    let mut out = Vec::new();
    for node in doc.descendants().filter(|n| n.has_tag_name("deadlock")) {
        // Ring buffers hold every kind of event; only deadlock reports carry a <deadlock>.
        let at = node
            .ancestors()
            .find(|a| a.has_tag_name("event"))
            .and_then(|e| e.attribute("timestamp"))
            .map(canonical_time)
            .transpose()?;
        if at.as_deref().zip(native_at).is_some_and(|(event, native)| event != native) {
            return Err(invalid_data());
        }
        let at = at.or_else(|| native_at.map(str::to_owned));
        out.push(deadlock(node, source, at).ok_or_else(invalid_data)?);
    }
    if out.is_empty() && !doc.descendants().any(|n| n.has_tag_name("RingBufferTarget") || n.has_tag_name("event")) {
        return Err(Error::invalid(t!("provider.mssql.deadlocks.no_graph"))
            .with_hint(t!("provider.mssql.deadlocks.no_graph.hint")));
    }
    Ok(out)
}

fn require_declared_graphs(doc: &Document) -> Result<()> {
    for event in doc.descendants().filter(|node| {
        node.has_tag_name("event")
            && matches!(node.attribute("name"), Some("xml_deadlock_report" | "database_xml_deadlock_report"))
    }) {
        let has_graph = event.descendants().any(|node| {
            node.has_tag_name("deadlock")
                && node.ancestors().find(|ancestor| ancestor.has_tag_name("event")) == Some(event)
        });
        if !has_graph {
            return Err(invalid_data());
        }
    }
    Ok(())
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
pub(crate) mod tests {
    use super::*;
    use crate::{
        read::ReadResult,
        resources::test_support::FakeRead,
        value::{Rec, Val},
    };
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

    pub(crate) fn event(at: &str) -> String {
        format!(
            r#"<event name="xml_deadlock_report" timestamp="{at}"><data name="xml_report"><value>{}</value></data></event>"#,
            KEY_LOOKUP_XDL.replace("<deadlock-list>", "").replace("</deadlock-list>", "")
        )
    }

    pub(crate) fn file_row(at: &str, xml: &str) -> Rec {
        Rec::of(&[
            ("file_name", Val::Text("/var/opt/mssql/log/system_health_0_134388425128000000.xel".into())),
            ("file_offset", Val::Int(512)),
            ("at", Val::Text(at.into())),
            ("event_xml", Val::Text(xml.into())),
        ])
    }

    fn scan_configuration() -> Rec {
        crate::resources::test_support::replace(
            &system_health::tests::configuration(),
            &[("observed_at", Val::Text("2026-10-11T12:10:00Z".into()))],
        )
    }

    fn recent_reader(file_events: ReadResult<Vec<Rec>>, final_configuration: Rec) -> FakeRead {
        FakeRead::new(vec![
            Ok(vec![Rec::of(&[
                ("edition", Val::Int(3)),
                ("db", Val::Text("shop".into())),
                ("observed_at", Val::Text("2026-10-11T12:10:00Z".into())),
            ])]),
            Ok(vec![scan_configuration()]),
            file_events,
            Ok(vec![final_configuration]),
        ])
    }

    #[test]
    fn a_saved_xdl_is_parsed_and_explained() {
        let found = parse_deadlock_xml(KEY_LOOKUP_XDL, "pasted").unwrap();
        assert_eq!(found.len(), 1);
        let d = &found[0];
        assert_eq!(d.pattern, DeadlockPattern::KeyLookup);
        assert!(d.at.is_none(), "an imported graph without an event clock stays undated");
        assert_eq!(d.objects, vec!["shop.customers".to_string()]);
        let victim = d.victim().unwrap();
        assert_eq!((victim.session.as_deref(), victim.application.as_deref()), (Some("61"), Some("orders-api")));
        assert_eq!(victim.isolation.as_deref(), Some("read committed"));
        assert!(victim.statement.as_deref().unwrap().starts_with("SELECT id, name"));
        assert_eq!(d.resources[0].holders[0].mode.as_deref(), Some("X"));
    }

    #[test]
    fn system_health_events_carry_their_time_and_dtds_are_refused() {
        let found = parse_deadlock_xml(&event("2026-10-11T12:05:01.123Z"), "system_health").unwrap();
        assert_eq!(found[0].at.as_deref(), Some("2026-10-11T12:05:01.123000000Z"));
        let evil =
            r#"<?xml version="1.0"?><!DOCTYPE x [<!ENTITY e SYSTEM "file:///etc/passwd">]><deadlock>&e;</deadlock>"#;
        assert!(parse_deadlock_xml(evil, "pasted").is_err());
        assert!(parse_deadlock_xml("<nothing/>", "pasted").is_err());
    }

    #[test]
    fn full_event_precision_distinguishes_repeated_participants_and_canonicalizes_replays() {
        let first = parse_deadlock_xml(&event("2026-10-11T12:05:01.1234567Z"), "system_health").unwrap();
        let second = parse_deadlock_xml(&event("2026-10-11T12:05:01.1234568Z"), "system_health").unwrap();
        let replay = parse_deadlock_xml(&event("2026-10-11T14:05:01.123456700+02:00"), "system_health").unwrap();
        assert_eq!(first[0].at.as_deref(), Some("2026-10-11T12:05:01.123456700Z"));
        assert_eq!(second[0].at.as_deref(), Some("2026-10-11T12:05:01.123456800Z"));
        assert_ne!(first[0].id, second[0].id, "two native events with the same SPIDs remain distinct");
        assert_eq!(first, replay, "equivalent explicit offsets and trailing zeros describe the same event");
        assert_eq!(first[0].processes, second[0].processes);
        assert_eq!(first[0].headline, second[0].headline);
        assert_eq!(first[0].explanation, second[0].explanation);
        assert_eq!(first[0].fixes, second[0].fixes);
    }

    #[test]
    fn invalid_event_clocks_are_rejected_without_inventing_an_offset_or_losing_precision() {
        let too_long = format!("2026-10-11T12:05:01.{}Z", "0".repeat(50));
        for at in [
            "",
            "2026-10-11T12:05:01.1234567",
            "2026-10-11T12:05:01.1234567Zgarbage",
            "2026-02-30T12:05:01.1234567Z",
            "2026-10-11T12:05:01.1234567001Z",
            too_long.as_str(),
        ] {
            let xml = event(at);
            assert!(parse_deadlock_xml(&xml, "pasted").is_err(), "invalid event clock: {at}");
            assert!(
                parse_with_time(&xml, "system_health", Some("2026-10-11T12:05:01.1234567Z")).is_err(),
                "a native row clock cannot repair an invalid event clock: {at}"
            );
        }
    }

    #[test]
    fn a_claimed_report_cannot_be_empty_or_borrow_a_nested_events_graph() {
        for name in ["xml_deadlock_report", "database_xml_deadlock_report"] {
            for at in ["bad-clock", "2026-10-11T12:05:01.1234567Z"] {
                let missing =
                    format!(r#"<event name="{name}" timestamp="{at}"><data name="xml_report"><value/></data></event>"#);
                assert!(parse_deadlock_xml(&missing, "pasted").is_err());
                assert!(parse_with_time(&missing, "system_health", Some("2026-10-11T12:05:01.1234567Z")).is_err());
                let nested = format!(
                    r#"<event name="{name}" timestamp="{at}">{}</event>"#,
                    event("2026-10-11T12:05:01.1234567Z")
                );
                assert!(parse_deadlock_xml(&nested, "pasted").is_err());
            }
        }
        assert!(parse_deadlock_xml(r#"<event name="unrelated"/>"#, "system_health").unwrap().is_empty());
    }

    #[test]
    fn a_native_row_clock_qualifies_identity_before_construction_and_must_agree_with_event_time() {
        let at = "2026-10-11T12:05:01.1234567Z";
        let supplied = parse_with_time(KEY_LOOKUP_XDL, "system_health", Some(at)).unwrap();
        let wrapped = parse_deadlock_xml(&event(at), "system_health").unwrap();
        assert_eq!(supplied, wrapped, "a timestamp-less native graph and its ring replay share one identity");
        let later = parse_with_time(KEY_LOOKUP_XDL, "system_health", Some("2026-10-11T12:05:01.1234568Z")).unwrap();
        assert_ne!(supplied[0].id, later[0].id, "the native fallback clock is not assigned after ID construction");
        assert!(parse_with_time(&event(at), "system_health", Some("2026-10-11T12:05:01.1234568Z")).is_err());
        assert!(parse_with_time(&event(at), "system_health", Some("2026-10-11T12:05:01.1234567")).is_err());
    }

    #[test]
    fn a_claimed_deadlock_requires_a_graph_while_unrelated_events_can_be_empty() {
        for xml in ["<RingBufferTarget/>", r#"<RingBufferTarget><event name="wait_info"/></RingBufferTarget>"#] {
            assert!(parse_deadlock_xml(xml, "system_health").unwrap().is_empty());
        }
        let malformed = r#"<event name="xml_deadlock_report" timestamp="2026-10-11T12:05:01.1234568Z"><data name="xml_report"><value><deadlock/></value></data></event>"#;
        assert!(parse_deadlock_xml(malformed, "system_health").is_err());
        let partial =
            format!("<RingBufferTarget>{}{malformed}</RingBufferTarget>", event("2026-10-11T12:05:01.1234567Z"));
        assert!(
            parse_deadlock_xml(&partial, "system_health").is_err(),
            "a malformed claimed graph cannot yield a successful partial set"
        );
    }

    #[tokio::test]
    async fn configured_file_history_keeps_fractional_events_and_unresolved_membership_without_database_grading() {
        let a = "2026-10-11T12:05:01.1234567Z";
        let b = "2026-10-11T12:05:01.1234568Z";
        let unnamed = event(b).replace(r#" currentdbname="shop""#, "");
        let mut io = recent_reader(Ok(vec![file_row(a, &event(a)), file_row(b, &unnamed)]), scan_configuration());
        let report = recent(&mut io, 1.0).await.unwrap();
        assert_eq!(report.scope, Some(MeasurementScope::Server));
        assert_eq!(report.deadlocks.len(), 2);
        assert_ne!(report.deadlocks[0].id, report.deadlocks[1].id);
        assert!(report.deadlocks.iter().any(|graph| graph.processes.iter().all(|process| process.database.is_none())));
        assert!(report.recent_only && report.note.is_some());
        assert_eq!(io.calls.len(), 4);
        assert!(
            io.calls
                .iter()
                .all(|call| !call.sql.contains("dm_xe_session_targets") && !call.sql.contains("target_data"))
        );
        assert_eq!(io.closed, 0, "scan caller retains close ownership");
    }

    #[tokio::test]
    async fn database_scoped_session_owns_report_scope_even_when_graph_database_names_are_missing() {
        use super::database::tests::{configuration, event, target};
        let server = Rec::of(&[
            ("edition", Val::Int(5)),
            ("db", Val::Text("shop".into())),
            ("observed_at", Val::Text("2020-01-01T07:00:00Z".into())),
        ]);
        let mut io =
            FakeRead::new(vec![Ok(vec![server.clone()]), Ok(vec![configuration()]), Ok(vec![target(&[event(1)])])]);
        let report = recent(&mut io, 1.0).await.unwrap();
        assert_eq!(report.scope, Some(MeasurementScope::Database));
        assert_eq!(report.deadlocks.len(), 1);
        assert!(report.deadlocks[0].processes.iter().all(|process| process.database.is_none()));
        assert_eq!(io.calls.len(), 3);
        assert_eq!(io.closed, 0, "the caller still owns close");

        let mut io = FakeRead::new(vec![Ok(vec![server]), Err(invalid_data().into())]);
        let unavailable = recent(&mut io, 1.0).await.unwrap();
        assert_eq!(unavailable.scope, None);
        assert!(unavailable.deadlocks.is_empty());
        assert!(unavailable.note.is_some());
        assert_eq!(io.calls.len(), 2, "an unavailable database source has no server-wide fallback");
    }

    #[tokio::test]
    async fn unavailable_relative_or_changed_file_configuration_does_not_claim_empty_server_history() {
        use crate::resources::test_support::replace;
        for configuration in
            [vec![], vec![replace(&scan_configuration(), &[("file_path", Val::Text("system_health.xel".into()))])]]
        {
            let mut io = FakeRead::new(vec![
                Ok(vec![Rec::of(&[
                    ("edition", Val::Int(3)),
                    ("db", Val::Text("shop".into())),
                    ("observed_at", Val::Text("2026-10-11T12:10:00Z".into())),
                ])]),
                Ok(configuration),
            ]);
            let report = recent(&mut io, 1.0).await.unwrap();
            assert_eq!(report.scope, None);
            assert!(report.deadlocks.is_empty() && report.note.is_some());
            assert_eq!(io.calls.len(), 2, "no ring/runtime path fallback");
        }
        let mut io = recent_reader(Ok(vec![]), replace(&scan_configuration(), &[("event_session_id", Val::Int(2))]));
        let changed = recent(&mut io, 1.0).await.unwrap();
        assert_eq!(changed.scope, None);
        assert!(changed.deadlocks.is_empty() && changed.note.is_some());
        assert_eq!(io.calls.len(), 4);
    }

    #[tokio::test]
    async fn successful_empty_file_history_has_observed_server_scope_without_proving_retention() {
        let mut io = recent_reader(Ok(vec![]), scan_configuration());
        let report = recent(&mut io, 1.0).await.unwrap();
        assert_eq!(report.scope, Some(MeasurementScope::Server));
        assert!(report.deadlocks.is_empty() && report.recent_only && report.note.is_some());
        assert_eq!(io.calls.len(), 4);
    }
}

#[cfg(test)]
mod native_tests;
