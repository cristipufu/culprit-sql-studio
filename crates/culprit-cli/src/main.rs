mod activity_cmd;
mod admission_cmd;
mod agents;
mod collect_cmd;
mod collection_shutdown;
mod commands;
mod conn_cmd;
mod dashboard_cmd;
mod fleet_cmd;
mod lint_cmd;
mod locks;
mod outbox_cmd;
mod output;
mod plan_cmd;
mod service_cmd;
mod share_cmd;
mod ssh;
mod timeline_cmd;
mod update_cmd;
mod waits_cmd;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand, ValueEnum};
use culprit_auth::SignInEvent;
use culprit_core::{Error, Report, Severity, t};
use culprit_engine::Workspace;
use output::{Format, Out};

#[derive(Parser)]
#[command(
    name = "culprit",
    version,
    about = "Find what is slowing your database down.",
    long_about = "Find what is slowing your database down.\n\nCulprit reads statistics views only (safe on production), ranks what it finds by share of database time, and gives each finding a stable id, a fix and a way to verify it. Output is JSON when stdout is not a terminal.",
    after_help = "Examples:\n  culprit conn add shop-staging --url 'Server=tcp:shop-staging.database.windows.net;Database=shop;Authentication=Active Directory Default' --env staging\n  culprit scan shop-staging\n  culprit show shop-staging 'missing-index:dbo.Orders(CustomerId)'\n  culprit top shop-staging --by cpu -n 10 --format json\n  culprit explain shop-staging --sql 'SELECT * FROM dbo.Orders WHERE CustomerId = 1'\n  claude mcp add culprit -- culprit mcp     # serve Culprit to coding agents"
)]
struct Cli {
    /// Output format. Defaults to text in a terminal, JSON otherwise.
    #[arg(long, global = true, value_enum)]
    format: Option<Format>,
    /// Shorthand for --format json.
    #[arg(long, global = true)]
    json: bool,
    /// The language to write findings and notes in (`es`, `fr`, `de`; English by default). JSON output, read by
    /// scripts and coding agents, stays in English.
    #[arg(long, global = true, env = "CULPRIT_LANG", value_name = "CODE")]
    lang: Option<String>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct ChangesArgs {
    connection: String,
    /// Compare everything since this moment (RFC 3339, e.g. a deploy) with the same length of time before it.
    #[arg(long)]
    since: Option<String>,
    /// The recent period as the last N hours: `6h`, `90m` or `2d` (default: the last hour, or since the previous scan).
    #[arg(long, value_parser = parse_hours)]
    recent: Option<f64>,
    /// Runs a query needs in each period before its averages are compared (default 10).
    #[arg(long)]
    min_runs: Option<f64>,
    /// How much slower or faster counts as a change (default 1.5).
    #[arg(long)]
    threshold: Option<f64>,
    /// Skip cloud metrics (CloudWatch, Azure Monitor) when looking for what changed around the periods.
    #[arg(long)]
    no_cloud: bool,
}

impl ChangesArgs {
    fn request(&self) -> culprit_engine::changes::ChangeRequest {
        culprit_engine::changes::ChangeRequest {
            since: self.since.clone(),
            recent_hours: self.recent,
            min_executions: self.min_runs,
            threshold: self.threshold,
            cloud: self.no_cloud.then_some(false),
            ..Default::default()
        }
    }
}

/// `--window`: `6h`, `90m`, `2d` or `1d12h`, in minutes.
pub(crate) fn parse_window_arg(text: &str) -> Result<u32, String> {
    culprit_core::parse_window(text)
}

/// The window a command reads, in minutes: `--window`, else `--window-days` in whole days, else none.
pub(crate) fn window_minutes(window: Option<u32>, window_days: Option<u32>) -> Option<u32> {
    window.or_else(|| window_days.map(|days| days.max(1).saturating_mul(culprit_core::MINUTES_PER_DAY)))
}

/// `6h`, `90m`, `2d`, or a bare number of hours.
pub(crate) fn parse_hours(text: &str) -> Result<f64, String> {
    let text = text.trim();
    let wrong = || format!("'{text}' isn't a duration like 6h, 90m or 2d");
    let (number, unit) = match text.chars().last() {
        Some(c) if c.is_ascii_alphabetic() => (&text[..text.len() - 1], c.to_ascii_lowercase()),
        _ => (text, 'h'),
    };
    let value: f64 = number.trim().parse().map_err(|_| wrong())?;
    let hours = match unit {
        'm' => value / 60.0,
        'h' => value,
        'd' => value * 24.0,
        _ => return Err(wrong()),
    };
    if hours > 0.0 && hours.is_finite() { Ok(hours) } else { Err("the recent period must be longer than zero".into()) }
}

#[derive(Subcommand)]
enum Command {
    /// Manage saved connections. Secrets go to the OS keychain; commands refer to connections by name.
    #[command(subcommand)]
    Conn(conn_cmd::ConnCommand),
    /// Microsoft Entra ID sign-ins made with a device code (used when the Azure CLI is missing or signed out).
    #[command(subcommand)]
    Auth(AuthCommand),
    /// One database at a glance: database time and its trend, findings, top queries, waits, health, sessions right
    /// now, deadlocks, indexes, plans and query statistics, each as one plain sentence with the numbers behind it and
    /// the command to run next. Reads the latest scan, and sessions once unless --no-live. Takes a connection or a
    /// saved report file.
    Dashboard(dashboard_cmd::DashboardArgs),
    /// Show the server, what this login can see, and which checks are locked and why.
    Doctor { connection: String },
    /// Take a snapshot, run every check, save the report and print the findings.
    Scan(ScanArgs),
    #[command(subcommand, about = culprit_core::t!("collection.cli.about"))]
    Collect(collect_cmd::CollectCommand),
    /// Findings from the latest saved report (no database access).
    Findings(FindingsArgs),
    /// One finding in full, with the agent brief.
    Show {
        connection: String,
        finding: String,
        #[arg(long)]
        report: Option<PathBuf>,
    },
    /// The heaviest queries from the latest saved report.
    Top(TopArgs),
    /// Show a statement's plan. Estimated plans never execute the statement.
    Explain(ExplainArgs),
    /// Read plans saved by other tools (.sqlplan, EXPLAIN JSON or text) and compare any two plans.
    #[command(subcommand)]
    Plan(plan_cmd::PlanCommand),
    /// Did the fix work? Re-scan and report each finding as fixed, still present or can't tell, plus new findings.
    /// Exits 2 when a finding you named is still present.
    Verify(share_cmd::VerifyArgs),
    /// A scan in a few facts, as JSON for a scheduled job to format and post: what the database looks like, what changed
    /// since the previous digest (new, fixed and worse findings, and lock escalations that happened again), the findings
    /// worth a look, the queries that used the most database time, the index advisor's plan and the deadlocks. Compares
    /// the latest two scans of a connection, or the report files given (no database access).
    Digest(share_cmd::DigestArgs),
    /// A finding (--finding <id>) or the whole latest scan as Markdown for a pull request, or a self-contained HTML page
    /// (--html). Literal values are redacted unless --no-redact; connection names and hosts are never included.
    Export(share_cmd::ExportArgs),
    /// Current sessions, blocking and waits. On non-production connections, --cancel or --kill acts on one session.
    Live(LiveArgs),
    /// Who blocks whom right now: chains under each head blocker, how long, on what, and what to do.
    Blocking { connection: String },
    /// Recent deadlocks, explained: the pattern, both statements and the fix. --file explains a saved one.
    Deadlocks(locks::DeadlocksArgs),
    /// Where database time goes: waits by category with the queries behind each, and pressure signals. --now samples
    /// what the server waits on right now.
    Waits(waits_cmd::WaitsArgs),
    /// Run a statement and print up to --limit rows. Reads run in a read-only transaction; anything that changes data
    /// commits, and on production needs --yes-execute-on-production.
    Run(RunArgs),
    /// Named queries you can rerun: list, show, add, rm, run.
    #[command(subcommand)]
    Saved(SavedCommand),
    /// List every check with the engines and capabilities it needs.
    Checks,
    /// List what a scan reads on each engine: its sources, and the capabilities each delivers to the checks.
    Sources,
    /// Databases, tables, views and columns as the editor's completion sees them (read live, read-only).
    Catalog { connection: String },
    /// What differs between two databases (latest scans): tables, columns, indexes, settings and version, with SQL to align
    /// missing indexes. Each side is a connection name or a saved report file.
    Drift { a: String, b: String },
    /// What changed between two periods: queries that got slower, new ones, ones that got faster or stopped, and the
    /// plan changes behind them. Query Store answers from its intervals; PostgreSQL and MySQL from Culprit's earlier scans.
    Changes(ChangesArgs),
    /// Put a moment on a connection's timeline: `culprit mark shop deploy v1.4.2` (from CI, after a migration). Charts
    /// and `changes` line regressions up with it. --list shows them, --remove <id> takes one off.
    Mark(timeline_cmd::MarkArgs),
    /// What changed around a connection: marks, tables, columns, indexes, procedures, settings, upgrades, restarts and,
    /// for Amazon RDS and Azure databases, throttling. Exact where the database dates it, otherwise between two scans.
    Events(timeline_cmd::PeriodArgs),
    /// What the cloud measured for the server (CloudWatch through `aws`, Azure Monitor through `az`; read-only, with
    /// your own sign-in): CPU, credits, IOPS against limits, latency, and when it held the server back.
    Cloud(timeline_cmd::PeriodArgs),
    /// The index advisor: indexes to add (ordered by the database time of the queries that need them), indexes to
    /// clean up, and tables whose shape costs reads or writes. Takes a connection or a saved report file.
    Indexes { connection: String },
    /// Re-run every check on a saved snapshot or report file, offline. Prints the full report.
    Analyze {
        file: PathBuf,
        /// Name to record as the connection (defaults to the one in the file).
        #[arg(long)]
        connection: Option<String>,
    },
    /// Lint migration or DDL SQL offline against a connection's latest saved snapshot (or a saved report/snapshot file).
    #[command(visible_alias = "check-sql")]
    LintSql(lint_cmd::LintSqlArgs),
    /// Serve Culprit to coding agents over MCP (stdio). Findings, statistics, schema and estimated plans; statements
    /// only on connections with `--agent query` and after approval in the desktop app. Every call is audited.
    Mcp(agents::McpArgs),
    /// What coding agents did through `culprit mcp`, most recent last.
    Audit(agents::AuditArgs),
    /// Set up a repository for coding agents.
    #[command(subcommand)]
    Agent(agents::AgentCommand),
    /// Replace this binary with the latest published release, verified against the release's SHA256SUMS.txt.
    /// --check only says whether a newer release exists. Culprit never checks for updates on its own: no other command
    /// goes online for them.
    Update(update_cmd::UpdateArgs),
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Forget device-code sign-ins: stored refresh tokens and cached access tokens. `az logout` signs the Azure CLI out.
    Logout,
}

#[derive(Args)]
struct ScanArgs {
    connection: String,
    /// Days of Query Store history to read.
    #[arg(long, default_value_t = 7)]
    window_days: u32,
    /// How far back to read history, in days, hours or minutes: 6h, 90m, 2d, 1d12h. Instead of --window-days. Query
    /// Store keeps history in intervals (often an hour), so the window starts at the interval its edge falls in.
    #[arg(long, value_parser = parse_window_arg, conflicts_with = "window_days", value_name = "DURATION")]
    window: Option<u32>,
    /// How many of the heaviest queries to keep.
    #[arg(long, default_value_t = 60)]
    top: u32,
    /// How many of the heaviest queries to collect plans for.
    #[arg(long, default_value_t = 25)]
    plans: u32,
    /// Exit with code 3 if any finding is at least this severe (for CI).
    #[arg(long, value_enum)]
    fail_on: Option<SeverityArg>,
    /// Print the whole report, including the snapshot.
    #[arg(long)]
    full: bool,
    /// For a database that keeps no statement history (SQLite): a log the application's framework already writes, read
    /// for the statements and their times, instead of the connection's saved one (`culprit conn set --query-log`).
    /// Rails, Django, Entity Framework Core, Phoenix/Ecto, or JSON lines.
    #[arg(long, value_name = "FILE")]
    query_log: Option<std::path::PathBuf>,
}

#[derive(Args)]
struct FindingsArgs {
    connection: String,
    /// Read a saved report file instead of the latest one.
    #[arg(long)]
    report: Option<PathBuf>,
    #[arg(long, value_enum)]
    min_severity: Option<SeverityArg>,
}

#[derive(Args)]
struct TopArgs {
    connection: String,
    #[arg(long, value_enum, default_value = "time")]
    by: TopBy,
    #[arg(short = 'n', long, default_value_t = 15)]
    limit: usize,
    #[arg(long)]
    report: Option<PathBuf>,
}

#[derive(Args)]
struct ExplainArgs {
    connection: String,
    #[arg(long, conflicts_with_all = ["file", "query_id"])]
    sql: Option<String>,
    #[arg(long, conflicts_with = "query_id")]
    file: Option<PathBuf>,
    /// Explain a query from the latest report by its id (its stored plan when the engine keeps one). PostgreSQL ids can
    /// be negative, e.g. --query-id -1973449073115120133.
    #[arg(long, allow_hyphen_values = true)]
    query_id: Option<String>,
    /// With --query-id: a specific stored plan, e.g. one of the plans Query Store kept for the query.
    #[arg(long, requires = "query_id", allow_hyphen_values = true)]
    plan_id: Option<String>,
    /// Execute the statement and report actual rows and timings.
    #[arg(long, conflicts_with = "last_actual")]
    analyze: bool,
    /// With --query-id: the actual plan of the query's last run as SQL Server kept it (2019+, LAST_QUERY_PLAN_STATS).
    /// Never runs the query.
    #[arg(long, requires = "query_id")]
    last_actual: bool,
    /// Test virtual indexes in planner memory without building them (PostgreSQL or YugabyteDB `hypopg`).
    #[arg(long = "with-index", value_name = "CREATE_INDEX_SQL", conflicts_with_all = ["analyze", "last_actual"])]
    with_index: Vec<String>,
    #[arg(long)]
    yes_execute_on_production: bool,
}

#[derive(Subcommand)]
enum SavedCommand {
    /// Saved queries, most recently run first.
    List,
    /// Print one saved query's SQL.
    Show { name: String },
    /// Save (or overwrite) a query under a name.
    Add {
        name: String,
        #[arg(long, conflicts_with = "file")]
        sql: Option<String>,
        #[arg(long)]
        file: Option<PathBuf>,
        /// The connection it runs on by default.
        #[arg(long)]
        connection: Option<String>,
        #[arg(long)]
        description: Option<String>,
    },
    /// Forget a saved query.
    Rm { name: String },
    /// Run a saved query (on --connection, or the one it was saved for).
    Run {
        name: String,
        #[arg(long)]
        connection: Option<String>,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        yes_execute_on_production: bool,
    },
}

#[derive(Args)]
struct LiveArgs {
    connection: String,
    /// Stop the statement this session is running; the session stays connected (PostgreSQL, MySQL).
    #[arg(long, value_name = "SESSION", conflicts_with = "kill")]
    cancel: Option<String>,
    /// End this session; its open transaction rolls back.
    #[arg(long, value_name = "SESSION")]
    kill: Option<String>,
    /// How far along this session's running statement is: per-operator progress on SQL Server, the plan with elapsed
    /// time and command progress on PostgreSQL and MySQL.
    #[arg(long, value_name = "SESSION", conflicts_with_all = ["cancel", "kill"])]
    progress: Option<String>,
    /// With --progress: keep refreshing every second until the statement finishes.
    #[arg(long, requires = "progress")]
    watch: bool,
}

#[derive(Args)]
struct RunArgs {
    connection: String,
    #[arg(long)]
    sql: String,
    #[arg(long, default_value_t = 100)]
    limit: usize,
    #[arg(long)]
    yes_execute_on_production: bool,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum SeverityArg {
    Low,
    Medium,
    #[value(alias = "critical")]
    High,
}

#[derive(Clone, Copy, ValueEnum)]
pub enum TopBy {
    Time,
    Cpu,
    Reads,
    Executions,
}

impl From<SeverityArg> for Severity {
    fn from(s: SeverityArg) -> Severity {
        match s {
            SeverityArg::Low => Severity::Low,
            SeverityArg::Medium => Severity::Medium,
            SeverityArg::High => Severity::High,
        }
    }
}

const EXIT_FINDINGS: u8 = 3;

fn writes_markdown(command: &Command) -> bool {
    matches!(
        command,
        Command::Dashboard(_)
            | Command::Scan(_)
            | Command::Findings(_)
            | Command::Show { .. }
            | Command::Analyze { .. }
            | Command::Verify(_)
            | Command::Export(_)
            | Command::Plan(plan_cmd::PlanCommand::Export { .. })
            | Command::LintSql(_)
    )
}

fn writes_sarif(command: &Command) -> bool {
    matches!(
        command,
        Command::Scan(_) | Command::Findings(_) | Command::Analyze { .. } | Command::LintSql(_) | Command::Drift { .. }
    )
}

fn main() -> ExitCode {
    update_cmd::remove_stale_old();
    let cli = Cli::parse();
    let format = match (cli.json, cli.format) {
        (true, _) => Format::Json,
        (false, Some(f)) => f,
        (false, None) if std::io::stdout().is_terminal() => Format::Text,
        _ => Format::Json,
    };
    let out = Out::new(format);
    if let Some(code) = cli.lang.as_deref().filter(|_| format != Format::Json) {
        // A language Culprit doesn't have: English, with a word on stderr (the output itself stays clean).
        if !culprit_core::i18n::set_language(code) {
            let known: Vec<String> = culprit_core::i18n::languages().iter().map(|l| l.code.clone()).collect();
            eprintln!("{}", t!("cli.no_language", code = format!("{code:?}"), known = known.join(", ")));
        }
    }
    // Markdown is written by the commands whose output is meant for a pull request or a ticket; elsewhere it would be
    // plain text under a Markdown label.
    if format == Format::Md && !writes_markdown(&cli.command) {
        out.error(&Error::invalid(t!("cli.no_markdown")).with_hint(t!("cli.no_markdown_hint")));
        return ExitCode::FAILURE;
    }
    if format == Format::Sarif && !writes_sarif(&cli.command) {
        out.error(&Error::invalid(t!("cli.no_sarif")).with_hint(t!("cli.no_sarif_hint")));
        return ExitCode::FAILURE;
    }
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            out.error(&Error::internal(e.to_string()));
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli.command, &out)) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            out.error(&e);
            ExitCode::FAILURE
        }
    }
}

