//! Plan insights about parallel plans: skewed threads, and parallelism a statement didn't need or couldn't get.

use super::*;

/// What a spilling operator wrote to disk, when the plan says: tempdb pages (SQL Server), the sort's or hash
/// aggregate's own disk use, or the temporary blocks the operator itself wrote (PostgreSQL with BUFFERS).
pub(super) fn spilled_bytes(n: &PlanNode) -> Option<f64> {
    let disk_kb = n
        .facts()
        .sort_space_kb
        .filter(|_| n.facts().sort_on_disk == Some(true))
        .or_else(|| n.facts().disk_usage_kb.filter(|kb| *kb > 0.0));
    disk_kb.map(|kb| kb * 1024.0).or_else(|| crate::plan_facts::spill_pages(n).map(|p| p * 8192.0))
}

pub(super) fn spill(cx: &Cx) -> Vec<Draft> {
    let mut out = Vec::new();
    for n in cx.nodes.iter().filter(|n| n.facts().exchange_spill == Some(true)) {
        let mut d = Draft::new("spill", InsightArea::Parallelism, cx.share(n), vec![n.id]);
        d.title = match spilled_bytes(n) {
            Some(b) => t!("insight.spill.exchange.title_size", op = &n.op, size = format::bytes(b)),
            None => t!("insight.spill.exchange.title", op = &n.op),
        };
        d.detail = t!("insight.spill.exchange.detail");
        d.fix = t!("insight.spill.exchange.fix");
        d.verify = t!("insight.spill.exchange.verify");
        out.push(d);
    }
    // The shared misestimate threshold establishes an observed mismatch, not the spill's cause.
    let misestimated: HashMap<u32, f64> = cx.plan.root.misestimates().into_iter().collect();
    for n in cx.nodes.iter().filter(|n| n.warnings.contains(&PlanWarning::Spill)) {
        let mut d = Draft::new("spill", InsightArea::Memory, cx.share(n), vec![n.id]);
        // Native spill size describes temporary work; it does not measure elapsed cost.
        match spilled_bytes(n) {
            Some(bytes) if bytes < SPILL_MATERIAL_BYTES => d.cap = Severity::Low,
            Some(_) => d.floor = Severity::Medium,
            None => {}
        }
        let underestimated = {
            let mut below = Vec::new();
            n.walk(&mut below);
            below.iter().any(|b| misestimated.get(&b.id).is_some_and(|m| *m > 0.0))
        };
        match cx.engine() {
            // EXPLAIN QUERY PLAN reports no spills.
            Engine::Sqlite => continue,
            Engine::Postgres | Engine::Yugabyte => {
                let disk_kb = n.facts().sort_space_kb.filter(|_| n.facts().sort_on_disk == Some(true));
                let batches = n.facts().hash_batches.map(f64::from).filter(|b| *b > 1.0);
                let agg_batches = n.facts().hash_aggregate_batches.map(f64::from).filter(|b| *b > 1.0);
                d.title = match (disk_kb, batches, agg_batches, spilled_bytes(n)) {
                    (Some(kb), _, _, _) => t!("insight.spill.work_mem.title_sort", size = format::bytes(kb * 1024.0)),
                    (None, Some(b), _, _) => {
                        t!("insight.spill.work_mem.title_hash", n = b, batches = format::count(b))
                    }
                    (None, None, _, Some(bytes)) => {
                        t!("insight.spill.work_mem.title_files", op = &n.op, size = format::bytes(bytes))
                    }
                    (None, None, Some(b), None) => {
                        t!("insight.spill.work_mem.title_groups", op = &n.op, n = b, batches = format::count(b))
                    }
                    (None, None, None, None) => t!("insight.spill.work_mem.title", op = &n.op),
                };
                let mut detail = vec![t!("insight.spill.work_mem.detail")];
                if underestimated {
                    detail.push(t!("insight.spill.work_mem.underestimated"));
                }
                d.detail = detail.join(" ");
                d.fix = t!("insight.spill.work_mem.fix");
                d.verify = t!("insight.spill.work_mem.verify");
            }
            // MariaDB's ANALYZE counts a sort's merge passes: it didn't fit in sort_buffer_size.
            Engine::Mysql => {
                let passes = n.facts().sort_merge_passes.filter(|passes| *passes > 0).map(f64::from);
                d.title = match passes {
                    Some(passes) => {
                        t!("insight.spill.sort_buffer.title", n = plural_n(passes), count = rows_text(passes))
                    }
                    None => t!("insight.spill.sort_buffer.title_unknown", op = &n.op),
                };
                let mut detail = vec![if passes.is_some() {
                    t!("insight.spill.sort_buffer.detail")
                } else {
                    t!("insight.spill.sort_buffer.detail_unknown")
                }];
                if underestimated {
                    detail.push(t!("insight.spill.sort_buffer.underestimated"));
                }
                d.detail = detail.join(" ");
                d.fix = if passes.is_some() {
                    t!("insight.spill.sort_buffer.fix")
                } else {
                    t!("insight.spill.sort_buffer.fix_unknown")
                };
                d.verify = t!("insight.spill.sort_buffer.verify");
            }
            Engine::Mssql => {
                d.title = match crate::plan_facts::spill_pages(n) {
                    Some(pages) => {
                        t!("insight.spill.tempdb.title_size", op = &n.op, size = format::bytes(pages * 8192.0))
                    }
                    None => t!("insight.spill.tempdb.title", op = &n.op),
                };
                let mut detail = vec![t!("insight.spill.tempdb.detail", op = &n.op)];
                if underestimated {
                    detail.push(t!("insight.spill.tempdb.underestimated"));
                }
                d.detail = detail.join(" ");
                d.fix = t!("insight.spill.tempdb.fix");
                d.verify = t!("insight.spill.tempdb.verify");
            }
        }
        if n.kind == NodeKind::Sort {
            d.fix = [d.fix, t!("insight.spill.fix.order")].join(" ");
        }
        out.push(d);
    }
    out
}

