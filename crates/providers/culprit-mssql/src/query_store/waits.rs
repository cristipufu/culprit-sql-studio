//! Query Store wait facts stay in milliseconds and retain their native category.
use crate::{
    read::{integer, invalid_data, text},
    value::Rec,
};
use culprit_core::{
    QueryWaitCoverage, Result, WaitStat,
    telemetry::{
        NativeWaitMeaning, QueryWaitSemantics, WaitCaptureCoverage, WaitCaptureState, WaitClass, WaitDisposition,
    },
};
use std::collections::{BTreeMap, BTreeSet};

struct Category {
    id: u8,
    native_name: &'static str,
    disposition: WaitDisposition,
}

// The native catalog's documented IDs, names and analytical policy have one owner for scans and exported metadata.
// https://learn.microsoft.com/sql/relational-databases/system-catalog-views/sys-query-store-wait-stats-transact-sql
// Query Store observes waits during execution, not compilation. Its Compilation category cannot establish the
// amount of time a statement spent compiling or identify the owner of a compilation-memory reservation.
const CATEGORIES: &[Category] = &[
    Category { id: 0, native_name: "Unknown", disposition: WaitDisposition::Unknown },
    Category { id: 1, native_name: "CPU", disposition: WaitDisposition::Analyze(WaitClass::Cpu) },
    Category { id: 2, native_name: "Worker Thread", disposition: WaitDisposition::Analyze(WaitClass::Workers) },
    Category { id: 3, native_name: "Lock", disposition: WaitDisposition::Analyze(WaitClass::Lock) },
    Category { id: 4, native_name: "Latch", disposition: WaitDisposition::Analyze(WaitClass::Latch) },
    Category { id: 5, native_name: "Buffer Latch", disposition: WaitDisposition::Analyze(WaitClass::Latch) },
    Category { id: 6, native_name: "Buffer IO", disposition: WaitDisposition::Analyze(WaitClass::Io) },
    Category { id: 7, native_name: "Compilation", disposition: WaitDisposition::Analyze(WaitClass::Compilation) },
    Category { id: 8, native_name: "SQL CLR", disposition: WaitDisposition::Analyze(WaitClass::Other) },
    Category { id: 9, native_name: "Mirroring", disposition: WaitDisposition::Analyze(WaitClass::Other) },
    Category { id: 10, native_name: "Transaction", disposition: WaitDisposition::Analyze(WaitClass::Other) },
    Category { id: 11, native_name: "Idle", disposition: WaitDisposition::Ignore },
    Category { id: 12, native_name: "Preemptive", disposition: WaitDisposition::Analyze(WaitClass::Other) },
    Category { id: 13, native_name: "Service Broker", disposition: WaitDisposition::Ignore },
    Category { id: 14, native_name: "Tran Log IO", disposition: WaitDisposition::Analyze(WaitClass::Log) },
    Category { id: 15, native_name: "Network IO", disposition: WaitDisposition::Analyze(WaitClass::Network) },
    Category { id: 16, native_name: "Parallelism", disposition: WaitDisposition::Analyze(WaitClass::Parallelism) },
    Category { id: 17, native_name: "Memory", disposition: WaitDisposition::Analyze(WaitClass::Memory) },
    Category { id: 18, native_name: "User Wait", disposition: WaitDisposition::Ignore },
    Category { id: 19, native_name: "Tracing", disposition: WaitDisposition::Ignore },
    Category { id: 20, native_name: "Full Text Search", disposition: WaitDisposition::Analyze(WaitClass::Other) },
    Category { id: 21, native_name: "Other Disk IO", disposition: WaitDisposition::Analyze(WaitClass::Io) },
    Category { id: 22, native_name: "Replication", disposition: WaitDisposition::Analyze(WaitClass::Other) },
    Category { id: 23, native_name: "Log Rate Governor", disposition: WaitDisposition::Analyze(WaitClass::Log) },
];

pub(crate) fn semantics(capture: WaitCaptureState) -> QueryWaitSemantics {
    QueryWaitSemantics {
        capture,
        coverage: WaitCaptureCoverage::Unverified,
        categories: CATEGORIES
            .iter()
            .map(|category| {
                (
                    category.id.to_string(),
                    NativeWaitMeaning { native_name: category.native_name.into(), disposition: category.disposition },
                )
            })
            .collect(),
    }
}

