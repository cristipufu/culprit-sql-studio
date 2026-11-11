//! Database operations and their execution policy. Connections resolve providers and credentials; Reports supplies
//! the selected scan for context. This service keeps confirmation, production and read-only guards in one place.

use crate::{ConnectionInfo, catalog, connection::Connections, deadlocks, reporting::Reports};
use culprit_core::{
    DeadlockReport, Engine, Error, ErrorCode, ExplainMode, LiveState, Plan, Report, Result, ResultSet, SessionAction, t,
};

pub(crate) struct Execution<'a> {
    pub connections: Connections<'a>,
    pub reports: Reports<'a>,
    pub previews: &'a tokio::sync::Semaphore,
}

impl Execution<'_> {
    pub(crate) async fn preview_table(
        &self,
        name: &str,
        options: &culprit_core::TablePreviewOptions,
    ) -> Result<culprit_core::TablePreview> {
        let _permit = self.previews.try_acquire().map_err(|_| {
            Error::new(ErrorCode::QueryFailed, t!("engine.execution.preview_running"))
                .with_hint(t!("engine.execution.preview_running_hint"))
        })?;
        let info = self.connections.connection(name)?;
        let sql = culprit_core::sql::table_preview_sql(info.engine, options)?;
        let result = self
            .read(name, &sql, options.limit.clamp(1, culprit_core::limits::MAX_RESULT_ROWS), true)
            .await
            .map_err(|error| {
            if error.code == ErrorCode::Timeout {
                error.with_hint(format!(
                    "{} {}",
                    t!("engine.execution.preview_timeout"),
                    t!("engine.execution.preview_timeout_editor")
                ))
            } else {
                error
            }
        })?;
        Ok(culprit_core::TablePreview { sql, result })
    }

    /// The dashboard for a connection's latest report (the one scanned with a window of `window_minutes`, when given).
    /// With `live`, sessions are read once too; if that fails, the "right now" tile says why and the rest still comes
    /// back.
    pub(crate) async fn dashboard(
        &self,
        name: &str,
        window_minutes: Option<u32>,
        live: bool,
    ) -> Result<culprit_core::Dashboard> {
        let report_name = name.to_owned();
        let dashboard = self
            .reports
            .background(move |reports| {
                let report = reports
                    .latest_report(&report_name, window_minutes)?
                    .ok_or_else(|| crate::reporting::no_saved_report(&report_name))?;
                let mut dashboard = culprit_checks::dashboard(&report);
                dashboard.connection = report_name;
                Ok(dashboard)
            })
            .await?;
        // An engine without sessions (SQLite) has none to read; its tile says so already.
        if live && dashboard.engine.abilities().sessions {
            return Ok(self.dashboard_live(name, dashboard).await?.dashboard);
        }
        Ok(dashboard)
    }

    /// The dashboard for the report already displayed by the caller, even if a newer scan was saved in the meantime.
    pub(crate) fn dashboard_for_report(&self, name: &str, report_id: &str) -> Result<culprit_core::Dashboard> {
        self.connections.connection(name)?;
        let mut dashboard = culprit_checks::dashboard(&self.reports.report_at(name, report_id)?);
        dashboard.connection = name.into();
        Ok(dashboard)
    }

    /// Refresh sessions without reloading another report or selected window. One read, with Rust's shared assessment.
    pub(crate) async fn dashboard_live(
        &self,
        name: &str,
        dashboard: culprit_core::Dashboard,
    ) -> Result<culprit_core::DashboardLive> {
        let connection = self.connections.connection(name)?;
        if dashboard.connection != name || dashboard.engine != connection.engine {
            return Err(Error::invalid(t!("engine.execution.live_elsewhere")));
        }
        if !connection.engine.abilities().sessions {
            return Ok(culprit_core::DashboardLive { dashboard, live: None, error: None });
        }
        Ok(culprit_checks::dashboard::refresh(dashboard, self.live(name).await))
    }

    /// Explain without running the statement, or measure it with Analyze. Production measurements only run proven reads.
    pub(crate) async fn explain(&self, name: &str, sql: &str, mode: ExplainMode, confirmed: bool) -> Result<Plan> {
        let info = self.connections.connection(name)?;
        // One statement only: Analyze runs it inside a transaction that's rolled back, and a second statement
        // (`… COMMIT …` on SQL Server, which needs no semicolons) could end that transaction early.
        culprit_core::sql::single_statement(sql, info.engine)?;
        guard_execution(&info, sql, mode == ExplainMode::Analyze, confirmed)?;
        let provider = self.connections.provider(name).await?;
        crate::operation::within(
            if mode == ExplainMode::Analyze {
                culprit_core::limits::QUERY_TIMEOUT
            } else {
                culprit_core::limits::OBSERVE_TIMEOUT
            },
            async {
                // A measured run executes the text inside a rolled-back transaction: the server must see one statement (and on
                // production, one read), or a second statement could end the transaction.
                if mode == ExplainMode::Analyze
                    && let Some(types) = self.connections.forget_rejected(name, provider.statement_types(sql).await)?
                {
                    let seen = reads_as(&types);
                    if types.len() != 1 {
                        return Err(Error::invalid(t!("engine.execution.one_statement")).with_hint(seen));
                    }
                    if info.environment.is_production() && !is_read_type(&types[0]) {
                        let message = t!("engine.execution.analyze_production", name = &info.name);
                        return Err(Error::new(ErrorCode::PermissionDenied, message)
                            .with_hint(format!("{seen} {}", t!("engine.execution.analyze_production_hint"))));
                    }
                }
                let mut plan = self.connections.forget_rejected(name, provider.explain(sql, mode).await)?;
                culprit_core::insights::annotate(&mut plan);
                culprit_checks::present_plan(&mut plan, None);
                Ok(plan)
            },
        )
        .await
    }

    /// Estimated plan of `sql` with one or more virtual indexes (`CREATE INDEX ...`) in planner memory (`hypopg`).
    pub(crate) async fn explain_hypothetical(&self, name: &str, sql: &str, indexes: &[String]) -> Result<Plan> {
        let info = self.connections.connection(name)?;
        culprit_core::sql::single_statement(sql, info.engine)?;
        for idx in indexes {
            culprit_core::sql::single_statement(idx, info.engine)?;
            if !culprit_core::sql::creates_index(idx, info.engine) {
                return Err(Error::invalid(t!("engine.execution.not_an_index")));
            }
        }
        let provider = self.connections.provider(name).await?;
        crate::operation::within(culprit_core::limits::OBSERVE_TIMEOUT, async {
            let mut plan = self.connections.forget_rejected(name, provider.explain_hypothetical(sql, indexes).await)?;
            culprit_core::insights::annotate(&mut plan);
            culprit_checks::present_plan(&mut plan, None);
            Ok(plan)
        })
        .await
    }

    /// Compare a statement's current estimated plan against its estimated plan with one or more virtual indexes
    /// (`CREATE INDEX ...`) in planner memory (`hypopg`), returning both plans, their structural diff, and whether
    /// the virtual index was chosen.
    pub(crate) async fn verify_index(
        &self,
        name: &str,
        sql: &str,
        indexes: &[String],
    ) -> Result<culprit_core::HypotheticalIndexVerification> {
        if indexes.is_empty() {
            return Err(Error::invalid(t!("engine.execution.no_index")));
        }
        let hypothetical = self.explain_hypothetical(name, sql, indexes).await?;
        let baseline = self.explain(name, sql, ExplainMode::Estimated, false).await?;
        Ok(culprit_core::verify_hypothetical_index(baseline, hypothetical))
    }

    /// The actual plan of a query's last run as the engine kept it (SQL Server 2019+ with LAST_QUERY_PLAN_STATS).
    /// Never runs the query; other engines say why they can't and how to measure it instead.
    pub(crate) async fn last_actual_plan(&self, name: &str, query_id: &str, plan_id: Option<&str>) -> Result<Plan> {
        self.last_actual_plan_at(name, query_id, plan_id, None).await
    }

    pub(crate) async fn last_actual_plan_at(
        &self,
        name: &str,
        query_id: &str,
        plan_id: Option<&str>,
        report_id: Option<&str>,
    ) -> Result<Plan> {
        let report_name = name.to_owned();
        let report_id = report_id.map(str::to_owned);
        let report =
            self.reports.background(move |reports| reports.plan_report(&report_name, report_id.as_deref())).await?;
        let observed = self.connections.observed_provider(name).await?;
        let provider = observed.provider();
        let result = crate::operation::within(culprit_core::limits::OBSERVE_TIMEOUT, async {
            let plan = self.connections.forget_rejected(name, provider.last_actual_plan(query_id, plan_id).await)?;
            Ok(Self::contextual_plan(plan, report.as_ref().map(|r| &r.snapshot), query_id, plan_id))
        })
        .await;
        observed.finish(result).await
    }

    /// How far along a session's running statement is: per-operator progress on SQL Server, the estimated plan with
    /// elapsed time and command/stage progress on Postgres and MySQL. Reads only; works on production too.
    pub(crate) async fn query_progress(
        &self,
        name: &str,
        session: &str,
        with_plan: bool,
    ) -> Result<culprit_core::QueryProgress> {
        self.connections.connection(name)?.engine.spec().validate_session_id(session)?;
        let observed = self.connections.observed_provider(name).await?;
        let provider = observed.provider();
        let result = crate::operation::within(culprit_core::limits::OBSERVE_TIMEOUT, async {
            let mut progress =
                self.connections.forget_rejected(name, provider.query_progress(session, with_plan).await)?;
            if let Some(plan) = &mut progress.plan {
                culprit_checks::present_progress_plan(plan);
            }
            Ok(progress)
        })
        .await;
        observed.finish(result).await
    }

    /// A plan for a query from the latest report: the stored plan when the engine keeps them (Query Store), otherwise an
    /// estimated plan of the query text. Never executes the statement.
    pub(crate) async fn query_plan(&self, name: &str, query_id: &str, plan_id: Option<&str>) -> Result<Plan> {
        self.query_plan_at(name, query_id, plan_id, None).await
    }

    /// Fetch a plan using the exact report/window the caller is displaying.
    pub(crate) async fn query_plan_at(
        &self,
        name: &str,
        query_id: &str,
        plan_id: Option<&str>,
        report_id: Option<&str>,
    ) -> Result<Plan> {
        let report_name = name.to_owned();
        let report_id = report_id.map(str::to_owned);
        let report =
            self.reports.background(move |reports| reports.plan_report(&report_name, report_id.as_deref())).await?;
        let plan = self
            .connections
            .forget_rejected(name, self.query_plan_from(name, query_id, plan_id, report.as_ref()).await)?;
        Ok(Self::contextual_plan(plan, report.as_ref().map(|r| &r.snapshot), query_id, plan_id))
    }

    fn contextual_plan(
        mut plan: Plan,
        snapshot: Option<&culprit_core::Snapshot>,
        query_id: &str,
        plan_id: Option<&str>,
    ) -> Plan {
        if let Some(snapshot) = snapshot {
            if let Some(history) = snapshot
                .queries()
                .iter()
                .find(|q| q.id == query_id)
                .and_then(|q| q.plan_history.as_ref())
                .filter(|h| h.kind == culprit_core::PlanHistoryKind::Cache && plan_id.is_some())
            {
                culprit_checks::present_retained_plan(&mut plan, &history.note);
                return plan;
            }
            culprit_core::insights::mark_plan(&mut plan, snapshot);
            if let Some(q) = snapshot.queries().iter().find(|q| q.id == query_id) {
                culprit_core::insights::mark_query(&mut plan, &q.text);
                if (plan_id.is_none() || q.plan_history_matches_workload())
                    && plan_id.is_none_or(|id| q.plan_id.as_deref() == Some(id) || q.plans.iter().any(|p| p.id == id))
                {
                    let mut measured = q.clone();
                    if let Some(id) = plan_id {
                        measured.plan_id = Some(id.into());
                    }
                    plan.insights = Some(culprit_checks::query_plan_insights(snapshot, &measured, &plan));
                    culprit_checks::present_plan(&mut plan, Some(&measured));
                    return plan;
                }
            }
        }
        let mut items = culprit_core::insights::insights(&plan);
        for i in &mut items {
            i.severity = culprit_core::Severity::Low;
            i.fix_sql = None;
        }
        let note = if plan.facts().constants_redacted {
            [
                t!("engine.execution.retained.independent"),
                t!("engine.execution.retained.unmeasured"),
                t!("engine.execution.retained.structural"),
            ]
            .join(" ")
        } else {
            [t!("engine.execution.retained.no_match"), t!("engine.execution.retained.structural")].join(" ")
        };
        plan.insights =
            Some(culprit_core::insights::PlanInsights { scan_context: Some(true), items, note: Some(note) });
        culprit_checks::present_plan(&mut plan, None);
        plan
    }

    async fn query_plan_from(
        &self,
        name: &str,
        query_id: &str,
        plan_id: Option<&str>,
        report: Option<&Report>,
    ) -> Result<Plan> {
        if let Some(id) = plan_id
            && let Some(history) = report
                .and_then(|r| r.snapshot.queries().iter().find(|q| q.id == query_id))
                .and_then(|q| q.plan_history.as_ref())
            && history.captured_plan_id.as_deref() == Some(id)
            && let Some(plan) = &history.captured_plan
        {
            return Ok(plan.clone());
        }
        let observed = self.connections.observed_provider(name).await?;
        let provider = observed.provider();
        let result = crate::operation::within(culprit_core::limits::OBSERVE_TIMEOUT, async {
            if let Some(plan_id) = plan_id {
                return provider.stored_plan(query_id, plan_id).await?.ok_or_else(|| {
                    Error::not_found(t!("engine.execution.no_stored_plan", plan = plan_id, query = query_id))
                });
            }
            let query = report
                .ok_or_else(|| Error::not_found(t!("engine.execution.no_report", name = name)))?
                .snapshot
                .workload
                .as_ref()
                .and_then(|w| w.query(query_id))
                .ok_or_else(|| Error::not_found(t!("engine.execution.no_query", query = query_id, name = name)))?;
            if let Some(plan_id) = query.plan_id.as_ref().or_else(|| query.plans.first().map(|p| &p.id))
                && query.plan_history_matches_workload()
                && let Some(plan) = provider.stored_plan(query_id, plan_id).await?
            {
                return Ok(plan);
            }
            match (provider.explain_recorded(&query.text, Some(query_id)).await, &query.plan) {
                (Ok(plan), _) => Ok(plan),
                // Normalized text (`WHERE id = ?`) can't be planned again; the scan planned a real sample of it.
                (Err(e), Some(captured))
                    if e.code != ErrorCode::AuthFailed && !crate::source_operation::native_shutdown_uncertain(&e) =>
                {
                    Ok(captured.clone())
                }
                (Err(e), _) => {
                    let hint =
                        e.hint_source().unwrap_or_else(|| culprit_core::message!("engine.execution.needs_values"));
                    Err(e.with_hint(hint))
                }
            }
        })
        .await;
        observed.finish(result).await
    }

    /// What the server waits on right now: a sample `seconds` long (capped at 60). Reads statistics views only.
    pub(crate) async fn wait_sample(&self, name: &str, seconds: u32) -> Result<culprit_core::WaitSample> {
        let seconds = seconds.clamp(1, 60);
        let observed = self.connections.observed_provider(name).await?;
        let provider = observed.provider();
        let result = crate::operation::within(std::time::Duration::from_secs(u64::from(seconds) + 15), async {
            self.connections.forget_rejected(name, provider.wait_sample(seconds).await)
        })
        .await;
        observed.finish(result).await
    }

    /// Sessions, blocking and waits, plus what Live may do to other sessions here (nothing on production).
    pub(crate) async fn live(&self, name: &str) -> Result<LiveState> {
        let info = self.connections.connection(name)?;
        let observed = self.connections.observed_provider(name).await?;
        let provider = observed.provider();
        let result = crate::operation::within(culprit_core::limits::OBSERVE_TIMEOUT, async {
            let mut live = self.connections.forget_rejected(name, provider.live().await)?;
            // Lock details are read only when someone waits, so a quiet server costs one query per refresh.
            if live.blocking.is_none() && live.sessions.iter().any(|s| s.blocked_by.is_some()) {
                let locks = match self.connections.forget_rejected(name, provider.lock_waits().await) {
                    Ok(locks) => locks,
                    Err(error) if crate::source_operation::native_shutdown_uncertain(&error) => return Err(error),
                    Err(error) => {
                        live.notes.push(t!("engine.execution.no_lock_details", why = &error.message));
                        Vec::new()
                    }
                };
                live.blocking = culprit_core::blocking::analyze(info.engine, &live.sessions, &locks);
            }
            if info.environment.is_production() {
                live.session_actions = Vec::new();
                live.session_actions_note = Some(t!("engine.execution.production_sessions"));
            } else {
                live.session_actions = provider.session_actions();
            }
            Ok(live)
        })
        .await;
        observed.finish(result).await
    }

    /// Recent deadlocks on `name` (newest first, explained), with fixes linked to the latest report's findings.
    pub(crate) async fn deadlocks(&self, name: &str, since_hours: f64) -> Result<DeadlockReport> {
        let observed = self.connections.observed_provider(name).await?;
        let provider = observed.provider();
        let result = crate::operation::within(culprit_core::limits::OBSERVE_TIMEOUT, async {
            let mut report = self
                .connections
                .forget_rejected(name, provider.deadlocks(since_hours.clamp(1.0, 24.0 * 30.0)).await)?;
            let report_name = name.to_owned();
            let latest =
                self.reports.background(move |reports| reports.latest_report(&report_name, None)).await.ok().flatten();
            deadlocks::link_findings(&mut report, latest.as_ref());
            Ok(report)
        })
        .await;
        observed.finish(result).await
    }

    /// Cancel another session's statement or end the session. Refused on production connections, whatever the caller says.
    pub(crate) async fn interrupt_session(&self, name: &str, session: &str, action: SessionAction) -> Result<()> {
        let info = self.connections.connection(name)?;
        if info.environment.is_production() {
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                t!("engine.execution.interrupt_production", name = &info.name),
            )
            .with_hint(t!("engine.execution.interrupt_production_hint")));
        }
        info.engine.spec().validate_session_id(session)?;
        let provider = self.connections.provider(name).await?;
        crate::operation::within(culprit_core::limits::OBSERVE_TIMEOUT, async {
            if !provider.session_actions().contains(&action) {
                return Err(Error::new(
                    ErrorCode::Unsupported,
                    match action {
                        SessionAction::Cancel => t!("engine.execution.no_cancel", engine = info.engine.to_string()),
                        SessionAction::Terminate => {
                            t!("engine.execution.no_terminate", engine = info.engine.to_string())
                        }
                    },
                )
                .with_hint(match action {
                    SessionAction::Cancel => t!("engine.execution.no_cancel_hint"),
                    SessionAction::Terminate => t!("engine.execution.no_terminate_hint"),
                }));
            }
            self.connections.forget_rejected(name, provider.interrupt_session(session, action).await)
        })
        .await
    }

    /// `confirmed` is the caller's explicit, per-action consent to execute on production.
    /// Run a statement. Reads (as Culprit's classifier proves them) run in a read-only transaction and never ask.
    /// Anything else commits; on production only when the person confirmed it, otherwise this returns
    /// `ConfirmationRequired` with what the statement does, for the question to ask.
    pub(crate) async fn run(&self, name: &str, sql: &str, limit: usize, confirmed: bool) -> Result<ResultSet> {
        let limit = limit.min(culprit_core::limits::MAX_RESULT_ROWS);
        let info = self.connections.connection(name)?;
        let check = culprit_core::sql::check(sql, info.engine);
        // Text that isn't exactly one statement never runs, confirmed or not: on SQL Server, statements need no
        // semicolon between them, so `SELECT 1 COMMIT CREATE TABLE …` would otherwise run all three.
        if let Some(problem) = check.problem {
            let hint = if check.problem_kind.is_some_and(culprit_core::sql::StatementProblem::is_unclosed) {
                t!("engine.execution.unclosed_hint")
            } else {
                t!("engine.execution.one_at_a_time_hint")
            };
            let mut error = Error::invalid(problem).with_hint(hint);
            if let Some(kind) = check.problem_kind {
                error = error.with_detail(culprit_core::ErrorDetail::Statement(kind));
            }
            return Err(error);
        }
        if check.reads_only {
            return self.run_read(name, sql, limit).await;
        }
        // An engine Culprit only reads (a SQLite file, opened read-only) never changes: say so before asking anyone to
        // confirm.
        if !info.engine.abilities().writes {
            let provider = self.connections.provider(name).await?;
            return self.connections.forget_rejected(name, crate::operation::run(provider.execute(sql, limit)).await);
        }
        if info.environment.is_production() && !confirmed {
            // A read-shaped statement Culprit can't prove reads (a locking read, a function call) may write; a verb
            // that writes does.
            let verb = check.verb.as_str();
            let reads_like = matches!(verb, "SELECT" | "WITH" | "TABLE" | "VALUES" | "SHOW" | "EXPLAIN");
            let message = match (verb.is_empty(), reads_like) {
                (true, _) => t!("engine.execution.confirm.statement", name = &info.name),
                (false, true) => t!("engine.execution.confirm.may_change", verb = verb, name = &info.name),
                (false, false) => t!("engine.execution.confirm.changes", verb = verb, name = &info.name),
            };
            return Err(Error::new(ErrorCode::ConfirmationRequired, message)
                .with_hint(check.warning.unwrap_or_else(|| t!("engine.execution.confirm_hint"))));
        }
        let provider = self.connections.provider(name).await?;
        self.connections.forget_rejected(name, crate::operation::run(provider.execute(sql, limit)).await)
    }

    /// Run a statement that must only read (agents, previews, the editor's reads). Refused unless Culprit's classifier
    /// proves it reads; Postgres and MySQL run it in a read-only transaction, and SQL Server, which has none, inside a
    /// transaction that is always rolled back.
    pub(crate) async fn run_read(&self, name: &str, sql: &str, limit: usize) -> Result<ResultSet> {
        self.read(name, sql, limit, false).await
    }

    async fn read(&self, name: &str, sql: &str, limit: usize, preview: bool) -> Result<ResultSet> {
        let limit = limit.min(culprit_core::limits::MAX_RESULT_ROWS);
        let info = self.connections.connection(name)?;
        let statement = culprit_core::sql::single_statement(sql, info.engine)?;
        if !culprit_core::sql::reads_only(statement, info.engine) {
            return Err(Error::new(ErrorCode::PermissionDenied, t!("engine.execution.reads_only"))
                .with_hint(t!("engine.execution.reads_only_hint")));
        }
        let provider = self.connections.provider(name).await?;
        crate::operation::within(
            if preview { culprit_core::limits::OBSERVE_TIMEOUT } else { culprit_core::limits::QUERY_TIMEOUT },
            async {
                // The server's own parser has the last word where it can give one (SQL Server compiles the text under SHOWPLAN):
                // exactly one statement, and a read.
                let server_checked =
                    match self.connections.forget_rejected(name, provider.statement_types(statement).await)? {
                        Some(types) if !(types.len() == 1 && is_read_type(&types[0])) => {
                            return Err(Error::new(ErrorCode::PermissionDenied, t!("engine.execution.reads_only"))
                                .with_hint(reads_as(&types)));
                        }
                        Some(_) => true,
                        None => false,
                    };
                let batch = read_only_batch(info.engine, statement);
                let read =
                    if preview { provider.preview(&batch, limit).await } else { provider.run(&batch, limit).await };
                let mut result = self.connections.forget_rejected(name, read)?;
                // Say so when the second opinion couldn't be asked for: the person (or the agent's approver) should know.
                if !server_checked && info.engine == Engine::Mssql {
                    result.messages.push(t!("engine.execution.no_showplan"));
                }
                Ok(result)
            },
        )
        .await
    }

    /// Databases, tables, views and columns for editor completion, read live through the read-only run path.
    pub(crate) async fn catalog(&self, name: &str) -> Result<catalog::Catalog> {
        let info = self.connections.connection(name)?;
        let observed = self.connections.observed_provider(name).await?;
        let provider = observed.provider();
        let result = crate::operation::within(culprit_core::limits::OBSERVE_TIMEOUT, async {
            let columns = self.connections.forget_rejected(name, catalog::columns(provider, info.engine).await)?;
            let databases = match self
                .connections
                .forget_rejected(name, provider.run(catalog::databases_sql(info.engine), 500).await)
            {
                Ok(databases) => Some(databases),
                Err(error) if crate::source_operation::native_shutdown_uncertain(&error) => return Err(error),
                Err(_) => None,
            };
            Ok(catalog::fold(columns, databases))
        })
        .await;
        observed.finish(result).await
    }
}