pub(super) fn parallelism_blocked(cx: &Cx) -> Vec<Draft> {
    // The plan doesn't carry the server's cost threshold for parallelism; the snapshot's settings may
    // ([`COST_THRESHOLD`]). Without them, below its default (5) no setting would make the plan parallel, and only above
    // the 50 it's commonly raised to does running serially say much.
    const DEFAULT_THRESHOLD: f64 = 5.0;
    const COMMON_THRESHOLD: f64 = 50.0;
    let facts = cx.plan.facts();
    let Some(reason) = facts.parallelism_blocker.as_deref() else { return Vec::new() };
    let cost = cx.plan.total_cost;
    let threshold = facts.cost_threshold_for_parallelism;
    // A threshold still at its default is one Culprit's own advice raises to the common 50
    // (config:cost-threshold-for-parallelism): a plan under that would stay on one CPU once it is raised, so removing
    // what keeps it serial now is no advice to give beside it. One answer for both findings.
    let bar = match threshold {
        Some(t) if t <= DEFAULT_THRESHOLD => COMMON_THRESHOLD,
        Some(t) => t,
        None => DEFAULT_THRESHOLD,
    };
    if reason == "TSQLUserDefinedFunctionsNotParallelizable" || cost < bar {
        return Vec::new();
    }
    // Which MAXDOP is 1: the statement's own hint, or the database's or the server's setting as the snapshot read them.
    let statement = cx.statement();
    let hinted = statement.match_indices("maxdop").any(|(at, _)| {
        statement[at + 6..]
            .trim_start()
            .strip_prefix('1')
            .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_digit()))
    });
    let (database, server) = (facts.database_maxdop, facts.server_maxdop);
    // Which setting keeps MAXDOP at 1, as far as the plan and the snapshot say.
    let maxdop = if hinted {
        Maxdop::Hint
    } else if database == Some(1) {
        Maxdop::Database
    } else if database.is_none_or(|d| d == 0) && server == Some(1) {
        Maxdop::Server
    } else if database.is_some() && server.is_some() {
        Maxdop::Elsewhere
    } else {
        Maxdop::Unknown
    };
    let mut d = Draft::new("parallelism-blocked", InsightArea::Parallelism, 0.1, vec![cx.plan.root.id]);
    d.fixed_weight = true;
    d.subtree = true;
    d.cap = if cost >= COMMON_THRESHOLD { Severity::Medium } else { Severity::Low };
    // SQL Server's own reason on a plan expensive enough to go parallel is Medium; a cheap one's is Low.
    d.floor = d.cap;
    d.title = match reason {
        "MaxDOPSetToOne" => match maxdop {
            Maxdop::Hint => t!("insight.parallelism_blocked.title.maxdop_hint"),
            Maxdop::Database => t!("insight.parallelism_blocked.title.maxdop_database"),
            Maxdop::Server => t!("insight.parallelism_blocked.title.maxdop_server"),
            Maxdop::Elsewhere => t!("insight.parallelism_blocked.title.maxdop_elsewhere"),
            Maxdop::Unknown => t!("insight.parallelism_blocked.title.maxdop_unknown"),
        },
        "EstimatedDOPIsOne" => t!("insight.parallelism_blocked.title.one_cpu"),
        "NoParallelPlansInDesktopOrExpressEdition" => t!("insight.parallelism_blocked.title.edition"),
        "CouldNotGenerateValidParallelPlan" => t!("insight.parallelism_blocked.title.no_valid_plan"),
        "NonParallelizableIntrinsicFunction" => t!("insight.parallelism_blocked.title.intrinsic_function"),
        "TableVariableTransactionsDoNotSupportParallelNestedTransaction" => {
            t!("insight.parallelism_blocked.title.table_variable")
        }
        "NoParallelForMemoryOptimizedTables" => t!("insight.parallelism_blocked.title.memory_optimized"),
        "UpdatingWritebackVariable" => t!("insight.parallelism_blocked.title.writeback_variable"),
        "CLRUserDefinedFunctionRequiresDataAccess" => t!("insight.parallelism_blocked.title.clr_function"),
        _ => t!("insight.parallelism_blocked.title.reported", reason = reason),
    };
    // The plan's estimated cost and the thresholds, as whole numbers.
    let whole = |v: f64| format::digits(format!("{v:.0}"));
    d.detail = match threshold {
        Some(t) if t <= DEFAULT_THRESHOLD => t!(
            "insight.parallelism_blocked.detail.default_threshold",
            cost = whole(cost),
            threshold = whole(t),
            common = whole(COMMON_THRESHOLD)
        ),
        Some(t) => t!("insight.parallelism_blocked.detail.threshold", cost = whole(cost), threshold = whole(t)),
        None => [
            t!("insight.parallelism_blocked.detail.cost", cost = whole(cost)),
            if cost >= COMMON_THRESHOLD {
                t!("insight.parallelism_blocked.detail.unknown_threshold_passed")
            } else {
                t!("insight.parallelism_blocked.detail.unknown_threshold")
            },
        ]
        .join(" "),
    };
    d.fix = match reason {
        "MaxDOPSetToOne" if hinted => t!("insight.parallelism_blocked.fix.maxdop_hint"),
        "MaxDOPSetToOne" if database == Some(1) => t!("insight.parallelism_blocked.fix.maxdop_database"),
        "MaxDOPSetToOne" if server == Some(1) => t!("insight.parallelism_blocked.fix.maxdop_server"),
        "MaxDOPSetToOne" if database.is_some() && server.is_some() => {
            t!("insight.parallelism_blocked.fix.maxdop_elsewhere")
        }
        "MaxDOPSetToOne" => t!("insight.parallelism_blocked.fix.maxdop_unknown"),
        _ => t!("insight.parallelism_blocked.fix.other"),
    };
    d.verify = t!("insight.parallelism_blocked.verify");
    vec![d]
}

