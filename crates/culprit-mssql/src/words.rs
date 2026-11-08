//! What the SQL Server provider says to a person, word for word: connection and permission errors with their hints,
//! the notes and locked-capability reasons a scan records, signal labels, and the names of what it measures. These
//! pin the English as the catalog speaks it.

use std::collections::HashMap;

use culprit_core::{AuthKind, ConnectionSpec, Coverage, Error, EventAction, Signal};

use crate::probe::{Probe, QueryStoreState};
use crate::value::{Rec, Val};

fn said(error: &Error) -> String {
    match &error.hint {
        Some(hint) => format!("{} | {hint}", error.message),
        None => error.message.clone(),
    }
}

fn signals(signals: &[Signal]) -> String {
    signals
        .iter()
        .map(|s| format!("{}: {}", s.label, s.healthy.as_deref().unwrap_or("-")))
        .collect::<Vec<_>>()
        .join("\n")
}

fn spec() -> ConnectionSpec {
    ConnectionSpec {
        host: "db.example.com".into(),
        user: Some("reader".into()),
        password: Some("secret".into()),
        ..Default::default()
    }
}

#[test]
fn connection_errors() {
    let refused = |s: ConnectionSpec| said(&crate::conn::build_config(&s).unwrap_err());
    assert_eq!(refused(ConnectionSpec { host: " ".into(), ..spec() }), "A SQL Server host is required");
    assert_eq!(
        refused(ConnectionSpec { user: None, ..spec() }),
        "SQL Server password authentication needs a user name"
    );
    let entra = ConnectionSpec { auth: AuthKind::Entra, ..spec() };
    assert_eq!(refused(entra.clone()), "Microsoft Entra ID tokens are only sent to Azure SQL hosts");
    let azure = ConnectionSpec { host: "shop.database.windows.net".into(), ..entra };
    assert_eq!(
        refused(azure.clone()),
        "No Microsoft Entra ID token is available | Sign in with `az login`, then try again."
    );
    let mut trusting = ConnectionSpec { token: Some("t".into()), ..azure };
    trusting.options.insert("trustServerCertificate".into(), "true".into());
    assert_eq!(
        refused(trusting),
        "Microsoft Entra ID authentication needs a verified server certificate | Remove \
         TrustServerCertificate=true: the token would otherwise be readable by anyone in the middle"
    );
    assert_eq!(
        refused(ConnectionSpec { auth: AuthKind::AwsIam, ..spec() }),
        "Amazon RDS for SQL Server does not support IAM database authentication | Use password authentication for \
         this connection"
    );
    assert_eq!(
        refused(ConnectionSpec { auth: AuthKind::None, ..spec() }),
        "SQL Server needs either password or Microsoft Entra ID authentication"
    );
    assert_eq!(
        said(&crate::conn::redirect_refused("collector.example")),
        "Refusing a redirect from an Azure SQL gateway to 'collector.example', outside Azure SQL"
    );
    assert_eq!(said(&crate::ado::parse_ado("Server=db,abc").unwrap_err()), "Invalid port `abc`");
    assert_eq!(said(&crate::ado::parse_ado("Database=shop").unwrap_err()), "The connection string has no Server");
}

