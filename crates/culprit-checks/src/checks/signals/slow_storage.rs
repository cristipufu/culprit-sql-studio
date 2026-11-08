//! The `slow-storage` check: Average read and write latency per database file since startup: sys.dm_io_virtual_file_stats, pg_stat_io (16+, with track_io_timing), performance_schema file I/O.

use super::*;
use crate::evidence::Requirement as Evidence;
use std::collections::BTreeSet;

pub fn slow_storage_info() -> CheckInfo {
    CheckInfo {
        id: "slow-storage".into(),
        title: t!("check.slow_storage.info.title"),
        category: Category::Configuration,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Mysql],
        requires: vec![],
        description: t!("check.slow_storage.info.description"),
    }
}

check!(
    SlowStorageCheck: slow_storage_info,
    decide = slow_files,
    render = slow_storage_finding,
    case<'s> = SlowFiles,
    evidence = &[Evidence::Pressure { signal: None, timed_files: true, complete: true }],
);

/// Reads (or log writes) at least this large on average are bulk I/O: backups, CHECKDB, read-ahead of large scans. Each
/// takes longer than the page-sized reads queries wait on, so their average says little about the storage.
pub(super) const BULK_IO_BYTES: f64 = 64.0 * 1024.0;

/// The files of one kind in one database, added up for the operation that matters for that kind: reads of data and
/// temporary files, writes of the log (every commit waits for them).
pub(super) struct FileGroup<'a> {
    files: Vec<&'a FileIo>,
    database: Option<String>,
    kind: FileKind,
    write: bool,
    ops: f64,
    ms: f64,
    /// Bytes moved, when every file says.
    bytes: Option<f64>,
}

impl FileGroup<'_> {
    /// Average time per operation, weighted by operations across the files: one quiet file read in large chunks by a
    /// backup doesn't stand for the database's storage.
    fn avg_ms(&self) -> f64 {
        per_run(self.ms, self.ops)
    }

    /// Average bytes per operation, when known.
    fn avg_bytes(&self) -> Option<f64> {
        self.bytes.map(|b| per_run(b, self.ops))
    }

    fn bulk(&self) -> bool {
        self.avg_bytes().is_some_and(|b| b >= BULK_IO_BYTES)
    }
}

/// Every kind of file in every database whose operation-weighted average crosses its threshold over 1,000 operations.
/// tempdb's log is left out: SQL Server doesn't harden it at commit, so no commit waits for those writes.
pub(super) fn slow_groups<'a>(snapshot: &Snapshot, files: &'a [FileIo]) -> Vec<FileGroup<'a>> {
    let mut groups: BTreeMap<(u8, Option<String>), FileGroup<'a>> = BTreeMap::new();
    for f in files {
        let (order, write) = match f.kind {
            FileKind::Data => (0, false),
            FileKind::Temp => (1, false),
            FileKind::Log => (2, true),
            FileKind::Other => continue,
        };
        if snapshot.server.engine == Engine::Mssql && f.kind == FileKind::Log && f.database.as_deref() == Some("tempdb")
        {
            continue;
        }
        let (ops, ms, bytes) =
            if write { (f.writes, f.write_ms, f.write_bytes) } else { (f.reads, f.read_ms, f.read_bytes) };
        // An operation nobody timed says nothing about its time.
        let Some(ms) = ms.filter(|_| ops > 0.0) else { continue };
        let g = groups.entry((order, f.database.clone())).or_insert_with(|| FileGroup {
            files: Vec::new(),
            database: f.database.clone(),
            kind: f.kind,
            write,
            ops: 0.0,
            ms: 0.0,
            bytes: Some(0.0),
        });
        g.files.push(f);
        g.ops += ops;
        g.ms += ms;
        g.bytes = g.bytes.zip(bytes).map(|(a, b)| a + b);
    }
    groups.into_values().filter(|g| g.ops >= 1_000.0 && g.avg_ms() >= if g.write { 10.0 } else { 20.0 }).collect()
}

/// What a group of files is called: by the database and kind where the engine says, else by the file.
#[derive(Clone)]
pub enum FileLabel {
    /// SQL Server's tempdb, by its name.
    Tempdb,
    /// A SQL Server database's log.
    Log(String),
    /// A SQL Server database's one data file: (database, file).
    DataFile(String, String),
    /// A SQL Server database's data files.
    DataFiles(String),
    /// PostgreSQL's WAL.
    Wal,
    /// PostgreSQL's temporary tables.
    TempTables,
    /// PostgreSQL's table and index files.
    TableFiles,
    /// The one file, by its name.
    File(String),
    /// Temporary files, log files or data files, by kind.
    Files(FileKind),
}

