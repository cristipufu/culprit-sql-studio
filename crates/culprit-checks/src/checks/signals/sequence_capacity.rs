use super::*;
use crate::evidence::{Observation, Requirement as Evidence};
use culprit_core::operations::SequenceCapacity;

pub fn sequence_capacity_info() -> CheckInfo {
    CheckInfo {
        id: "sequence-capacity".into(),
        title: t!("check.sequence_capacity.info.title"),
        category: Category::Maintenance,
        engines: vec![Engine::Mssql, Engine::Postgres, Engine::Mysql],
        requires: vec![],
        description: t!("check.sequence_capacity.info.description"),
    }
}

check!(
    SequenceCapacityCheck: sequence_capacity_info,
    decide = near_limits,
    render = sequence_capacity_finding,
    case<'s> = NearLimit<'s>,
    evidence = &[Evidence::Observed(Observation::Sequences, &[culprit_core::source_id::SEQUENCE_CAPACITY])],
);

/// Something about to run out of numbers: an identity, sequence or auto-increment near its bound, a range-partitioned
/// table about to run out of partitions (table-bloat's finding, on the engines it doesn't run on), or a 32-bit primary
/// key on a huge table.
pub enum NearLimit<'s> {
    Sequence { sequence: &'s SequenceCapacity, remaining: u128, severity: Severity },
    Partition(super::table_bloat::ExpiringPartition),
    Int32Key { table: &'s TableStat, column: &'s culprit_core::ColumnInfo, rows: f64, severity: Severity },
}

fn near_limits(s: &Snapshot) -> Vec<NearLimit<'_>> {
    let mut out: Vec<NearLimit<'_>> = s
        .operations
        .sequences
        .iter()
        .filter_map(|seq| {
            let (remaining, total) = seq.remaining()?;
            if total == 0 {
                return None;
            }
            let fraction = remaining as f64 / total as f64;
            if fraction > 0.01 && (remaining > 10_000 || fraction > 0.1) {
                return None;
            }
            let severity = if remaining <= 10_000 { Severity::Medium } else { Severity::Low };
            Some(NearLimit::Sequence { sequence: seq, remaining, severity })
        })
        .collect();
    if s.server.engine != Engine::Postgres {
        out.extend(super::table_bloat::expiring_partitions(s).into_iter().map(NearLimit::Partition));
    }
    out.extend(int32_primary_keys_near_limit(s));
    out
}

/// Large tables (`>= 100M` rows) whose single-column primary key is a 32-bit signed integer (`int`, `integer`, `int4`,
/// `serial`), when not already covered by an explicit `operations.sequences` entry.
fn int32_primary_keys_near_limit(s: &Snapshot) -> Vec<NearLimit<'_>> {
    if s.server.engine == Engine::Sqlite || crate::evidence::index_catalog(s).is_some() || s.columns.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for t in &s.tables {
        if t.partition_of.is_some() {
            continue;
        }
        let Some(rows) =
            t.rows.or_else(|| t.storage.as_ref().and_then(|st| st.planner_rows)).filter(|r| *r >= 100_000_000.0)
        else {
            continue;
        };
        let qual = t.qualified();
        let covered_by_sequence = s.operations.sequences.iter().any(|seq| {
            seq.table.as_deref().is_some_and(|tbl| {
                tbl.eq_ignore_ascii_case(&qual) || tbl.rsplit('.').next().unwrap_or(tbl).eq_ignore_ascii_case(&t.name)
            })
        });
        if covered_by_sequence {
            continue;
        }
        let Some(pk) = s.indexes_on(&t.schema, &t.name).find(|ix| ix.is_primary_key && ix.columns.len() == 1) else {
            continue;
        };
        let pk_col_name = culprit_core::strip_direction(&pk.columns[0]);
        let Some(col) = s.columns.iter().find(|c| {
            c.schema.eq_ignore_ascii_case(&t.schema)
                && c.table.eq_ignore_ascii_case(&t.name)
                && c.name.eq_ignore_ascii_case(pk_col_name)
                && is_signed_int32(&c.data_type)
        }) else {
            continue;
        };
        let severity = if rows >= 500_000_000.0 { Severity::High } else { Severity::Medium };
        out.push(NearLimit::Int32Key { table: t, column: col, rows, severity });
    }
    out
}