#[test]
fn driver_and_server_errors() {
    use tiberius::error::Error as T;
    let mapped = |e: T| said(&crate::conn::map_error(e));
    assert_eq!(
        mapped(T::Io {
            kind: std::io::ErrorKind::TimedOut,
            message: crate::conn::connect_timed_out("db.example.com:1433")
        }),
        "SQL Server did not respond in time: timed out connecting to db.example.com:1433 | Check the host, port and \
         firewall, or whether the server is under heavy load."
    );
    assert_eq!(
        mapped(T::Io { kind: std::io::ErrorKind::ConnectionRefused, message: "connection refused".into() }),
        "Could not reach SQL Server: connection refused | Check the host and port, and that the server accepts TCP \
         connections from this machine."
    );
    assert_eq!(
        mapped(T::Tls("unknown issuer".into())),
        "TLS handshake failed: unknown issuer | If this is a development server with a self-signed certificate, set \
         TrustServerCertificate=true."
    );
    assert_eq!(
        mapped(T::Routing { host: "worker.example".into(), port: 11000 }),
        "SQL Server redirected twice (last to worker.example:11000)"
    );
    let server = |code: u32, message: &str| said(&crate::conn::server_error(code, message.into()));
    assert_eq!(
        server(18456, "Login failed for user 'reader'."),
        "Login failed for user 'reader'. | Check the user name and password, and that SQL authentication is enabled \
         on the server."
    );
    assert_eq!(
        server(18456, "Login failed for user '<token-identified principal>'."),
        "Login failed for user '<token-identified principal>'. | This Microsoft Entra identity has no user in the \
         database. Ask an admin to run CREATE USER [you@company.com] FROM EXTERNAL PROVIDER;"
    );
    assert_eq!(
        server(4060, "Cannot open database"),
        "Cannot open database | Check the database name and that this login has a user in it."
    );
    assert_eq!(
        server(40615, "Cannot open server"),
        "Cannot open server | Your IP address is not allowed by the server firewall. Add it under the server's \
         networking settings."
    );
    assert_eq!(
        server(40613, "Database is not currently available"),
        "Database is not currently available | The database is temporarily unavailable or throttled. Try again in a \
         few seconds."
    );
    assert_eq!(
        server(1222, "Lock request time out period exceeded."),
        "Lock request time out period exceeded. | A lock wait exceeded Culprit's 2 second limit; the server is busy \
         or something is blocking."
    );
    assert_eq!(
        server(300, "VIEW SERVER STATE permission was denied"),
        "VIEW SERVER STATE permission was denied | Grant the monitoring permission: GRANT VIEW DATABASE STATE TO \
         [<user>]; on Azure SQL Database, or GRANT VIEW SERVER STATE TO [<login>]; on SQL Server."
    );
    assert_eq!(server(208, "Invalid object name 'x'."), "Invalid object name 'x'.");
}

#[test]
fn statement_and_plan_errors() {
    use crate::statement::*;
    let raw = |m: &str| Error::new(culprit_core::ErrorCode::QueryFailed, m);
    assert_eq!(
        said(&parameter_error(raw("Must declare the scalar variable \"@P1\"."))),
        "This statement uses @P1, which SQL Server needs a value for | Replace each parameter with a sample value of \
         its type (a quoted string for text, a number for an id) and try again. Queries Culprit captured open from \
         Queries with the plan SQL Server actually used."
    );
    assert_eq!(
        said(&parameter_error(raw("This syntax is only allowed for parameterized queries."))),
        "This text starts with a parameter list ((@P1 int)…), which SQL Server only accepts from an application | \
         Delete the leading parameter list and replace each parameter with a sample value of its type. Queries \
         Culprit captured open from Queries with the plan SQL Server actually used."
    );
    assert_eq!(
        said(&best_plan(&[]).unwrap_err()),
        "SQL Server returned no plan for this statement | It builds no plan for a statement that reads no table \
         (SELECT 1, a variable assignment) or only declares or sets something. Explain a query that reads a table."
    );
    assert_eq!(result_messages(0), vec!["The statement completed without returning rows.".to_string()]);
    assert!(result_messages(1).is_empty());
    assert_eq!(result_messages(3), vec!["The batch returned 3 result sets; showing the first.".to_string()]);
    assert_eq!(result_messages(1234), vec!["The batch returned 1234 result sets; showing the first.".to_string()]);
    assert_eq!(changed_committed(0), "0 rows changed, committed.");
    assert_eq!(changed_committed(1), "1 row changed, committed.");
    assert_eq!(changed_committed(12345), "12345 rows changed, committed.");
    assert_eq!(committed(), "Committed.");

    let plan_error = |xml: &str| said(&crate::plan_xml::parse(xml).unwrap_err());
    assert!(plan_error("not xml").starts_with("Could not read the SQL Server plan XML: "));
    assert!(
        said(&crate::plan_xml::statement_types("not xml").unwrap_err())
            .starts_with("Could not read the SQL Server plan XML: ")
    );
    assert_eq!(plan_error("<ShowPlanXML/>"), "The plan XML contains no statement with a query plan");
    assert_eq!(
        plan_error("<ShowPlanXML><StmtSimple><QueryPlan/></StmtSimple></ShowPlanXML>"),
        "The query plan has no operators"
    );
}