pub(super) fn file_label(snapshot: &Snapshot, g: &FileGroup) -> FileLabel {
    let one = (g.files.len() == 1).then(|| g.files[0].name.clone());
    match (snapshot.server.engine, g.kind, g.database.as_deref()) {
        (Engine::Mssql, _, Some("tempdb")) => FileLabel::Tempdb,
        (Engine::Mssql, FileKind::Log, Some(db)) => FileLabel::Log(db.into()),
        (Engine::Mssql, _, Some(db)) => match one {
            Some(name) => FileLabel::DataFile(db.into(), name),
            None => FileLabel::DataFiles(db.into()),
        },
        (Engine::Postgres, FileKind::Log, _) => FileLabel::Wal,
        (Engine::Postgres, FileKind::Temp, _) => FileLabel::TempTables,
        (Engine::Postgres, _, _) => FileLabel::TableFiles,
        (Engine::Mssql | Engine::Yugabyte | Engine::Mysql | Engine::Sqlite, kind, _) => match one {
            Some(name) => FileLabel::File(name),
            None => FileLabel::Files(kind),
        },
    }
}

fn label_words(label: &FileLabel) -> String {
    match label {
        FileLabel::Tempdb => "tempdb".into(),
        FileLabel::Log(db) => t!("check.slow_storage.label.log", database = db),
        FileLabel::DataFile(db, name) => t!("check.slow_storage.label.data_file", database = db, file = name),
        FileLabel::DataFiles(db) => t!("check.slow_storage.label.data_files", database = db),
        FileLabel::Wal => t!("check.slow_storage.label.wal"),
        FileLabel::TempTables => t!("check.slow_storage.label.temp_tables"),
        FileLabel::TableFiles => t!("check.slow_storage.label.table_files"),
        FileLabel::File(name) => name.clone(),
        FileLabel::Files(FileKind::Temp) => t!("check.slow_storage.label.temp_files"),
        FileLabel::Files(FileKind::Log) => t!("check.slow_storage.label.log_files"),
        FileLabel::Files(_) => t!("check.slow_storage.label.files"),
    }
}

/// Where the server runs, as the fix for storage that answers slowly depends on it.
#[derive(Clone, Copy)]
pub enum StorageHost {
    /// Azure SQL Database: the service tier decides the latency and the I/O limits.
    AzureSqlDatabase,
    /// Managed Instance General Purpose keeps each file on a remote disk sized from the file.
    ManagedInstance,
    Volume,
}

/// The operation a group of files is judged on.
#[derive(Clone, Copy, PartialEq)]
pub enum Operation {
    Read,
    Write,
    /// Log writes that are flushes.
    Flush,
}

impl Operation {
    /// As the messages' `select` reads it.
    fn key(self) -> &'static str {
        match self {
            Operation::Read => "read",
            Operation::Write => "write",
            Operation::Flush => "flush",
        }
    }
}

/// One kind of file in one database whose average read (or log write) crosses its threshold over 1,000 operations.
pub struct SlowFiles {
    files: Vec<FileIo>,
    database: Option<String>,
    label: FileLabel,
    /// The files' name in the finding's id: the one file's, or their kind's.
    what: String,
    operation: Operation,
    avg_ms: f64,
    ops: f64,
    avg_bytes: Option<f64>,
    /// Mostly large reads (backups, CHECKDB, read-ahead), whose average overstates what queries see.
    bulk: bool,
    /// Not this database's own files: context, not graded.
    shared: bool,
    severity: Severity,
    host: StorageHost,
}

fn slow_files(snapshot: &Snapshot) -> Vec<SlowFiles> {
    let Some(p) = snapshot.pressure.as_ref() else { return Vec::new() };
    let files = culprit_core::signals::merge_logs(&p.files);
    let host = match snapshot.server.platform() {
        Some(culprit_core::Platform::AzureSqlDatabase) => StorageHost::AzureSqlDatabase,
        Some(culprit_core::Platform::AzureSqlManagedInstance) => StorageHost::ManagedInstance,
        _ => StorageHost::Volume,
    };
    slow_groups(snapshot, &files)
        .into_iter()
        .map(|g| {
            let write = g.write;
            let operation = if !write {
                Operation::Read
            } else if g.files.iter().all(|f| f.write_operation == Some(culprit_core::FileWriteOperation::Flush)) {
                Operation::Flush
            } else {
                Operation::Write
            };
            let what = match g.files.as_slice() {
                [one] => one.name.clone(),
                _ => format!(
                    "{} files",
                    match g.kind {
                        FileKind::Temp => "temp",
                        FileKind::Log => "log",
                        _ => "data",
                    }
                ),
            };
            // Only reads come in bulk sizes that slow each one down; a log is judged on its commits.
            let bulk = !write && g.bulk();
            let shared = snapshot.server.engine != Engine::Mssql
                || g.database.as_deref().is_none_or(|db| !db.eq_ignore_ascii_case(&snapshot.server.database));
            let amount = counter_rate_per_day(snapshot, g.ms);
            // Historical/shared averages do not establish a current incident in this database. A slow read also
            // needs an amount behind it: a thousand old reads on a mostly idle database need no urgent warning.
            let severity = if bulk || shared || amount.is_none_or(|ms| ms < MEDIUM_MS_A_DAY) {
                Severity::Low
            } else {
                Severity::Medium
            };
            SlowFiles {
                label: file_label(snapshot, &g),
                files: g.files.iter().map(|f| (*f).clone()).collect(),
                database: g.database.clone(),
                what,
                operation,
                avg_ms: g.avg_ms(),
                ops: g.ops,
                avg_bytes: g.avg_bytes(),
                bulk,
                shared,
                severity,
                host,
            }
        })
        .collect()
}

