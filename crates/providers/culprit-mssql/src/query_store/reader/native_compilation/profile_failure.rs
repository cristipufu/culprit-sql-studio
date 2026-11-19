//! Failure-only private receipts. Diagnostics use existing accounting and never issue a replacement read.
use super::*;

pub(super) fn save_open_failure(
    fixture: &OwnedSourceFixture,
    directory: &Path,
    name: &str,
    started: Instant,
    error: &culprit_core::Error,
) {
    save(
        directory,
        name,
        &json!({
            "stage":"measurement-open","error":error.code,"error_detail":error.detail,
            "scope":null,"accounting_before_close":null,"accounting_after_close":null,
            "metadata_stages":null,"shutdown":shutdown_state(fixture.pool.source_shutdown()),
            "full_profile_including_preparation_client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
            "limitation":"opening failed before a measured owner was returned; scope and accounting are unavailable, not empty",
            "production_admission":"unqualified","native_cpu_io_cost_qualified":false,
        }),
    );
}

pub(super) async fn close_and_save(
    reader: &mut Reader<MeasuredRead>,
    directory: &Path,
    name: &str,
    started: Instant,
    error: culprit_core::Error,
) -> culprit_core::Error {
    let scope = reader.io.scope().clone();
    let accounting = reader.io.report();
    let metadata = metadata_stages(reader.read_metrics().as_ref());
    let cached_layout = reader.native_layout.map(|layout| {
        json!({
            "edition":layout.edition,"replica_group_column_present":layout.partitioned,
            "tempdb_column_present":layout.tempdb,"log_bytes_column_present":layout.log_bytes,
            "waits_supported":layout.waits,"query_store_mode":format!("{:?}",layout.mode),
        })
    });
    let replica_catalog_cached = reader.replica_catalog.is_some();
    let population_catalog_cached = reader.population_catalog.is_some();
    // Retain and close the same native owner before writing. Closing keeps its existing settlement work;
    // diagnostics add no independent quiet check, catalog discovery, retry or source query.
    let shutdown = reader.close().await;
    let returned = after_shutdown(error.clone(), shutdown);
    save(
        directory,
        name,
        &json!({
            "stage":"native-profile","error":error.code,"error_detail":error.detail,
            "returned_error":returned.code,"returned_error_detail":returned.detail,
            "scope":scope,"source_queries":reader.queries(),
            "accounting_before_close":accounting,"accounting_after_close":reader.io.report(),
            "metadata_stages":metadata,"cached_layout":cached_layout,
            "replica_catalog_cached":replica_catalog_cached,"population_catalog_cached":population_catalog_cached,
            "shutdown":shutdown_state(shutdown),
            "full_profile_including_preparation_and_observer_client_elapsed_ms":started.elapsed().as_secs_f64()*1000.0,
            "limitation":"a completed zero-row catalog is distinct from no completed read; uncached layout does not establish that its query was never executed; no diagnostic SQL or independent quiet sample is added",
            "production_admission":"unqualified","native_cpu_io_cost_qualified":false,
        }),
    );
    returned
}

fn metadata_stages(metrics: Option<&ReadWorkMetrics>) -> Value {
    let stage = |sql: &str| {
        let template_id = digest(sql.as_bytes());
        let metric =
            metrics.and_then(|metrics| metrics.templates.iter().find(|metric| metric.template_id == template_id));
        json!({"template_id":template_id,"read_metrics":metric})
    };
    json!({
        "layout":stage(crate::query_store::runtime::LAYOUT),
        "replica_catalog":stage(crate::query_store::catalog_sql::REPLICAS),
    })
}

fn after_shutdown(error: culprit_core::Error, shutdown: SourceShutdown) -> culprit_core::Error {
    if shutdown == SourceShutdown::Unconfirmed { CollectionStop::CancellationUnconfirmed.error() } else { error }
}

fn save(directory: &Path, name: &str, receipt: &Value) {
    if let Err(error) = save_cost_receipt(directory, &format!("{name}-profile-failure"), receipt) {
        // The native attempt has already failed and settled. Preserve its original failure if the private
        // diagnostic file cannot be written, and make the missing receipt explicit without native details.
        eprintln!("compilation profile failure receipt unavailable: code={:?},detail={:?}", error.code, error.detail);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_empty_catalog_keeps_zero_rows_and_its_exact_native_template() {
        let metric = ReadQueryMetrics {
            template_id: digest(crate::query_store::catalog_sql::REPLICAS.as_bytes()),
            statement: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
            completed_tasks: 1,
            failures: 0,
            timeouts: 0,
            cancellations: 0,
            unconfirmed_cancellations: 0,
            stop_causes: vec![],
            rows_decoded: "0".into(),
            charged_value_bytes: "0".into(),
            client_elapsed_ms: 0.5,
        };
        let metrics = ReadWorkMetrics {
            active_tasks: 0,
            untracked_tasks: 0,
            templates: vec![metric.clone()],
            native_session_work: None,
        };
        let stages = metadata_stages(Some(&metrics));
        assert_eq!(stages["replica_catalog"]["read_metrics"], json!(metric));
        assert!(stages["layout"]["read_metrics"].is_null(), "absent metric is not a successful zero-row read");
        let unknown = metadata_stages(None);
        assert!(unknown["replica_catalog"]["read_metrics"].is_null());
        assert_eq!(unknown["replica_catalog"]["template_id"], stages["replica_catalog"]["template_id"]);
    }

    #[test]
    fn settled_failure_preserves_original_detail_and_unconfirmed_close_remains_distinct() {
        let original = CollectionStop::SourceChanged.error();
        let settled = after_shutdown(original.clone(), SourceShutdown::Quiescent);
        assert_eq!(settled.code, original.code);
        assert_eq!(settled.detail, original.detail);
        let unconfirmed = after_shutdown(original, SourceShutdown::Unconfirmed);
        assert_eq!(unconfirmed.detail, CollectionStop::CancellationUnconfirmed.error().detail);
    }
}
