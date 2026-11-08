//! Declarative requirements for a negative check result. Partial evidence can still produce findings;
//! it cannot establish that an unobserved problem is absent. Requirements live beside each check.

use culprit_core::{CheckEvidence, Snapshot, SourceState, source_id, t};

/// Facts a source can contribute, independent of any check's identifier.
#[derive(Clone, Copy, Debug)]
pub enum Observation {
    LogFailures,
    RequestCounts,
    Spaces,
    Sequences,
    Multixacts,
    OperationalCounters,
    Cluster,
    WalSize,
}

#[derive(Clone, Copy, Debug)]
pub enum QueryMeasure {
    Work,
    Plans,
    CapturedPlans,
    Timeline,
    PlanVariants,
    Variance,
    ParameterSpread,
    Failures,
    Planning,
    Jit,
    LogBytes,
    Rows,
    Histograms,
    TempWrites,
    Grants,
    Waits(&'static str),
}

#[derive(Clone, Copy, Debug)]
pub enum TableMeasure {
    Heap,
    DeadRows,
    Xid,
    Bloat,
    HotUpdates,
    RowSize,
    Scans,
    Writes,
}

/// A reusable evidence contract. Custom checks can use the same evaluator in `Check::evidence`.
#[derive(Clone, Copy, Debug)]
pub enum Requirement<'a> {
    Observed(Observation, &'a [&'a str]),
    Source(&'a str),
    IndexCatalog,
    ForeignKeys,
    IndexHistory,
    Query(QueryMeasure),
    Tables(TableMeasure),
    Statistics,
    Pressure {
        signal: Option<&'a str>,
        timed_files: bool,
        complete: bool,
    },
    Settings,
    Deadlocks,
    Unknown,
    /// Evidence the check knows is incomplete, and why: the key of the message that says so.
    Incomplete(&'a str),
}

pub fn source_failed(s: &Snapshot, names: &[&str]) -> Option<String> {
    s.source_problem(names)
}

pub fn index_catalog(s: &Snapshot) -> Option<String> {
    catalog_of(s, |_| true)
}

/// [`index_catalog`] for what one table's indexes can serve: a YugabyteDB index elsewhere without sharding metadata
/// says nothing about this table's, while a failed source still does.
pub fn index_catalog_for_table(s: &Snapshot, schema: &str, table: &str) -> Option<String> {
    catalog_of(s, |i| i.schema.eq_ignore_ascii_case(schema) && i.table.eq_ignore_ascii_case(table))
}

fn catalog_of(s: &Snapshot, judged: impl Fn(&culprit_core::IndexStat) -> bool) -> Option<String> {
    if s.server.engine == culprit_core::Engine::Yugabyte {
        if let Some(reason) = source_failed(s, &[source_id::TABLE_STORAGE]) {
            return Some(reason);
        }
        if s.indexes.iter().filter(|index| judged(index)).any(|index| index.docdb.is_none()) {
            return Some(t!("evidence.docdb_sharding_missing"));
        }
    }
    source_failed(
        s,
        &[source_id::TABLES, source_id::INDEXES, source_id::FOREIGN_KEYS, source_id::COLUMNS, source_id::CATALOG],
    )
}

pub fn assess(requirements: &[Requirement<'_>], s: &Snapshot) -> CheckEvidence {
    if requirements.is_empty() {
        return CheckEvidence::Insufficient(t!("evidence.undeclared"));
    }
    let mut result = CheckEvidence::Available;
    for requirement in requirements {
        match requirement.assess(s) {
            evidence @ CheckEvidence::Unavailable(_) => return evidence,
            evidence @ CheckEvidence::NotApplicable(_) => result = evidence,
            evidence @ CheckEvidence::Insufficient(_) if matches!(result, CheckEvidence::Available) => {
                result = evidence
            }
            evidence @ CheckEvidence::Insufficient(_) if matches!(result, CheckEvidence::Queries { .. }) => {
                result = evidence
            }
            CheckEvidence::Queries { assessed, reason } => match &mut result {
                CheckEvidence::Available => result = CheckEvidence::Queries { assessed, reason },
                CheckEvidence::Queries { assessed: previous, reason: previous_reason } => {
                    previous.retain(|q| assessed.contains(q));
                    if previous_reason.is_none() {
                        *previous_reason = reason;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
    result
}

impl Requirement<'_> {
    fn assess(&self, s: &Snapshot) -> CheckEvidence {
        use CheckEvidence::*;
        let insufficient = |why: String| Insufficient(why);
        match self {
            Self::Observed(observation, sources) => {
                let known = match observation {
                    Observation::LogFailures => !s.operations.log_failures.is_empty(),
                    Observation::RequestCounts => s.queries().iter().any(|q| q.requests.is_some()),
                    Observation::Spaces => !s.operations.spaces.is_empty(),
                    Observation::Sequences => !s.operations.sequences.is_empty(),
                    Observation::Multixacts => !s.operations.multixacts.is_empty(),
                    Observation::OperationalCounters => !s.operations.counters.is_empty(),
                    Observation::Cluster => s.operations.cluster.is_some(),
                    Observation::WalSize => s.operations.sqlite.as_ref().is_some_and(|w| w.wal_bytes.is_some()),
                };
                if let Some(reason) = source_failed(s, sources) {
                    return if known { Insufficient(reason) } else { Unavailable(reason) };
                }
                if known {
                    return Available;
                }
                if !sources.is_empty()
                    && sources
                        .iter()
                        .all(|name| s.source_outcome(name).is_some_and(|o| o.state == SourceState::NotApplicable))
                {
                    return NotApplicable(
                        s.source_outcome(sources[0])
                            .and_then(|o| o.reason.clone())
                            .unwrap_or_else(|| t!("evidence.source_not_applicable")),
                    );
                }
                insufficient(t!("evidence.source_not_recorded"))
            }
            Self::Source(name) => match s.source_outcome(name) {
                Some(o) => match o.state {
                    SourceState::Collected | SourceState::Empty => Available,
                    SourceState::Partial => Insufficient(
                        o.reason.clone().unwrap_or_else(|| t!("evidence.named_source_partial", name = *name)),
                    ),
                    SourceState::Unavailable => Unavailable(
                        o.reason.clone().unwrap_or_else(|| t!("evidence.named_source_unavailable", name = *name)),
                    ),
                    SourceState::NotApplicable => NotApplicable(
                        o.reason.clone().unwrap_or_else(|| t!("evidence.named_source_not_applicable", name = *name)),
                    ),
                },
                None => insufficient(t!("evidence.source_outcome_missing")),
            },
            Self::IndexCatalog => index_catalog(s)
                .map(|reason| Unavailable(t!("evidence.schema_incomplete", reason = reason)))
                .unwrap_or(Available),
            Self::ForeignKeys if s.foreign_keys.is_empty() => {
                if [source_id::FOREIGN_KEYS, source_id::CATALOG].iter().any(|name| {
                    s.source_outcome(name).is_some_and(|o| {
                        matches!(o.state, SourceState::Collected | SourceState::Empty | SourceState::NotApplicable)
                    })
                }) {
                    NotApplicable(t!("evidence.no_foreign_keys"))
                } else {
                    insufficient(t!("evidence.foreign_keys_unestablished"))
                }
            }
            Self::ForeignKeys => Available,
            Self::IndexHistory
                if s.server.engine == culprit_core::Engine::Yugabyte
                    && !s
                        .source_outcome(source_id::DOCDB_INDEX_USAGE)
                        .is_some_and(|o| matches!(o.state, SourceState::Collected | SourceState::Empty)) =>
            {
                insufficient(t!("evidence.index_usage_cluster"))
            }
            Self::IndexHistory if !crate::indexes::watched_long_enough(s) => {
                insufficient(t!("evidence.index_usage_week"))
            }
            Self::IndexHistory => Available,
            Self::Query(measure) => query(*measure, s),
            Self::Tables(measure) => tables(*measure, s),
            Self::Statistics => {
                if let Some(reason) = source_failed(s, &[source_id::STATISTICS, source_id::TABLES]) {
                    return Unavailable(reason);
                }
                if !s.statistics.is_empty()
                    || s.source_outcome(source_id::STATISTICS)
                        .is_some_and(|o| matches!(o.state, SourceState::Collected | SourceState::Empty))
                    || (!s.tables.is_empty() && s.tables.iter().all(|t| t.modifications_since_analyze.is_some()))
                {
                    Available
                } else {
                    insufficient(t!("evidence.statistics_incomplete"))
                }
            }
            Self::Pressure { signal, timed_files, complete } => {
                let Some(p) = &s.pressure else {
                    return Unavailable(t!("evidence.pressure_missing"));
                };
                if signal.is_some_and(|key| p.signal(key).is_none()) {
                    return insufficient(t!("evidence.pressure_signal_missing"));
                }
                if signal
                    .and_then(|key| p.signal(key))
                    .is_some_and(|s| !s.value.is_finite() || s.status == culprit_core::SignalStatus::Info)
                {
                    return insufficient(t!("evidence.pressure_scope"));
                }
                if *timed_files
                    && !p.files.iter().any(|f| {
                        f.avg_read_ms().into_iter().chain(f.avg_write_ms()).any(|ms| ms.is_finite() && ms >= 0.0)
                    })
                {
                    return insufficient(t!("evidence.storage_untimed"));
                }
                if *complete { Available } else { insufficient(t!("evidence.incomplete_coverage")) }
            }
            Self::Settings if s.settings.is_empty() => insufficient(t!("evidence.no_settings")),
            Self::Deadlocks if s.deadlocks.is_none() => insufficient(t!("evidence.no_deadlocks")),
            Self::Settings => source_failed(
                s,
                &[
                    source_id::SETTINGS,
                    source_id::ADVICE_SETTINGS,
                    source_id::SERVER_VARIABLES,
                    source_id::TABLES,
                    source_id::CATALOG,
                    source_id::STATISTICS,
                    source_id::WORKLOAD,
                    source_id::GLOBAL_STATUS,
                    source_id::PERFORMANCE_COUNTERS,
                    source_id::PLAN_CACHE_USE,
                    source_id::RESOURCE_USE,
                ],
            )
            .map(Insufficient)
            .unwrap_or(Available),
            Self::Deadlocks => source_failed(s, &[source_id::DEADLOCKS]).map(Unavailable).unwrap_or_else(|| {
                let Some(d) = s.deadlocks.as_ref() else {
                    return insufficient(t!("evidence.no_deadlocks"));
                };
                if d.counted.is_some() || d.note.is_some() {
                    insufficient(t!("evidence.deadlock_sample"))
                } else {
                    Available
                }
            }),
            Self::Unknown => insufficient(t!("evidence.incomplete_coverage")),
            Self::Incomplete(key) => insufficient(culprit_core::i18n::t(key, &[])),
        }
    }
}

fn query(measure: QueryMeasure, s: &Snapshot) -> CheckEvidence {
    use CheckEvidence::*;
    let insufficient = |why: String| Insufficient(why);
    let Some(w) = &s.workload else {
        if matches!(measure, QueryMeasure::Grants) && s.pressure.as_ref().is_some_and(|p| !p.grants.is_empty()) {
            return insufficient(t!("evidence.grants_without_history"));
        }
        return Unavailable(s.no_workload_reason().unwrap_or_else(|| t!("evidence.no_history")));
    };
    if w.queries.is_empty() {
        return insufficient(t!("evidence.no_statements"));
    }
    if matches!(measure, QueryMeasure::CapturedPlans) {
        if w.queries.iter().any(|q| q.plan.is_none()) {
            return insufficient(t!("evidence.some_plans"));
        }
        return insufficient(t!("evidence.captured_plans"));
    }
    if matches!(measure, QueryMeasure::Timeline) && !w.timeline.as_ref().is_some_and(|t| t.len() >= 4) {
        return insufficient(t!("evidence.timeline_short"));
    }
    let source = match w.kind() {
        Some(culprit_core::WorkloadKind::QueryStore) => source_id::QUERY_STORE,
        Some(culprit_core::WorkloadKind::PlanCache) => source_id::PLAN_CACHE,
        Some(culprit_core::WorkloadKind::QueryLog) => source_id::QUERY_LOG,
        _ => source_id::WORKLOAD,
    };
    if let Some(reason) = source_failed(s, &[source]) {
        return Insufficient(reason);
    }
    let detail_sources: &[&str] = match measure {
        QueryMeasure::Waits(_) => &[source_id::QUERY_WAITS],
        QueryMeasure::Failures => &[source_id::QUERY_COSTS, source_id::QUERY_ERRORS],
        QueryMeasure::Planning | QueryMeasure::Jit | QueryMeasure::LogBytes => &[source_id::QUERY_COSTS],
        QueryMeasure::Grants => &[source_id::GRANTS],
        QueryMeasure::Histograms => &[source_id::HISTOGRAMS],
        _ => &[],
    };
    if let Some(reason) = source_failed(s, detail_sources) {
        return Insufficient(reason);
    }
    let assessed: Vec<_> = w
        .queries
        .iter()
        .filter(|q| {
            let costs = |f: fn(&culprit_core::QueryCosts) -> bool| q.costs.as_ref().is_some_and(f);
            match measure {
                QueryMeasure::PlanVariants => {
                    q.workload_plans().len() >= 2
                        && q.workload_plans().iter().all(|p| {
                            p.executions >= culprit_core::PLAN_VARIANT_MIN_RUNS
                                && p.mean_time_ms.is_finite()
                                && p.first_seen.as_deref().and_then(culprit_core::format::epoch_seconds).is_some()
                                && p.last_seen.as_deref().and_then(culprit_core::format::epoch_seconds).is_some()
                        })
                }
                QueryMeasure::Variance => q.stddev_time_ms.is_some() && q.max_time_ms.is_some() && q.executions >= 50.0,
                QueryMeasure::ParameterSpread => !q.workload_plans().is_empty() || q.spread.is_some(),
                QueryMeasure::Failures => costs(|c| c.failed.is_some() || c.aborted.is_some()),
                QueryMeasure::Planning => costs(|c| c.plan_ms.is_some()),
                QueryMeasure::Jit => costs(|c| c.jit_ms.is_some()),
                QueryMeasure::LogBytes => costs(|c| c.log_bytes.is_some()),
                QueryMeasure::Rows => q.rows_per_execution().is_some(),
                QueryMeasure::Histograms => s.histograms.is_some(),
                QueryMeasure::TempWrites => q.temp_writes.is_some(),
                QueryMeasure::Grants => s.pressure.as_ref().is_some_and(|p| {
                    p.grants
                        .iter()
                        .any(|g| q.query_hash.as_deref() == Some(g.query_hash.as_str()) && g.executions >= 5.0)
                }),
                QueryMeasure::Waits(category) => q
                    .waits
                    .iter()
                    .any(|w| w.category.eq_ignore_ascii_case(category) && w.wait_ms.is_finite() && w.wait_ms >= 0.0),
                QueryMeasure::Plans => q.plan.is_some(),
                _ => true,
            }
        })
        .map(|q| q.id.clone())
        .collect();
    if matches!(measure, QueryMeasure::Work | QueryMeasure::Timeline | QueryMeasure::Histograms)
        && assessed.len() == w.queries.len()
    {
        Available
    } else if assessed.len() == w.queries.len() {
        Queries { assessed, reason: None }
    } else if assessed.is_empty() && matches!(measure, QueryMeasure::PlanVariants) {
        insufficient(t!("evidence.no_plan_variants"))
    } else if assessed.is_empty() {
        insufficient(t!("evidence.unmeasured"))
    } else {
        Queries { assessed, reason: Some(t!("evidence.some_unmeasured")) }
    }
}

fn tables(measure: TableMeasure, s: &Snapshot) -> CheckEvidence {
    use CheckEvidence::*;
    if let Some(reason) = source_failed(s, &[source_id::TABLES, source_id::CATALOG]) {
        return Unavailable(reason);
    }
    let storage = matches!(measure, TableMeasure::Bloat | TableMeasure::HotUpdates);
    if storage && let Some(reason) = source_failed(s, &[source_id::TABLE_STORAGE]) {
        return Unavailable(reason);
    }
    let complete = !s.tables.is_empty()
        && s.tables.iter().all(|t| match measure {
            TableMeasure::Heap => t.is_heap.is_some() && t.rows.is_some(),
            TableMeasure::DeadRows => t.rows.is_some() && t.dead_rows.is_some(),
            TableMeasure::Xid => t.xid_age.is_some(),
            TableMeasure::Bloat => t.storage.as_ref().is_some_and(|v| v.bloat_bytes.is_some()),
            TableMeasure::HotUpdates => {
                t.updates.is_some() && t.storage.as_ref().is_some_and(|v| v.hot_updates.is_some())
            }
            TableMeasure::RowSize => t.rows.is_some() && t.lob_bytes.is_some() && t.total_bytes.is_some(),
            TableMeasure::Scans => t.rows.is_some() && t.seq_rows_read.is_some(),
            TableMeasure::Writes => crate::indexes::measured_writes(s, &t.schema, &t.name).is_some(),
        });
    if complete
        || (s.tables.is_empty()
            && [source_id::TABLES, source_id::CATALOG]
                .iter()
                .any(|n| s.source_outcome(n).is_some_and(|o| o.state == SourceState::Empty)))
    {
        Available
    } else {
        Insufficient(t!("evidence.tables_unmeasured"))
    }
}