async fn run(command: Command, out: &Out) -> culprit_core::Result<u8> {
    // Needs no workspace, so an update works even when the configuration can't be read.
    if let Command::Update(args) = command {
        return update_cmd::run(args, out).await;
    }
    if let Command::Collect(command) = command {
        return collect_cmd::run(command, out).await;
    }
    let mut ws = Workspace::open()?;
    // A device-code sign-in needs a person to read the code; scripts and agents get an error with a hint instead.
    if std::io::stderr().is_terminal() && !matches!(&command, Command::Collect(_)) {
        ws = ws.with_sign_in_handler(std::sync::Arc::new(|event| match event {
            SignInEvent::Code { prompt } => {
                eprintln!("{}", t!("cli.sign_in.code", uri = &prompt.verification_uri, code = &prompt.user_code))
            }
            SignInEvent::Done => eprintln!("{}", t!("cli.sign_in.done")),
            SignInEvent::Failed { message } => eprintln!("{message}"),
        }));
    }
    dispatch(command, ws, out).await
}

type CommandTask<'a> = std::pin::Pin<Box<dyn std::future::Future<Output = culprit_core::Result<u8>> + 'a>>;

/// Construct only the selected operation, outside the caller's async frame and polling stack.
fn command_task<'a, F>(factory: impl FnOnce() -> F) -> CommandTask<'a>
where
    F: std::future::Future<Output = culprit_core::Result<u8>> + 'a,
{
    Box::pin(factory())
}