#[test]
fn actual_plans_progress_and_sessions() {
    use crate::actual::*;
    assert_eq!(said(&ids_are_numbers()), "Query Store ids are numbers");
    assert_eq!(
        said(&cache_unreadable()),
        "This login can't read the plan cache | Grant the monitoring permission: GRANT VIEW DATABASE STATE TO \
         [<user>]; on Azure SQL Database, or GRANT VIEW SERVER STATE TO [<login>]; on SQL Server."
    );
    assert_eq!(
        said(&needs_2019()),
        "Keeping the last actual plan needs SQL Server 2019 or newer | Measure the query instead: open it in the \
         editor with real values and use Analyze."
    );
    assert_eq!(
        said(&not_kept()),
        "SQL Server isn't keeping actual plans for this database | Turn on LAST_QUERY_PLAN_STATS (ALTER DATABASE \
         SCOPED CONFIGURATION SET LAST_QUERY_PLAN_STATS = ON;), let the query run once, then look again."
    );
    assert_eq!(
        said(&not_cached()),
        "This query's plan isn't in the plan cache right now | SQL Server keeps the last actual plan only while the \
         plan is cached. It comes back the next time the query runs."
    );
    assert_eq!(own_estimate("BACKUP DATABASE"), "SQL Server's own estimate for BACKUP DATABASE");
    assert_eq!(
        no_profiling(),
        "Per-operator progress needs lightweight query profiling: on by default since SQL Server 2019 \
         (LIGHTWEIGHT_QUERY_PROFILING), trace flag 7412 on 2016 SP1 and 2017. Culprit shows the estimated plan and \
         how long the statement has been running."
    );
    use crate::live::*;
    assert_eq!(
        said(&cancel_unsupported()),
        "SQL Server can't stop another session's statement without ending the session | End the session instead; \
         its open transaction rolls back."
    );
    assert_eq!(said(&session_gone(51)), "Session 51 is gone");
    assert_eq!(said(&own_session(52)), "Session 52 is Culprit's own");
    assert_eq!(said(&system_session(5)), "Session 5 is a SQL Server system session");
    assert_eq!(
        said(&crate::waits_unreadable("GRANT VIEW SERVER STATE TO [reader];".into())),
        "Sampling waits needs permission to read the server's wait statistics | GRANT VIEW SERVER STATE TO [reader];"
    );
    assert_eq!(said(&crate::preview_stopped()), "The table preview worker stopped");
}

fn probe() -> Probe {
    Probe {
        version: "16.0.4135.4".into(),
        major: 16,
        edition: "Standard Edition (64-bit)".into(),
        engine_edition: 3,
        database: "shop".into(),
        data_bytes: None,
        can_view_database_state: false,
        can_view_server_state: false,
        can_showplan: false,
        login: "reader".into(),
        user: "reader".into(),
        started_at: None,
        query_store: Some(QueryStoreState { actual: "READ_WRITE".into(), options: Rec::default() }),
        rds: false,
    }
}

