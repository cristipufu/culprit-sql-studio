//! Command execution lives in focused functions so the async dispatcher does not reserve every command's stack frame.

use std::path::PathBuf;

use culprit_core::{Error, ExplainMode, Result, SessionAction, Severity, SnapshotOptions, t};
use culprit_engine::Workspace;

use crate::output::Out;
use crate::{
    ChangesArgs, ExplainArgs, FindingsArgs, LiveArgs, RunArgs, SavedCommand, ScanArgs, TopArgs, fail_code, load,
    report_file, window_minutes,
};

pub(super) async fn doctor(ws: &Workspace, connection: &str, out: &Out) -> Result<u8> {
    let info = ws.connection(connection)?;
    let server = ws.probe(connection).await?;
    let locked = ws.locked_checks(&server);
    out.doctor(&info, &server, &locked);
    Ok(0)
}

pub(super) async fn scan(ws: &Workspace, args: ScanArgs, out: &Out) -> Result<u8> {
    let window = window_minutes(args.window, Some(args.window_days)).unwrap_or_default();
    let options = SnapshotOptions { top_queries: args.top, plans: args.plans, ..SnapshotOptions::default() }
        .with_window_minutes(window);
    out.progress(&t!("cli.scanning", connection = &args.connection));
    let report = ws.scan_with_log(&args.connection, &options, args.query_log.as_deref()).await?;
    out.scan(&report, args.full, &ws.paths().reports_dir(&args.connection));
    Ok(fail_code(&report, args.fail_on))
}

pub(super) fn findings(ws: &Workspace, args: FindingsArgs, out: &Out) -> Result<u8> {
    let report = load(ws, &args.connection, args.report.as_ref())?;
    let min: Severity = args.min_severity.map(Into::into).unwrap_or(Severity::Low);
    out.findings(&report, min);
    Ok(0)
}

pub(super) fn show(
    ws: &Workspace,
    connection: String,
    finding: String,
    report: Option<PathBuf>,
    out: &Out,
) -> Result<u8> {
    let report = load(ws, &connection, report.as_ref())?;
    let f = report.findings.iter().find(|f| f.id == finding).ok_or_else(|| {
        Error::not_found(t!("engine.share.no_finding", id = &finding, name = &connection))
            .with_hint(t!("cli.list_findings", connection = &connection))
    })?;
    out.show(&report, f);
    Ok(0)
}

pub(super) fn top(ws: &Workspace, args: TopArgs, out: &Out) -> Result<u8> {
    let report = load(ws, &args.connection, args.report.as_ref())?;
    out.top(&report, args.by, args.limit);
    Ok(0)
}

