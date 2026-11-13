//! Native resource work has no inferred elapsed share, allocating operator or required memory limit.
use culprit_core::{Metric, StatementTemporaryWork, TemporaryWorkExplanation, Unit, format, key, t};

pub struct TemporaryWorkEvidence<'a>(&'a StatementTemporaryWork);

pub fn evidence(work: &StatementTemporaryWork) -> Option<TemporaryWorkEvidence<'_>> {
    work.has_measurements().then_some(TemporaryWorkEvidence(work))
}

pub fn explain(evidence: TemporaryWorkEvidence<'_>) -> TemporaryWorkExplanation {
    let mut metrics = Vec::new();
    let (title, summary, qualification) = match evidence.0 {
        StatementTemporaryWork::BlockIo {
            bytes_per_block,
            read_blocks,
            written_blocks,
            read_time_ms,
            write_time_ms,
            ..
        } => {
            count(&mut metrics, key!("view.temporary_work.read_blocks"), read_blocks);
            count(&mut metrics, key!("view.temporary_work.written_blocks"), written_blocks);
            if let Some(bytes) = bytes_per_block {
                metrics.push(Metric::said(key!("view.temporary_work.block_bytes"), f64::from(*bytes), Unit::Bytes));
            }
            for (value, label) in [
                (read_time_ms, key!("view.temporary_work.read_time")),
                (write_time_ms, key!("view.temporary_work.write_time")),
            ] {
                if let Some(value) = value {
                    metrics.push(Metric::said(label, *value, Unit::Ms));
                }
            }
            (
                t!("view.temporary_work.blocks.title"),
                t!("view.temporary_work.blocks.summary"),
                t!("view.temporary_work.blocks.limit"),
            )
        }
        StatementTemporaryWork::TableCreation { created_tables, disk_tables, sort_merge_passes, .. } => {
            count(&mut metrics, key!("view.temporary_work.created_tables"), created_tables);
            count(&mut metrics, key!("view.temporary_work.disk_tables"), disk_tables);
            count(&mut metrics, key!("view.temporary_work.merge_passes"), sort_merge_passes);
            (
                t!("view.temporary_work.tables.title"),
                t!("view.temporary_work.tables.summary"),
                t!("view.temporary_work.tables.limit"),
            )
        }
        StatementTemporaryWork::SpaceUsage { bytes_per_page, usage_pages_total, .. } => {
            metrics.push(Metric::said(key!("view.temporary_work.usage_pages"), *usage_pages_total, Unit::Pages));
            metrics.push(Metric::said(key!("view.temporary_work.page_bytes"), f64::from(*bytes_per_page), Unit::Bytes));
            (
                t!("view.temporary_work.space.title"),
                t!("view.temporary_work.space.summary"),
                t!("view.temporary_work.space.limit"),
            )
        }
    };
    TemporaryWorkExplanation {
        title,
        summary,
        metrics,
        limitations: vec![qualification, t!("view.temporary_work.limit.cause")],
        investigation: t!("view.temporary_work.investigation"),
        verify: t!("view.temporary_work.verify"),
    }
}

fn count(metrics: &mut Vec<Metric>, key: &str, value: &Option<String>) {
    if let Some(value) = value {
        metrics.push(Metric::text(culprit_core::i18n::t(key, &[]), format::digits(value.clone())).keyed(key));
    }
}

#[cfg(test)]
mod tests;