fn reasons(missing: &[culprit_core::MissingCapability]) -> String {
    missing
        .iter()
        .map(|m| format!("{:?}: {} | {}", m.capability, m.reason, m.fix.as_deref().unwrap_or("-")))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn what_a_login_can_read() {
    assert_eq!(Probe { major: 18, ..probe() }.product(), "SQL Server (version 18)");
    let (_, missing) = crate::probe::expected_capabilities(&probe());
    assert_eq!(
        reasons(&missing),
        "QueryStore: This login cannot read Query Store views. | GRANT VIEW SERVER PERFORMANCE STATE TO [reader];
TableStats: This login lacks VIEW DATABASE STATE / VIEW SERVER STATE. | GRANT VIEW SERVER PERFORMANCE STATE TO [reader];
IndexUsage: This login lacks VIEW DATABASE STATE / VIEW SERVER STATE. | GRANT VIEW SERVER PERFORMANCE STATE TO [reader];
MissingIndexes: This login lacks VIEW DATABASE STATE / VIEW SERVER STATE. | GRANT VIEW SERVER PERFORMANCE STATE TO [reader];
WaitStats: This login lacks VIEW DATABASE STATE / VIEW SERVER STATE. | GRANT VIEW SERVER PERFORMANCE STATE TO [reader];
Sessions: This login lacks VIEW DATABASE STATE / VIEW SERVER STATE. | GRANT VIEW SERVER PERFORMANCE STATE TO [reader];
Showplan: This login cannot request estimated plans. | GRANT SHOWPLAN TO [reader];"
    );
    let off = Probe {
        query_store: Some(QueryStoreState { actual: "READ_ONLY_REQUESTED".into(), options: Rec::default() }),
        can_view_database_state: true,
        can_showplan: true,
        ..probe()
    };
    assert_eq!(
        reasons(&crate::probe::expected_capabilities(&off).1),
        "QueryStore: Query Store reports the state “READ_ONLY_REQUESTED” on this database. | ALTER DATABASE CURRENT SET QUERY_STORE = \
         ON (OPERATION_MODE = READ_WRITE);"
    );
    let unknown = Probe { query_store: None, can_view_database_state: true, can_showplan: true, ..probe() };
    assert_eq!(
        reasons(&crate::probe::expected_capabilities(&unknown).1),
        "QueryStore: Query Store options could not be read. | GRANT VIEW SERVER PERFORMANCE STATE TO [reader];"
    );
}

#[test]
fn what_a_scan_notes() {
    use crate::collect::*;
    let account = |probe: &Probe| {
        let mut c = Coverage::default();
        query_store_account(&mut c, probe);
        let outcomes: Vec<String> = c.outcomes().into_iter().map(|o| o.reason.unwrap_or_default()).collect();
        let (_, missing, _) = c.finish();
        format!("{}\n{}", outcomes.join("\n"), reasons(&missing))
    };
    assert_eq!(
        account(&Probe { query_store: None, ..probe() }),
        "Query Store requires SQL Server 2016 or later and a user database.
QueryStore: Query Store is not available here; it needs SQL Server 2016 or later and a user database. | -"
    );
    assert_eq!(
        account(&Probe {
            query_store: Some(QueryStoreState { actual: "OFF".into(), options: Rec::default() }),
            ..probe()
        }),
        "Query Store is OFF on this database.
QueryStore: Query Store is off on this database, so workload history is limited to what is still in the plan cache. \
         | ALTER DATABASE CURRENT SET QUERY_STORE = ON (OPERATION_MODE = READ_WRITE);"
    );
    assert_eq!(unparsed_plans(3).render(), "3 Query Store plans could not be parsed");
    assert_eq!(unparsed_plans(1).render(), "1 Query Store plan could not be parsed");
    let mut c = Coverage::default();
    plan_cache_unreadable(&mut c, &probe());
    index_usage_unreadable(&mut c, &probe());
    server_state_unreadable(&mut c, &probe());
    assert_eq!(c.outcomes()[0].reason.as_deref(), Some("This login cannot read plan cache statistics."));
    assert_eq!(
        reasons(&c.finish().1),
        "PlanCache: This login cannot read plan cache statistics. | GRANT VIEW SERVER PERFORMANCE STATE TO [reader];
IndexUsage: Index usage counters are not readable with this login, so unused-index checks are off. | GRANT VIEW SERVER PERFORMANCE STATE TO [reader];
MissingIndexes: This login lacks VIEW DATABASE STATE / VIEW SERVER STATE. | GRANT VIEW SERVER PERFORMANCE STATE TO [reader];
WaitStats: This login lacks VIEW DATABASE STATE / VIEW SERVER STATE. | GRANT VIEW SERVER PERFORMANCE STATE TO [reader];"
    );
    assert_eq!(
        crate::workload::waits_grant(),
        "Grant VIEW DATABASE STATE (VIEW DATABASE PERFORMANCE STATE on SQL Server 2022+)."
    );
    assert_eq!(
        crate::workload::waits_not_captured(),
        "Query Store wait capture is disabled or could not be established; missing wait categories are unknown."
    );
}

#[test]
fn deadlock_sources() {
    use crate::deadlocks::*;
    assert_eq!(telemetry(), "Azure SQL deadlock telemetry");
    assert_eq!(
        telemetry_unreadable(&"Invalid object name"),
        "Culprit couldn't read Azure SQL Database's deadlock telemetry (Invalid object name). Deadlocks are also in \
         Azure Monitor (diagnostic setting \"Deadlocks\"), or paste a deadlock graph (.xdl) to have it explained."
    );
    assert_eq!(origin(true, true), ("system_health event files and ring buffer".into(), None));
    assert_eq!(origin(true, false), ("system_health event files".into(), None));
    assert_eq!(
        origin(false, true),
        (
            "system_health ring buffer".into(),
            Some("Read from system_health's in-memory buffer, which only keeps the most recent events.".into())
        )
    );
    assert_eq!(
        origin(false, false),
        (
            "system_health".into(),
            Some(
                "Reading deadlocks needs VIEW SERVER STATE (VIEW SERVER PERFORMANCE STATE on SQL Server 2022). Paste \
                 a deadlock graph (.xdl) to have it explained."
                    .into()
            )
        )
    );
    let unreadable = said(&parse_deadlock_xml("not xml", "pasted").unwrap_err());
    assert!(unreadable.starts_with("This isn't a deadlock graph Culprit can read: "), "{unreadable}");
    assert_eq!(
        said(&parse_deadlock_xml("<graph/>", "pasted").unwrap_err()),
        "No <deadlock> element found | Save the deadlock graph from SSMS or system_health as .xdl XML."
    );
}

#[test]
fn changes_and_spaces() {
    use crate::recorded::title;
    let lines: Vec<String> = ["U", "V", "P", "FN", "TR"]
        .into_iter()
        .flat_map(|kind| {
            [EventAction::Created, EventAction::Changed].into_iter().flat_map(move |action| {
                [None, Some("sales.orders")]
                    .into_iter()
                    .map(move |parent| title(kind, action, "audit", "sales.audit", parent).render())
            })
        })
        .collect();
    assert_eq!(
        lines.join("\n"),
        "Table sales.audit was created
Table sales.audit was created
Table sales.audit was altered (its columns, constraints or indexes)
Table sales.audit was altered (its columns, constraints or indexes)
View sales.audit was created
View sales.audit was created
View sales.audit was altered
View sales.audit was altered
Procedure sales.audit was created
Procedure sales.audit was created
Procedure sales.audit was altered
Procedure sales.audit was altered
Function sales.audit was created
Function sales.audit was created
Function sales.audit was altered
Function sales.audit was altered
Trigger sales.audit was created
Trigger audit on sales.orders was created
Trigger sales.audit was altered
Trigger audit on sales.orders was altered"
    );
    use crate::operations::*;
    assert_eq!(log_name("shop"), "shop transaction log");
    assert_eq!(filegroup_name("shop", "PRIMARY"), "shop data filegroup PRIMARY");
    assert_eq!(tempdb_name(), "tempdb data files (whole instance)");
    let parts = tempdb_parts(&Rec::of(&[
        ("users", Val::Float(1.0)),
        ("internals", Val::Float(2.0)),
        ("versions", Val::Float(3.0)),
    ]));
    assert_eq!(
        parts.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["User objects", "Internal objects", "Version store"]
    );
}

#[test]
fn server_signals() {
    use crate::pressure::*;
    let counters: HashMap<String, f64> = [
        ("Batch Requests/sec", 3_600_000.0),
        ("SQL Compilations/sec", 400_000.0),
        ("SQL Re-Compilations/sec", 40_000.0),
        ("Page life expectancy", 1200.0),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect();
    assert_eq!(
        signals(&counter_signals(&counters, Some(10.0), false)),
        "Batch requests: -
Plans compiled: under 10% of batches is healthy
Plans recompiled: -
Page life expectancy: how long a page stays in memory once read: five minutes or more is healthy; under a minute means pages leave memory soon after they're read, often because of large scans"
    );
    assert_eq!(
        signals(&counter_signals(&counters, Some(10_000.0), false)).lines().nth(1),
        Some("Plans compiled: too few batches to judge; under 10% is healthy under load")
    );
    let shared = "covers every database on the server behind this one (its elastic pool's, if it has one), so it isn't graded here";
    assert_eq!(
        signals(&counter_signals(&counters, Some(10.0), true)),
        format!(
            "Batch requests (whole server): {shared}
Plans compiled (whole server): {shared}
Plans recompiled (whole server): {shared}
Page life expectancy (whole server): {shared}"
        )
    );
    assert_eq!(
        signals(&resource_signals(40.0, 86.43, 70.0)),
        "Storage I/O of its limit: Average use of this database's I/O limit; peak 86.4%. The individual resource samples establish whether pressure was sustained.
Memory in use: of its memory limit; a full cache is normal, so it isn't graded"
    );
    let totals: HashMap<String, (f64, f64, f64)> =
        [("THREADPOOL".to_string(), (5000.0, 100.0, 3.0)), ("RESOURCE_SEMAPHORE".to_string(), (2000.0, 50.0, 2.0))]
            .into_iter()
            .collect();
    let scheduling = "Scheduling share of recorded waits: Share of recorded waiting spent ready to run before being scheduled. This is not CPU utilization; the ratio alone does not establish CPU pressure.";
    assert_eq!(
        signals(&wait_total_signals(&totals, false, Some(2.0))),
        format!(
            "{scheduling}
Waiting for workers since start (whole instance): -
Waiting for memory grants since start (whole instance): -
Waiting for workers, per hour: under a second an hour is healthy"
        )
    );
    assert_eq!(
        signals(&wait_total_signals(&totals, true, Some(0.5))),
        format!(
            "{scheduling}
Waiting for workers since start: -
Waiting for memory grants since start: -
Waiting for workers since start: under a second an hour is healthy"
        )
    );
    assert_eq!(
        signals(&[
            pending_grants_signal(0.0),
            threadpool_now_signal(0.0),
            version_store_signal(1024.0),
            oldest_transaction_signal(30.0),
        ]),
        "Waiting for memory: 0 is healthy
Requests in this database without a worker: One observation; sample again to establish whether the queue persists
Version store: small, and shrinking when transactions end
Oldest open transaction: seconds or minutes, not hours"
    );
    let rates: HashMap<&str, f64> =
        [("Batch Requests/sec", 100.0), ("SQL Compilations/sec", 20.0), ("SQL Re-Compilations/sec", 2.0)]
            .into_iter()
            .collect();
    assert_eq!(
        signals(&sampled_signals(|name| rates.get(name).copied(), 10.0, 100.0, false)),
        format!(
            "Batch requests: -
Plans compiled (whole instance): -
Recompiles: -
{scheduling}"
        )
    );
    assert_eq!(signals(&sampled_signals(|name| rates.get(name).copied(), 10.0, 100.0, true)), scheduling);
    assert_eq!(
        sample_method("sys.dm_os_wait_stats", 15),
        "Difference of two readings of sys.dm_os_wait_stats 15 s apart"
    );
    assert!(sample_notes(true).is_empty());
    assert_eq!(
        sample_notes(false),
        vec![
            "These wait counters cover the whole SQL Server instance, including other databases; they do not \
             establish pressure in the selected database."
                .to_string()
        ]
    );
    use crate::signals::*;
    let context = "Only user queries retained by this database's Query Store; first compile 2026-10-01T00:00:00, most recent 2026-10-02T00:00:00. Counts cover those entries' history, not the selected runtime window. Compilation duration is elapsed time, not measured CPU usage.";
    assert_eq!(
        signals(&compile_signals(10.0, 50.0, "2026-10-01T00:00:00", "2026-10-02T00:00:00")),
        format!("Compiles recorded: {context}\nCompilation time: {context}")
    );
    assert_eq!(
        signals(&login_signals(Some(1000.0), Some(9000.0), Some(100.0), false)),
        "New connections: under 5 a second: pooled connections log in once and are reused
Connections served from a pool: above 90% when the application pools connections"
    );
    assert_eq!(
        signals(&login_signals(Some(1000.0), Some(9000.0), Some(100.0), true)),
        format!("New connections (whole server): {shared}\nConnections served from a pool (whole server): {shared}")
    );
}
