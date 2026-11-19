//! Project the shared cache-entry explanations without exporting their original SQL or revising pair ownership.
use super::*;
use culprit_core::{CacheEntryContextDifference, CacheEntryMemoryReport};

const MAX_CACHE_ENTRY_METRICS: usize = 3;
const MAX_CACHE_ENTRY_LIMITATIONS: usize = 8;

pub(crate) fn cache_entry_memory(report: &CacheEntryMemoryReport) -> String {
    let title = super::super::brief_detail(&report.title, OBSERVATION_DETAIL_CHARS);
    let summary = super::super::brief_detail(&report.summary, OBSERVATION_DETAIL_CHARS);
    let reason =
        report.source_reason.as_deref().map(|reason| super::super::brief_detail(reason, OBSERVATION_DETAIL_CHARS));
    let limitations = report
        .limitations
        .iter()
        .take(MAX_CACHE_ENTRY_LIMITATIONS)
        .map(|limitation| super::super::brief_detail(limitation, OBSERVATION_DETAIL_CHARS))
        .collect::<Vec<_>>();
    let mut text = native_work_summary(
        Summary {
            title: &title,
            summary: &summary,
            source: SourceQualification::Single { state: report.source_state, reason: reason.as_deref() },
            limitations: &limitations,
        },
        report.entries.iter().map(|entry| {
            let mut fields = vec![entry.explanation.clone()];
            fields.extend(native_metrics(&entry.metrics).take(MAX_CACHE_ENTRY_METRICS));
            fields.extend([entry.investigation.clone(), entry.verify.clone()]);
            (entry.title.as_str(), fields)
        }),
    );
    let comparisons = report.context_differences.iter().filter_map(|difference| comparison(report, difference));
    let comparisons = comparisons.take(super::super::BRIEF_FINDING_LIMIT).collect::<Vec<_>>();
    append_native_work_entries(&mut text, comparisons.iter().map(|(title, fields)| (title.as_str(), fields.clone())));
    append_remaining_limitations(&mut text, &limitations);
    text
}

fn comparison(
    report: &CacheEntryMemoryReport,
    difference: &CacheEntryContextDifference,
) -> Option<(String, Vec<String>)> {
    if difference.left_observation_key == difference.right_observation_key {
        return None;
    }
    let left = displayed_entry_title(report, &difference.left_observation_key)?;
    let right = displayed_entry_title(report, &difference.right_observation_key)?;
    Some((
        t!(
            "evidence.cli.cache_entry_memory.context_pair",
            left = super::super::brief_detail(left, super::super::BRIEF_TITLE_CHARS),
            right = super::super::brief_detail(right, super::super::BRIEF_TITLE_CHARS)
        ),
        vec![difference.explanation.clone(), difference.investigation.clone()],
    ))
}

fn displayed_entry_title<'a>(report: &'a CacheEntryMemoryReport, key: &str) -> Option<&'a str> {
    let mut matches = report.entries.iter().enumerate().filter(|(_, entry)| entry.observation_key == key);
    let (index, entry) = matches.next()?;
    (index < super::super::BRIEF_FINDING_LIMIT && matches.next().is_none()).then_some(entry.title.as_str())
}

#[cfg(test)]
mod tests;