/// Poll only the selected command's frame; unrelated commands must not consume its caller's stack.
fn dispatch<'a>(command: Command, ws: Workspace, out: &'a Out) -> CommandTask<'a> {
    match command {
        Command::Conn(c) => Box::pin(async move { conn_cmd::run(&ws, c, out).await }),
        Command::Auth(AuthCommand::Logout) => Box::pin(async move {
            let removed = ws.sign_out()?;
            out.done(&t!("cli.sign_out", n = removed, count = removed.to_string()));
            Ok(0)
        }),
        Command::Dashboard(args) => Box::pin(async move { dashboard_cmd::run(&ws, args, out).await }),
        Command::Doctor { connection } => Box::pin(async move { commands::doctor(&ws, &connection, out).await }),
        Command::Scan(args) => Box::pin(async move { commands::scan(&ws, args, out).await }),
        Command::Collect(_) => Box::pin(async move { unreachable!("collection dispatch precedes workspace creation") }),
        Command::Findings(args) => Box::pin(async move { commands::findings(&ws, args, out) }),
        Command::Show { connection, finding, report } => {
            Box::pin(async move { commands::show(&ws, connection, finding, report, out) })
        }
        Command::Top(args) => Box::pin(async move { commands::top(&ws, args, out) }),
        Command::Explain(args) => Box::pin(async move { commands::explain(&ws, args, out).await }),
        Command::Plan(command) => Box::pin(async move { plan_cmd::run(&ws, command, out).await }),
        Command::Waits(args) => Box::pin(async move { waits_cmd::run(&ws, args, out).await }),
        Command::Verify(args) => Box::pin(async move { share_cmd::verify(&ws, args, out).await }),
        Command::Digest(args) => Box::pin(async move { share_cmd::digest(&ws, args, out) }),
        Command::Export(args) => Box::pin(async move { share_cmd::export(&ws, args) }),
        Command::Live(args) => Box::pin(async move { commands::live(&ws, args, out).await }),
        Command::Blocking { connection } => Box::pin(async move { locks::blocking(&ws, &connection, out).await }),
        Command::Deadlocks(args) => Box::pin(async move { locks::deadlocks(&ws, args, out).await }),
        Command::Run(args) => Box::pin(async move { commands::run_query(&ws, args, out).await }),
        Command::Saved(command) => Box::pin(async move { commands::saved(&ws, command, out).await }),
        Command::Catalog { connection } => Box::pin(async move { commands::catalog(&ws, &connection, out).await }),
        Command::Checks => Box::pin(async move {
            out.checks(&ws.checks());
            Ok(0)
        }),
        Command::Sources => Box::pin(async move {
            out.sources(&culprit_core::Engine::ALL.map(|e| (e, culprit_engine::registry::sources(e))));
            Ok(0)
        }),
        Command::Drift { a, b } => Box::pin(async move { commands::drift(&ws, a, b, out) }),
        Command::Changes(args) => Box::pin(async move { commands::changes(&ws, args, out).await }),
        Command::Mark(args) => Box::pin(async move { timeline_cmd::mark(&ws, args, out) }),
        Command::Events(args) => Box::pin(async move { timeline_cmd::events(&ws, args, out).await }),
        Command::Cloud(args) => Box::pin(async move { timeline_cmd::cloud(&ws, args, out).await }),
        Command::Indexes { connection } => Box::pin(async move { commands::indexes(&ws, connection, out) }),
        Command::Analyze { file, connection } => Box::pin(async move { commands::analyze(&ws, file, connection, out) }),
        Command::LintSql(args) => Box::pin(async move { lint_cmd::run(&ws, args, out) }),
        Command::Mcp(args) => Box::pin(async move { agents::mcp(ws, args).await }),
        Command::Audit(args) => Box::pin(async move { agents::audit(&ws, args, out) }),
        Command::Agent(command) => Box::pin(async move { agents::agent(&ws, command, out) }),
        Command::Update(_) => Box::pin(async move { unreachable!("an update is run before the workspace opens") }),
    }
}