/// The statement types SQL Server's showplan reports for a read. An allow-list: anything else (`SELECT INTO`, writes,
/// types a later version adds) is refused and named in the error, so a new kind of read fails safe.
const READ_TYPES: [&str; 4] = ["SELECT", "SELECT WITHOUT QUERY", "XML SELECT", "JSON SELECT"];

fn is_read_type(t: &str) -> bool {
    READ_TYPES.iter().any(|r| r.eq_ignore_ascii_case(t.trim()))
}

/// PostgreSQL and MySQL providers already run reads inside a read-only transaction. SQL Server has no read-only
/// transactions, so a read runs inside one that is always rolled back.
pub(crate) fn read_only_batch(engine: Engine, statement: &str) -> String {
    match engine {
        Engine::Mssql => {
            format!("SET XACT_ABORT ON; BEGIN TRANSACTION;\n{statement}\n;IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION;")
        }
        // SQLite's file is opened read-only: nothing to wrap.
        Engine::Postgres | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite => statement.to_string(),
    }
}

/// "The server reads this text as: select, insert.": the statement types the server's own parser reported.
fn reads_as(types: &[String]) -> String {
    t!("engine.execution.reads_as", types = types.join(", ").to_lowercase())
}

/// Explain Analyze executes the statement. On production it only does that for a statement Culprit's classifier proves
/// reads (SQL Server has no read-only transactions, so the classifier is the whole guard there); a write is never
/// executed just to measure it.
fn guard_execution(info: &ConnectionInfo, sql: &str, executes: bool, _confirmed: bool) -> Result<()> {
    if !executes || !info.environment.is_production() || culprit_core::sql::reads_only(sql, info.engine) {
        return Ok(());
    }
    Err(Error::new(ErrorCode::PermissionDenied, t!("engine.execution.analyze_production", name = &info.name))
        .with_hint(t!("engine.execution.analyze_production_hint")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn one_preview_per_workspace_is_enforced_before_opening_another_connection() {
        let home = std::env::temp_dir().join(format!("culprit-preview-gate-{}", std::process::id()));
        let ws = crate::Workspace::open_in(&home);
        let options = culprit_core::TablePreviewOptions {
            schema: "public".into(),
            table: "orders".into(),
            filter: None,
            order: None,
            limit: 100,
        };
        let permit = ws.previews.try_acquire().unwrap();
        let error = ws.preview_table("missing", &options).await.unwrap_err();
        assert_eq!(error.message, "A table preview is already running");
        drop(permit);
        assert_eq!(ws.preview_table("missing", &options).await.unwrap_err().code, ErrorCode::NotFound);
        let _ = std::fs::remove_dir_all(home);
    }
    use culprit_core::{AuthKind, Environment};

    fn info(engine: Engine, environment: Environment) -> ConnectionInfo {
        ConnectionInfo {
            security: None,
            name: "shop".into(),
            engine,
            environment,
            host: "db.example.com".into(),
            port: None,
            database: None,
            user: None,
            auth: AuthKind::Password,
            has_secret: true,
            display: String::new(),
            last_report: None,
            agent_access: Default::default(),
            query_log: None,
            ssh: None,
        }
    }

    #[test]
    fn reads_are_the_select_types_showplan_reports() {
        for t in ["SELECT", "select", "SELECT WITHOUT QUERY", "XML SELECT", "JSON SELECT"] {
            assert!(is_read_type(t), "{t}");
        }
        for t in ["SELECT INTO", "INSERT", "UPDATE", "DELETE", "MERGE", "EXECUTE PROC", "COND", "SELECT INTO XML", ""] {
            assert!(!is_read_type(t), "{t}");
        }
    }

    #[test]
    fn production_analyze_only_measures_reads() {
        let prod = info(Engine::Mssql, Environment::Production);
        assert!(guard_execution(&prod, "SELECT * FROM t", true, true).is_ok());
        assert!(guard_execution(&prod, "SELECT * FROM t", true, false).is_ok());
        assert!(guard_execution(&prod, "DELETE FROM t", true, true).is_err());
        assert!(guard_execution(&prod, "SELECT 1; DELETE FROM t", true, true).is_err());
        assert!(guard_execution(&prod, "SELECT * INTO copy FROM t", true, true).is_err());
        // Estimated plans don't execute, so they are never gated.
        assert!(guard_execution(&prod, "DELETE FROM t", false, false).is_ok());
        let dev = info(Engine::Postgres, Environment::Development);
        assert!(guard_execution(&dev, "DELETE FROM t", true, false).is_ok());
    }
}