fn is_signed_int32(data_type: &str) -> bool {
    let lower = data_type.trim().to_ascii_lowercase();
    if lower.contains("unsigned") {
        return false;
    }
    let base = lower.split('(').next().unwrap_or(&lower).trim();
    matches!(base, "int" | "integer" | "int4" | "serial" | "serial4")
}

fn sequence_capacity_finding(s: &Snapshot, case: NearLimit<'_>) -> Finding {
    match case {
        NearLimit::Sequence { sequence: seq, remaining, severity } => {
            let mut f = finding(
                "sequence-capacity",
                &seq.name,
                Category::Maintenance,
                subject(SubjectKind::Database, &seq.name),
            );
            let steps = format::count(remaining as f64);
            f.title = t!("check.sequence_capacity.title", name = &seq.name, steps = &steps);
            f.summary = [
                t!(
                    "check.sequence_capacity.summary.recorded",
                    current = &seq.current,
                    increment = &seq.increment,
                    minimum = &seq.minimum,
                    maximum = &seq.maximum
                ),
                if seq.allocated {
                    t!("check.sequence_capacity.summary.allocated")
                } else {
                    t!("check.sequence_capacity.summary.observed")
                },
                t!("check.sequence_capacity.summary.not_a_forecast"),
            ]
            .join(" ");
            f.severity = severity;
            f.confidence = Confidence::High;
            f.impact.score = base_score(f.severity);
            f.impact.label = t!("check.sequence_capacity.impact", steps = &steps);
            f.metrics = vec![
                Metric::text(t!("check.sequence_capacity.metric.recorded"), &seq.current),
                Metric::text(t!("check.sequence_capacity.metric.minimum"), &seq.minimum),
                Metric::text(t!("check.sequence_capacity.metric.maximum"), &seq.maximum),
                Metric::new(t!("check.sequence_capacity.metric.steps"), remaining as f64, Unit::Count),
            ];
            if let Some(table) = &seq.table {
                f.metrics.push(Metric::text(t!("check.sequence_capacity.metric.table"), table));
            }
            f.fix = Some(Fix {
                summary: t!("check.sequence_capacity.fix"),
                sql: None,
                cost: Some(t!("check.sequence_capacity.fix_cost")),
            });
            f.verify = vec![t!("check.sequence_capacity.verify")];
            f
        }
        NearLimit::Partition(partition) => {
            let mut f = super::table_bloat::expiring_partition_finding(s, partition);
            f.check = "sequence-capacity".into();
            f
        }
        NearLimit::Int32Key { table: t, column: col, rows, severity } => {
            let qual = t.qualified();
            let id_key = format!("int32-pk:{qual}.{}", col.name);
            let mut f =
                finding("sequence-capacity", &id_key, Category::Maintenance, subject(SubjectKind::Table, &qual));
            f.severity = severity;
            f.confidence = Confidence::High;
            f.impact.score = base_score(f.severity);
            let count = format::count(rows);
            f.impact.label = t!("check.sequence_capacity.int32.impact", rows = &count);
            f.title = t!("check.sequence_capacity.int32.title", table = &t.name, column = &col.name, rows = &count);
            f.summary = t!(
                "check.sequence_capacity.int32.summary",
                table = &qual,
                column = &col.name,
                data_type = &col.data_type,
                rows = &count
            );
            f.metrics = vec![
                Metric::new(t!("check.sequence_capacity.int32.metric.rows"), rows, Unit::Rows),
                Metric::text(
                    t!("check.sequence_capacity.int32.metric.column"),
                    format!("{}.{} ({})", t.name, col.name, col.data_type),
                ),
                Metric::text(t!("check.sequence_capacity.int32.metric.limit"), "2,147,483,647"),
            ];
            f.fix = Some(Fix {
                summary: t!("check.sequence_capacity.int32.fix", table = &qual, column = &col.name),
                sql: None,
                cost: Some(t!("check.sequence_capacity.int32.fix_cost")),
            });
            f.verify = vec![t!("check.sequence_capacity.int32.verify", table = &qual, column = &col.name)];
            f
        }
    }
}