pub(crate) fn fail_code(report: &Report, fail_on: Option<SeverityArg>) -> u8 {
    let Some(threshold) = fail_on.map(Severity::from) else { return 0 };
    if report.findings.iter().any(|f| f.severity >= threshold) { EXIT_FINDINGS } else { 0 }
}

pub(crate) fn load(ws: &Workspace, connection: &str, path: Option<&PathBuf>) -> culprit_core::Result<Report> {
    if let Some(path) = path {
        return ws.analyze_file(path);
    }
    if connection.to_ascii_lowercase().ends_with(".json") && ws.connection(connection).is_err() {
        return ws.analyze_file(std::path::Path::new(connection));
    }
    ws.latest_report(connection, None)?.ok_or_else(|| {
        Error::not_found(t!("engine.no_saved_report", name = connection))
            .with_hint(t!("engine.scan_first", name = connection))
    })
}

/// `arg` names a saved report file rather than a connection.
pub(crate) fn report_file(arg: &str) -> Option<PathBuf> {
    let path = PathBuf::from(arg);
    (arg.ends_with(".json") && path.is_file()).then_some(path)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn unrelated_commands_do_not_retain_background_collection_state_on_their_stack() {
        let out = Out::new(Format::Json);
        let request = run(Command::Checks, &out);
        let bytes = std::mem::size_of_val(&request);
        assert!(bytes <= 4096, "command entry retains {bytes} inline bytes; only the selected handler owns its state");
    }

    #[test]
    fn ssh_connection_flags_parse_and_cannot_silently_drop_an_incomplete_route() {
        let cli = Cli::try_parse_from([
            "culprit",
            "conn",
            "add",
            "shop",
            "--url",
            "postgres://app@db/shop",
            "--ssh",
            "reader@bastion",
            "--ssh-port",
            "2222",
            "--ssh-key",
            "key with spaces",
        ])
        .unwrap();
        let Command::Conn(conn_cmd::ConnCommand::Add(args)) = cli.command else { panic!("connection add") };
        let ssh = args.ssh.config().unwrap();
        assert_eq!(ssh.destination, "reader@bastion");
        assert_eq!(ssh.port, Some(2222));
        assert_eq!(ssh.identity_file.as_deref(), Some("key with spaces"));
        for args in [vec!["--ssh-key", "key"], vec!["--ssh-port", "2222"], vec!["--ssh", "bastion", "--ssh-port", "0"]]
        {
            let mut command = vec!["culprit", "conn", "add", "shop", "--url", "postgres://app@db/shop"];
            command.extend(args);
            assert!(Cli::try_parse_from(command).is_err());
        }
        assert!(Cli::try_parse_from(["culprit", "conn", "set", "shop", "--ssh", "bastion"]).is_ok());
        assert!(Cli::try_parse_from(["culprit", "conn", "set", "shop", "--no-ssh"]).is_ok());
        assert!(Cli::try_parse_from(["culprit", "conn", "set", "shop", "--ssh", "bastion", "--no-ssh"]).is_err());
    }

    #[test]
    fn saved_file_findings_apply_current_health_rules_without_rewriting_the_file() {
        let dir = std::env::temp_dir().join(format!("culprit-file-reading-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ws = Workspace::open_in(&dir);
        let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/pg-demo.report.json");
        let mut report = ws.analyze_file(&fixture).unwrap();
        let signal =
            report.snapshot.pressure.as_mut().unwrap().signals.iter_mut().find(|s| s.key == "cache-hit").unwrap();
        signal.status = culprit_core::SignalStatus::Bad;
        signal.healthy = Some("Legacy cache threshold".into());
        let path = dir.join("saved.json");
        let bytes = serde_json::to_vec(&report).unwrap();
        std::fs::write(&path, &bytes).unwrap();

        let reopened = load(&ws, "unused", Some(&path)).unwrap();
        let signal = reopened.snapshot.pressure.as_ref().unwrap().signal("cache-hit").unwrap();
        assert_eq!(signal.status, culprit_core::SignalStatus::Info);
        assert!(signal.healthy.as_deref().unwrap().contains("ratio alone"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn scan_args(args: &[&str]) -> Result<ScanArgs, clap::Error> {
        let cli = Cli::try_parse_from([&["culprit", "scan", "shop"], args].concat())?;
        match cli.command {
            Command::Scan(scan) => Ok(scan),
            _ => unreachable!("parsed as scan"),
        }
    }

    #[test]
    fn scan_takes_a_window_in_hours() {
        let scan = scan_args(&["--window", "6h"]).expect("6h parses");
        assert_eq!(scan.window, Some(360));
        assert_eq!(window_minutes(scan.window, Some(scan.window_days)), Some(360));
        // Without --window, --window-days (or its default of 7) is the window, as before.
        let scan = scan_args(&["--window-days", "2"]).expect("days parse");
        assert_eq!(window_minutes(scan.window, Some(scan.window_days)), Some(2 * culprit_core::MINUTES_PER_DAY));
        let scan = scan_args(&[]).expect("defaults parse");
        assert_eq!(window_minutes(scan.window, Some(scan.window_days)), Some(7 * culprit_core::MINUTES_PER_DAY));
    }

    #[test]
    fn scan_refuses_a_window_given_twice_or_badly() {
        let both = scan_args(&["--window", "6h", "--window-days", "2"]).err().expect("conflict");
        assert_eq!(both.kind(), clap::error::ErrorKind::ArgumentConflict);
        let garbage = scan_args(&["--window", "6"]).err().expect("no unit");
        assert!(garbage.to_string().contains("isn't a window"), "{garbage}");
    }

    #[test]
    fn dashboard_and_waits_take_a_window_too() {
        let cli = Cli::try_parse_from(["culprit", "dashboard", "shop", "--window", "90m"]).expect("dashboard");
        let Command::Dashboard(dashboard) = cli.command else { unreachable!("parsed as dashboard") };
        assert_eq!(window_minutes(dashboard.window, dashboard.window_days), Some(90));
        let cli = Cli::try_parse_from(["culprit", "waits", "shop", "--window", "1d12h"]).expect("waits");
        let Command::Waits(waits) = cli.command else { unreachable!("parsed as waits") };
        assert_eq!(window_minutes(waits.window, waits.window_days), Some(36 * 60));
        assert!(Cli::try_parse_from(["culprit", "waits", "shop", "--window", "6h", "--window-days", "1"]).is_err());
    }
}