pub(crate) fn capture_state(row: &Rec) -> Result<WaitCaptureState> {
    match text(row, "wait_stats_capture_mode_desc")?.as_str() {
        "ON" => Ok(WaitCaptureState::Enabled),
        "OFF" => Ok(WaitCaptureState::Disabled),
        _ => Err(invalid_data()),
    }
}

pub(crate) const RELATION: &str =
    "WITH culprit_waits AS (SELECT /* culprit */ w.* FROM sys.query_store_wait_stats w)\n";
#[derive(Clone, Debug)]
pub(crate) struct Summary {
    pub plan_id: Option<String>,
    pub category_id: u8,
    pub category: String,
    pub total_ms: u64,
}
impl Summary {
    pub(crate) fn decode(row: &Rec, by_plan: bool) -> Result<Self> {
        Ok(Self {
            plan_id: if by_plan { Some(integer(row, "plan_id")?.to_string()) } else { None },
            category_id: integer(row, "wait_category")?.try_into().map_err(|_| invalid_data())?,
            category: text(row, "wait_category_desc")?,
            total_ms: integer(row, "wait_ms")?.try_into().map_err(|_| invalid_data())?,
        })
    }
}
pub(crate) fn summary_sql(by_plan: bool) -> String {
    let declaration = if by_plan { "DECLARE /* culprit */ @ids xml=CONVERT(xml,@P2);\n" } else { "" };
    let plan = if by_plan { "ws.plan_id," } else { "" };
    let selected = if by_plan {
        " AND ws.plan_id IN(SELECT x.value('@id[1]','bigint') FROM @ids.nodes('/ids/id') n(x))"
    } else {
        ""
    };
    format!(
        r#"{window_declaration}{declaration}{RELATION}SELECT /* culprit */ {plan} ws.wait_category,ws.wait_category_desc,SUM(ws.total_query_wait_time_ms) AS wait_ms
FROM culprit_waits ws
JOIN sys.query_store_runtime_stats_interval i ON i.runtime_stats_interval_id=ws.runtime_stats_interval_id
JOIN sys.query_store_plan p ON p.plan_id=ws.plan_id
JOIN sys.query_store_query q ON q.query_id=p.query_id
JOIN sys.query_store_query_text t ON t.query_text_id=q.query_text_id
WHERE {window_membership} AND {workload}{selected}
GROUP BY {plan} ws.wait_category,ws.wait_category_desc OPTION(MAXDOP 1)"#,
        workload = crate::statement_origin::application_candidates_sql("t.query_sql_text"),
        window_declaration = super::window_sql::DECLARATION,
        window_membership = super::window_sql::MEMBERSHIP,
    )
}

pub(crate) struct Folded {
    pub values: Vec<WaitStat>,
    pub unknown_categories: BTreeSet<String>,
}

pub(crate) fn fold<'a>(items: impl IntoIterator<Item = &'a Summary>) -> Folded {
    let mut by_category: BTreeMap<&'static str, (f64, BTreeMap<String, f64>)> = BTreeMap::new();
    let mut unknown_categories = BTreeSet::new();
    for item in items {
        let meaning =
            CATEGORIES.iter().find(|category| category.id == item.category_id && category.native_name == item.category);
        let class = match meaning.map(|category| category.disposition) {
            Some(WaitDisposition::Analyze(class)) => class,
            Some(WaitDisposition::Ignore) => continue,
            Some(WaitDisposition::Unknown) | None => {
                unknown_categories.insert(format!("{}:{}", item.category_id, item.category));
                continue;
            }
        };
        if item.total_ms == 0 {
            continue;
        }
        let entry = by_category.entry(class.category()).or_default();
        entry.0 += item.total_ms as f64;
        *entry.1.entry(item.category.clone()).or_default() += item.total_ms as f64;
    }
    let mut values = by_category
        .into_iter()
        .map(|(category, (wait_ms, sources))| WaitStat {
            category: category.into(),
            wait_type: sources.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|(source, _)| source),
            wait_ms,
            tasks: None,
        })
        .collect::<Vec<_>>();
    values.sort_by(|a, b| b.wait_ms.total_cmp(&a.wait_ms));
    Folded { values, unknown_categories }
}

pub(crate) fn coverage(complete: bool) -> QueryWaitCoverage {
    let classes = CATEGORIES
        .iter()
        .filter_map(|category| match category.disposition {
            WaitDisposition::Analyze(class) => Some(class),
            WaitDisposition::Ignore | WaitDisposition::Unknown => None,
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    QueryWaitCoverage { classes, complete }
}

#[cfg(test)]
mod native_tests;
#[cfg(test)]
mod tests;