pub(super) fn parallel_skew(cx: &Cx) -> Vec<Draft> {
    let mut out = Vec::new();
    for n in &cx.nodes {
        let Some((workers, total, max)) = crate::plan_facts::parallel_distribution(n, cx.engine()) else {
            continue;
        };
        let avg = total / workers as f64;
        if total < 10_000.0 || avg <= 0.0 || max < 3.0 * avg {
            continue;
        }
        // Skew costs time only in proportion to the work involved, and a TOP makes uneven threads normal.
        let has_top = cx.nodes.iter().any(|x| x.op == "Top" || x.op == "Limit");
        let mut d = Draft::new("parallel-skew", InsightArea::Parallelism, cx.share(n) * 0.3, vec![n.id]);
        d.cap = if has_top || total < 100_000.0 { Severity::Low } else { Severity::Medium };
        // PostgreSQL's parallel workers are processes; SQL Server's are threads.
        let (workers, share) = (format::digits(workers.to_string()), format::percent(max / total));
        if cx.engine() == Engine::Postgres {
            d.title = t!("insight.parallel_skew.title_process", workers = workers, share = share, op = &n.op);
            d.detail = t!("insight.parallel_skew.detail_process");
            d.verify = t!("insight.parallel_skew.verify_process");
        } else {
            d.title = t!("insight.parallel_skew.title_thread", workers = workers, share = share, op = &n.op);
            d.detail = t!("insight.parallel_skew.detail_thread");
            d.verify = t!("insight.parallel_skew.verify_thread");
        }
        d.fix = t!("insight.parallel_skew.fix");
        out.push(d);
    }
    out.sort_by(|a, b| b.impact.total_cmp(&a.impact));
    out.into_iter().take(1).collect()
}

/// Which setting makes MAXDOP 1 for a plan SQL Server ran serially for that reason.
enum Maxdop {
    /// The statement's own OPTION (MAXDOP 1).
    Hint,
    /// The database's MAXDOP.
    Database,
    /// The server's max degree of parallelism, the database leaving it to the server.
    Server,
    /// Neither, though both are known: a Query Store hint, a workload group, or a setting changed since.
    Elsewhere,
    /// The snapshot doesn't say.
    Unknown,
}