fn slow_storage_finding(snapshot: &Snapshot, slow: SlowFiles) -> Finding {
    let label = label_words(&slow.label);
    let op = slow.operation.key();
    let key =
        format!("{}:{}", slow.database.as_deref().unwrap_or(""), culprit_core::redact::shareable_name(&slow.what));
    let mut f = finding("slow-storage", &key, Category::Configuration, subject(SubjectKind::Database, &label));
    let avg = format::duration(slow.avg_ms);
    f.title = if slow.bulk {
        t!(
            "check.slow_storage.title.bulk",
            label = &label,
            average = &avg,
            size = format::bytes(slow.avg_bytes.unwrap_or(0.0))
        )
    } else {
        t!("check.slow_storage.title.average", operation = op, label = &label, average = &avg)
    };
    // PostgreSQL says what each file's statistics cover; the others count since the server started.
    let period = if snapshot.server.engine == Engine::Postgres {
        let coverage = slow
            .files
            .iter()
            .map(|f| f.coverage(snapshot))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>()
            .join("; ");
        t!("check.slow_storage.coverage", coverage = coverage)
    } else {
        since_start(snapshot)
    };
    // The label starts with a database name: never capitalize it, lead with the operation instead.
    let mut summary = vec![t!(
        "check.slow_storage.summary.took",
        operation = op,
        label = &label,
        average = &avg,
        count = format::count(slow.ops),
        period = period
    )];
    match snapshot.server.engine {
        Engine::Postgres => summary.push(t!("check.slow_storage.summary.cache_postgres")),
        Engine::Mysql => summary.push(t!("check.slow_storage.summary.cache_mysql")),
        Engine::Mssql | Engine::Sqlite | Engine::Yugabyte => {}
    }
    if slow.bulk {
        summary.push(t!("check.slow_storage.summary.bulk", size = format::bytes(slow.avg_bytes.unwrap_or(0.0))));
    }
    f.summary = summary.join(" ");
    f.severity = slow.severity;
    severity_score(&mut f);
    f.impact.label = t!("check.slow_storage.impact", average = &avg, operation = op);
    f.confidence = Confidence::Medium;
    f.metrics = vec![
        Metric::new(t!("check.slow_storage.metric.average", operation = op), slow.avg_ms, Unit::Ms),
        Metric::new(t!("check.slow_storage.metric.count", operation = op), slow.ops, Unit::Count),
        Metric::text(
            t!("check.slow_storage.metric.files", n = slow.files.len()),
            slow.files.iter().map(|f| f.name.clone()).collect::<Vec<_>>().join(", "),
        ),
    ];
    let snapshot_files = snapshot.pressure.as_ref().map_or(&[][..], |p| p.files.as_slice());
    f.facts.storage_files =
        slow.files.iter().flat_map(|merged| culprit_core::signals::merged_from(merged, snapshot_files)).collect();
    if let Some(b) = slow.avg_bytes.filter(|_| slow.operation == Operation::Read) {
        f.metrics.push(Metric::new(t!("check.slow_storage.metric.read_size"), b, Unit::Bytes));
    }
    f.fix = Some(Fix {
        summary: match slow.host {
            StorageHost::AzureSqlDatabase => t!("check.slow_storage.fix.azure_sql_database"),
            StorageHost::ManagedInstance => t!("check.slow_storage.fix.managed_instance"),
            StorageHost::Volume => t!("check.slow_storage.fix.volume"),
        },
        sql: None,
        cost: None,
    });
    f.verify = vec![t!("check.slow_storage.verify", operation = op)];
    f.caveats.push(t!("check.slow_storage.caveat.historical"));
    if slow.shared {
        f.caveats.push(t!("check.slow_storage.caveat.shared"));
    }
    f
}