pub(super) async fn live(ws: &Workspace, args: LiveArgs, out: &Out) -> Result<u8> {
    let target = match (args.cancel, args.kill) {
        (Some(id), _) => Some((id, SessionAction::Cancel)),
        (None, Some(id)) => Some((id, SessionAction::Terminate)),
        (None, None) => None,
    };
    if let Some((id, action)) = target {
        ws.interrupt_session(&args.connection, &id, action).await?;
        out.done(&match action {
            SessionAction::Cancel => t!("cli.live.cancelled", session = &id),
            SessionAction::Terminate => t!("cli.live.ended", session = &id),
        });
        return Ok(0);
    }
    if let Some(session) = args.progress.as_deref() {
        let mut first = true;
        loop {
            let progress = ws.query_progress(&args.connection, session, first || !args.watch).await?;
            out.query_progress(&progress, !first);
            if !args.watch || !progress.running {
                return Ok(0);
            }
            first = false;
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
    }
    let live = ws.live(&args.connection).await?;
    out.live(&live);
    Ok(0)
}

pub(super) async fn run_query(ws: &Workspace, args: RunArgs, out: &Out) -> Result<u8> {
    let rs = ws.run(&args.connection, &args.sql, args.limit, args.yes_execute_on_production).await?;
    out.result_set(&rs);
    Ok(0)
}

pub(super) async fn saved(ws: &Workspace, command: SavedCommand, out: &Out) -> Result<u8> {
    match command {
        SavedCommand::List => out.saved_queries(&ws.saved_queries()?),
        SavedCommand::Show { name } => out.saved_query(&ws.saved_query(&name)?),
        SavedCommand::Add { name, sql, file, connection, description } => {
            let sql = match (sql, file) {
                (Some(sql), _) => sql,
                (None, Some(path)) => std::fs::read_to_string(&path)
                    .map_err(|e| culprit_core::Error::invalid(format!("{}: {e}", path.display())))?,
                (None, None) => return Err(culprit_core::Error::invalid(t!("cli.saved.pass_sql"))),
            };
            let query = culprit_engine::saved::SaveQuery { name, sql, connection, description, previous_name: None };
            out.saved_query(&ws.save_query(query)?);
        }
        SavedCommand::Rm { name } => {
            ws.delete_saved_query(&name)?;
            out.done(&t!("cli.saved.forgot", name = &name));
        }
        SavedCommand::Run { name, connection, limit, yes_execute_on_production } => {
            let rs = ws.run_saved(&name, connection.as_deref(), limit, yes_execute_on_production).await?;
            out.result_set(&rs);
        }
    }
    Ok(0)
}

pub(super) async fn catalog(ws: &Workspace, connection: &str, out: &Out) -> Result<u8> {
    out.catalog(&ws.catalog(connection).await?);
    Ok(0)
}

pub(super) fn drift(ws: &Workspace, a: String, b: String, out: &Out) -> Result<u8> {
    let drift = match (report_file(&a), report_file(&b)) {
        (None, None) => ws.drift(&a, &b)?,
        (fa, fb) => {
            let side = |name: &str, file: Option<PathBuf>| match file {
                Some(path) => ws.analyze_file(&path),
                None => load(ws, name, None).map(|mut r| {
                    r.snapshot.connection = name.to_string();
                    r
                }),
            };
            culprit_engine::drift_reports(&side(&a, fa)?, &side(&b, fb)?)?
        }
    };
    out.drift(&drift);
    Ok(0)
}

pub(super) async fn changes(ws: &Workspace, args: ChangesArgs, out: &Out) -> Result<u8> {
    out.changes(&ws.changes(&args.connection, &args.request()).await?);
    Ok(0)
}

pub(super) fn indexes(ws: &Workspace, connection: String, out: &Out) -> Result<u8> {
    let advice = match report_file(&connection) {
        Some(path) => culprit_engine::index_advice(&ws.analyze_file(&path)?),
        None => ws.index_advice(&connection)?,
    };
    out.index_advice(&advice);
    Ok(0)
}

pub(super) fn analyze(ws: &Workspace, file: PathBuf, connection: Option<String>, out: &Out) -> Result<u8> {
    let mut report = ws.analyze_file(&file)?;
    if let Some(name) = connection {
        report.snapshot.connection = name;
    }
    out.analyzed(&report);
    Ok(0)
}

pub(super) async fn explain(ws: &Workspace, args: ExplainArgs, out: &Out) -> Result<u8> {
    let plan = if args.last_actual {
        let query_id = args.query_id.as_deref().unwrap_or_default();
        ws.last_actual_plan(&args.connection, query_id, args.plan_id.as_deref()).await?
    } else if args.query_id.is_some() && !args.analyze && args.with_index.is_empty() {
        let query_id = args.query_id.as_deref().unwrap_or_default();
        ws.query_plan(&args.connection, query_id, args.plan_id.as_deref()).await?
    } else {
        let sql = explain_sql(ws, &args)?;
        if !args.with_index.is_empty() {
            ws.explain_hypothetical(&args.connection, &sql, &args.with_index).await?
        } else {
            let mode = if args.analyze { ExplainMode::Analyze } else { ExplainMode::Estimated };
            ws.explain(&args.connection, &sql, mode, args.yes_execute_on_production).await?
        }
    };
    out.plan(&plan);
    Ok(0)
}

fn explain_sql(ws: &Workspace, args: &ExplainArgs) -> Result<String> {
    if let Some(sql) = &args.sql {
        return Ok(sql.clone());
    }
    if let Some(file) = &args.file {
        return std::fs::read_to_string(file).map_err(|e| Error::invalid(format!("{}: {e}", file.display())));
    }
    if let Some(id) = &args.query_id {
        let report = load(ws, &args.connection, None)?;
        let q = report
            .snapshot
            .workload
            .as_ref()
            .and_then(|w| w.query(id))
            .ok_or_else(|| Error::not_found(t!("cli.explain.no_query", id = id)))?;
        return Ok(q.text.clone());
    }
    Err(Error::invalid(t!("cli.explain.pass_sql")))
}
