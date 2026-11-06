use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use culprit_core::{AuthKind, Engine, Environment, Error};
use culprit_engine::{ImportSource, NewConnection, Workspace};

use crate::output::Out;
use crate::{agents, ssh};

#[derive(Subcommand)]
pub(crate) enum ConnCommand {
    /// Save a connection. Paste a URL, ADO, JDBC or libpq string, or a host.
    Add(AddArgs),
    /// List saved connections and their last scan.
    List,
    /// Connect and identify the server.
    Test { name: String },
    /// Forget a connection and delete its keychain secret. Its saved reports stay on disk; a connection added later
    /// under the same name starts without them (they move to `reports/.removed/`).
    Rm { name: String },
    /// Rename a connection (its alias). Keychain secret and report history move with it.
    Rename { old: String, new: String },
    /// Find databases your cloud CLI can see (Azure SQL and PostgreSQL/MySQL flexible servers via `az login`, Amazon
    /// RDS/Aurora via `aws login`) and optionally save them.
    Discover(DiscoverArgs),
    /// Store the password for a password-authenticated connection in the OS keychain. Reads it from stdin (hidden when typed).
    SetPassword { name: String },
    /// Change a connection's settings, e.g. what coding agents may do with it (`--agent off|stats|query`).
    Set(agents::SetArgs),
    /// Bring in connections saved in Azure Data Studio, VS Code (mssql), DBeaver or DataGrip. Reads names, hosts, databases
    /// and user names only, never passwords; password connections ask once with `culprit conn set-password`.
    Import(ImportArgs),
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum ImportFrom {
    Auto,
    Ads,
    Vscode,
    Dbeaver,
    Datagrip,
}

#[derive(Args)]
pub(crate) struct ImportArgs {
    /// Which tool to read. `auto` reads every tool found on this machine.
    #[arg(long, value_enum, default_value = "auto")]
    from: ImportFrom,
    /// Read this file instead of the tool's usual location (repeatable): settings.json, data-sources.json, dataSources.xml.
    #[arg(long = "file", value_name = "PATH")]
    files: Vec<PathBuf>,
    /// Keep connections whose alias, host or database contains this word (repeatable; all must match).
    #[arg(long = "match", value_name = "WORD")]
    terms: Vec<String>,
    /// List what would be imported without saving anything.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum CloudArg {
    Azure,
    Aws,
}

#[derive(Args)]
pub(crate) struct DiscoverArgs {
    /// Which cloud to search.
    #[arg(long, value_enum, default_value = "azure")]
    cloud: CloudArg,
    /// AWS region to search (repeatable). Defaults to every enabled region.
    #[arg(long = "region", value_name = "REGION")]
    regions: Vec<String>,
    /// Keep databases whose database or server name contains this word. Repeat to narrow down (all must match). Omit to show the first 20 matches.
    #[arg(long = "match", value_name = "WORD")]
    terms: Vec<String>,
    /// Save unsaved matches under their suggested aliases; requires a complete, narrowed search.
    #[arg(long)]
    save: bool,
    /// Also save production databases (they are listed either way).
    #[arg(long)]
    include_production: bool,
}

#[derive(Args)]
pub(crate) struct AddArgs {
    pub(crate) name: String,
    #[command(flatten)]
    pub(crate) ssh: ssh::SshArgs,
    /// Connection string or host (postgres://…, mssql://…, Server=…;Database=…, jdbc:…, host=… dbname=…), or a SQLite
    /// file's path.
    #[arg(long)]
    pub(crate) url: String,
    #[arg(long, value_enum, default_value = "development")]
    pub(crate) env: EnvArg,
    #[arg(long, value_enum)]
    pub(crate) engine: Option<EngineArg>,
    #[arg(long, value_enum)]
    pub(crate) auth: Option<AuthArg>,
    #[arg(long)]
    pub(crate) database: Option<String>,
    #[arg(long)]
    pub(crate) user: Option<String>,
    /// Read the password from stdin instead of the connection string.
    #[arg(long, conflicts_with = "password_env")]
    pub(crate) password_stdin: bool,
    /// Read the password from this environment variable every time instead of storing it (CI, agents).
    #[arg(long)]
    pub(crate) password_env: Option<String>,
    /// Save without testing the connection first.
    #[arg(long)]
    pub(crate) no_test: bool,
    /// For a database that keeps no statement history (SQLite): the log the application's framework writes, read on
    /// every scan. Rails, Django, Entity Framework Core, Phoenix/Ecto, or JSON lines.
    #[arg(long, value_name = "FILE")]
    pub(crate) query_log: Option<PathBuf>,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum EnvArg {
    Production,
    Staging,
    Development,
    Local,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum EngineArg {
    Postgres,
    #[value(alias = "yugabytedb", alias = "ysql")]
    Yugabyte,
    Mssql,
    Mysql,
    Sqlite,
}

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum AuthArg {
    Password,
    Entra,
    AwsIam,
    None,
}

pub(crate) async fn run(ws: &Workspace, command: ConnCommand, out: &Out) -> culprit_core::Result<u8> {
    match command {
        ConnCommand::List => {
            out.connections(&ws.connections()?);
            Ok(0)
        }
        ConnCommand::Test { name } => {
            let server = ws.probe(&name).await?;
            out.server(&server);
            Ok(0)
        }
        ConnCommand::Rename { old, new } => {
            let info = ws.rename_connection(&old, &new)?;
            out.connection_renamed(&old, &info);
            Ok(0)
        }
        ConnCommand::Discover(args) => {
            let found = match args.cloud {
                CloudArg::Azure => {
                    if !args.regions.is_empty() {
                        return Err(Error::invalid("--region only applies to --cloud aws"));
                    }
                    out.progress("Searching Azure Resource Graph…");
                    ws.discover_azure(&args.terms).await?
                }
                CloudArg::Aws => {
                    let message = if args.regions.is_empty() {
                        "Searching Amazon RDS in every enabled region…".to_string()
                    } else {
                        format!("Searching Amazon RDS in {}…", args.regions.join(", "))
                    };
                    out.progress(&message);
                    ws.discover_aws(&args.regions, &args.terms).await?
                }
            };
            let mut saved = Vec::new();
            if args.save {
                if found.has_more || !found.warnings.is_empty() {
                    return Err(Error::invalid("Discovery is incomplete; no connections were saved")
                        .with_hint("Refine --match or --region until the search is complete, then retry --save"));
                }
                for d in found.items.iter().filter(|d| d.saved_as.is_none()) {
                    if d.environment.is_production() && !args.include_production {
                        continue;
                    }
                    saved.push(ws.add_discovered(d)?);
                }
            }
            out.discovered(&found, &saved);
            Ok(0)
        }
        ConnCommand::Import(args) => {
            let source = match args.from {
                ImportFrom::Auto => None,
                ImportFrom::Ads => Some(ImportSource::Ads),
                ImportFrom::Vscode => Some(ImportSource::Vscode),
                ImportFrom::Dbeaver => Some(ImportSource::Dbeaver),
                ImportFrom::Datagrip => Some(ImportSource::Datagrip),
            };
            let mut found = if args.files.is_empty() {
                ws.import_candidates(source)?
            } else {
                let mut all = Vec::new();
                for file in &args.files {
                    all.extend(ws.import_file(file, source)?);
                }
                all
            };
            let terms: Vec<String> = args.terms.iter().map(|t| t.to_ascii_lowercase()).collect();
            found.retain(|c| {
                let text =
                    format!("{} {} {}", c.alias, c.host, c.database.as_deref().unwrap_or("")).to_ascii_lowercase();
                terms.iter().all(|t| text.contains(t.as_str()))
            });
            let new: Vec<_> = found.iter().filter(|c| c.saved_as.is_none()).cloned().collect();
            let saved = if args.dry_run || new.is_empty() { Vec::new() } else { ws.import_connections(&new)? };
            out.imported(&found, &saved, args.dry_run);
            Ok(0)
        }
        ConnCommand::SetPassword { name } => {
            let password = read_password(&format!("Password for {name}: "))?;
            let info = ws.set_password(&name, &password)?;
            out.done(&format!("Stored the password for {} in the OS keychain", info.name));
            Ok(0)
        }
        ConnCommand::Set(args) => agents::set(ws, args, out),
        ConnCommand::Rm { name } => {
            ws.remove_connection(&name)?;
            out.done(&format!("Removed {name}"));
            Ok(0)
        }
        ConnCommand::Add(args) => {
            let password = if args.password_stdin {
                let mut buf = String::new();
                std::io::stdin().read_to_string(&mut buf).map_err(|e| Error::invalid(e.to_string()))?;
                Some(buf.trim_end_matches(['\r', '\n']).to_string())
            } else {
                None
            };
            let new = NewConnection {
                name: args.name.clone(),
                input: args.url,
                environment: match args.env {
                    EnvArg::Production => Environment::Production,
                    EnvArg::Staging => Environment::Staging,
                    EnvArg::Development => Environment::Development,
                    EnvArg::Local => Environment::Local,
                },
                engine: args.engine.map(|e| match e {
                    EngineArg::Postgres => Engine::Postgres,
                    EngineArg::Yugabyte => Engine::Yugabyte,
                    EngineArg::Mssql => Engine::Mssql,
                    EngineArg::Mysql => Engine::Mysql,
                    EngineArg::Sqlite => Engine::Sqlite,
                }),
                ssh: args.ssh.config(),
                database: args.database,
                user: args.user,
                password,
                password_env: args.password_env,
                auth: args.auth.map(|a| match a {
                    AuthArg::Password => AuthKind::Password,
                    AuthArg::Entra => AuthKind::Entra,
                    AuthArg::AwsIam => AuthKind::AwsIam,
                    AuthArg::None => AuthKind::None,
                }),
            };
            let server = if args.no_test {
                None
            } else {
                out.progress(&format!("Testing {}…", args.name));
                Some(ws.test_connection(&new).await?)
            };
            let mut info = ws.add_connection(new)?;
            if let Some(log) = args.query_log.as_deref() {
                info = ws.set_query_log(&info.name, Some(log))?;
            }
            out.connection_added(&info, server.as_ref());
            Ok(0)
        }
    }
}

/// One line from stdin. When stdin is a terminal the typed characters are hidden and a prompt goes to stderr.
fn read_password(prompt: &str) -> culprit_core::Result<String> {
    use std::io::{BufRead, Write};
    let interactive = std::io::stdin().is_terminal();
    let hide = |on: bool| {
        if interactive {
            echo_input(!on);
        }
    };
    if interactive {
        eprint!("{prompt}");
        let _ = std::io::stderr().flush();
    }
    hide(true);
    let mut line = String::new();
    let read = std::io::stdin().lock().read_line(&mut line);
    hide(false);
    if interactive {
        eprintln!();
    }
    read.map_err(|e| Error::invalid(e.to_string()))?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

/// Turn the terminal's echo of typed characters on or off.
#[cfg(unix)]
fn echo_input(on: bool) {
    let _ = std::process::Command::new("stty").arg(if on { "echo" } else { "-echo" }).status();
}

/// Turn the console's echo of typed characters on or off.
#[cfg(windows)]
fn echo_input(on: bool) {
    use windows_sys::Win32::System::Console::{
        CONSOLE_MODE, ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
    };
    // SAFETY: GetStdHandle returns this process's console input handle (or an invalid one, which GetConsoleMode then
    // rejects), and the mode is only written back to the same handle.
    unsafe {
        let input = GetStdHandle(STD_INPUT_HANDLE);
        let mut mode: CONSOLE_MODE = 0;
        if GetConsoleMode(input, &mut mode) != 0 {
            SetConsoleMode(input, if on { mode | ENABLE_ECHO_INPUT } else { mode & !ENABLE_ECHO_INPUT });
        }
    }
}

#[cfg(not(any(unix, windows)))]
fn echo_input(_on: bool) {}
