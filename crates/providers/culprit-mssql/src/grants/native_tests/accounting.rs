//! The owned small-cache fixture requires the actual bounded dispatch roster, including native plan binding.
use crate::{cached_statements, read::ReadSession};
use culprit_core::{Result, SnapshotOptions, telemetry::*};
use serde::Serialize;
use std::collections::BTreeSet;

#[derive(Serialize)]
pub(super) struct ExpectedRead {
    role: &'static str,
    template_id: String,
}
fn templates() -> [(&'static str, String); 4] {
    [
        ("summary", crate::cached_runtime::summary_sql()),
        ("planned-payload", cached_statements::detail_sql(true, true)),
        ("text-only-payload", cached_statements::detail_sql(false, true)),
        ("independent-binding", crate::relational_inputs::binding_check_sql().into()),
    ]
}
pub(super) fn expected_reads() -> [ExpectedRead; 4] {
    templates().map(|(role, sql)| ExpectedRead { role, template_id: digest(sql.as_bytes()) })
}
pub(super) fn validate(
    metrics: Option<&ReadWorkMetrics>,
    queries: u32,
    selected_queries: usize,
    options: &SnapshotOptions,
) -> Result<()> {
    validate_payload_shapes(selected_queries, options)?;
    let metrics = metrics.ok_or_else(invalid)?;
    let expected = expected_reads().map(|read| read.template_id).into_iter().collect::<BTreeSet<_>>();
    if !metrics.is_valid()
        || metrics.has_unconfirmed_work()
        || metrics.untracked_tasks != 0
        || queries as usize != expected.len()
        || metrics.templates.len() != expected.len()
        || metrics.templates.iter().any(|metric| {
            !expected.contains(&metric.template_id)
                || metric.completed_tasks != 1
                || metric.failures != 0
                || metric.timeouts != 0
                || metric.cancellations != 0
        })
    {
        return Err(invalid());
    }
    Ok(())
}
fn validate_payload_shapes(selected_queries: usize, options: &SnapshotOptions) -> Result<()> {
    let planned = (options.plans as usize).min(selected_queries);
    let text_only = selected_queries - planned;
    let budget = ReadSession::application_budget();
    if selected_queries > options.top_queries as usize
        || planned == 0
        || text_only == 0
        || planned > cached_statements::detail_selectors_per_read(&budget, true, true)?
        || text_only > cached_statements::detail_selectors_per_read(&budget, false, true)?
    {
        return Err(invalid());
    }
    Ok(())
}
fn invalid() -> culprit_core::Error {
    CollectionStop::InvalidPage.error()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn options() -> SnapshotOptions {
        SnapshotOptions { top_queries: 50, plans: 2, ..Default::default() }
    }
    fn metrics() -> ReadWorkMetrics {
        let recorder = ReadMetricsRecorder::new(culprit_core::Engine::Mssql);
        for (_, sql) in templates() {
            let template = recorder.begin(&sql);
            recorder.finish(template.as_deref(), Duration::ZERO, &ReadProgress::default(), &ReadTaskOutcome::default());
        }
        recorder.snapshot().unwrap()
    }

    #[test]
    fn the_small_cache_roster_includes_the_independent_binding_without_repeating_payload_shapes() {
        let observed = metrics();
        validate(Some(&observed), expected_reads().len() as u32, 3, &options()).unwrap();
        for change in ["missing-binding", "foreign-template", "repeated-payload", "failed", "unconfirmed", "active"] {
            let mut altered = observed.clone();
            let mut queries = expected_reads().len() as u32;
            match change {
                "missing-binding" => {
                    let binding = digest(crate::relational_inputs::binding_check_sql().as_bytes());
                    altered.templates.retain(|template| template.template_id != binding);
                    queries -= 1;
                }
                "foreign-template" => altered.templates[0].template_id = digest(b"unrelated-native-template"),
                "repeated-payload" => {
                    altered.templates[0].completed_tasks += 1;
                    queries += 1;
                }
                "failed" => altered.templates[0].failures = 1,
                "unconfirmed" => {
                    altered.templates[0].failures = 1;
                    altered.templates[0].unconfirmed_cancellations = 1;
                }
                _ => altered.active_tasks = 1,
            }
            assert!(validate(Some(&altered), queries, 3, &options()).is_err(), "{change}");
        }
        assert!(validate(None, expected_reads().len() as u32, 3, &options()).is_err());
    }

    #[test]
    fn caller_shape_bounds_reuse_the_existing_application_payload_budget() {
        validate_payload_shapes(50, &options()).unwrap();
        assert!(validate_payload_shapes(2, &options()).is_err(), "the literal-marker control needs a text-only member");
        assert!(validate_payload_shapes(51, &options()).is_err());
        let budget = ReadSession::application_budget();
        let planned_limit = cached_statements::detail_selectors_per_read(&budget, true, true).unwrap();
        let text_limit = cached_statements::detail_selectors_per_read(&budget, false, true).unwrap();
        let planned =
            SnapshotOptions { plans: (planned_limit + 1) as u32, top_queries: (planned_limit + 2) as u32, ..options() };
        assert!(validate_payload_shapes(planned_limit + 2, &planned).is_err());
        let text = SnapshotOptions { plans: 1, top_queries: (text_limit + 2) as u32, ..options() };
        assert!(validate_payload_shapes(text_limit + 2, &text).is_err());
    }

    #[test]
    fn failure_accounting_retains_exact_template_counts_without_serializing_source_sql() {
        let metrics = metrics();
        let summary = metrics.summary(None).unwrap();
        let diagnostic = serde_json::json!({"expected_reads":expected_reads(),"read_work":summary});
        let encoded = serde_json::to_string(&diagnostic).unwrap();
        for (role, sql) in templates() {
            assert!(encoded.contains(role));
            assert!(encoded.contains(&digest(sql.as_bytes())));
            assert!(!encoded.contains(&sql));
        }
        assert_eq!(diagnostic["read_work"]["templates"].as_array().unwrap().len(), expected_reads().len());
        assert!(
            diagnostic["read_work"]["templates"].as_array().unwrap().iter().all(|template| {
                template["completed_tasks"] == 1 && template["statement"]["state"] == "unavailable"
            })
        );
    }
}
