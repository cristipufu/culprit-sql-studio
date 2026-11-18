//! Owned-fixture observation only: delegates the same source and structurally checks rows with its real lease.
use super::*;
use culprit_core::Error;
use serde_json::{Value, json};
use std::collections::HashSet;

const MAX_FAILURE_SAMPLES: usize = 4;
struct SourceDiagnostics {
    inner: Box<dyn TelemetrySource>,
    capture: CaptureIdentity,
    description: Option<Box<CollectionDescription>>,
    seen: HashSet<NativeRecordIdentity>,
    failure_samples: usize,
}
pub(super) fn source(inner: Box<dyn TelemetrySource>, capture: CaptureIdentity) -> Box<dyn TelemetrySource> {
    Box::new(SourceDiagnostics { inner, capture, description: None, seen: HashSet::new(), failure_samples: 0 })
}
pub(super) fn failed(stage: &str, error: &Error) {
    eprintln!(
        "owned capture boundary failure: {}",
        json!({"stage":stage,"code":error.code,"stop":CollectionStop::from_error(error)})
    );
}
pub(super) fn incomplete(manifest: &CollectionManifest) {
    eprintln!(
        "owned capture incomplete: {}",
        json!({
            "source_queries":manifest.source_queries,
            "source_is_final_snapshot":manifest.description.as_ref().is_some_and(|source|source.observation.snapshot().is_some()),
            "files":manifest.files.iter().map(|file|json!({"stream":file.stream,"status":file.status,"rows":file.rows,"pages":file.pages,"bytes":file.bytes})).collect::<Vec<_>>(),
            "reference_integrity_complete":manifest.references.integrity_complete,
            "read_metrics_valid":manifest.read_metrics.as_ref().map(ReadWorkMetrics::is_valid),
            "native_work_unconfirmed":manifest.read_metrics.as_ref().map(ReadWorkMetrics::has_unconfirmed_work),
        })
    );
}
impl SourceDiagnostics {
    fn inspect(&mut self, records: &[TelemetryRecord]) {
        let Some(source) = self.description.as_deref() else { return };
        for record in records {
            let duplicate = !self.seen.insert(record.native_identity());
            let exported = ExportedRecord::new(source, &self.capture, record.clone());
            if (!duplicate && exported.is_ok()) || self.failure_samples == MAX_FAILURE_SAMPLES {
                continue;
            }
            self.failure_samples += 1;
            let error = exported.as_ref().err();
            eprintln!(
                "owned capture returned-row boundary: {}",
                json!({
                    "sample":self.failure_samples,
                    "duplicate_native_key":duplicate,
                    "export_succeeded":exported.is_ok(),
                    "export_error_code":error.map(|error|error.code),
                    "export_stop":error.map(CollectionStop::from_error),
                    "facts":row_facts(source,record),
                })
            );
        }
    }
}
fn row_facts(source: &CollectionDescription, record: &TelemetryRecord) -> Value {
    let kind = match record {
        TelemetryRecord::ObjectDefinition(row) => match &row.fact {
            ObjectDefinitionFact::MssqlRetainedPlan(_) => "mssql-retained-plan",
            _ => "other-object-definition",
        },
        _ => "other-record",
    };
    let source_fit = match record {
        TelemetryRecord::ObjectDefinition(row) => {
            source.observation.accepts_snapshot_record(record.stream(), &row.source)
        }
        _ => false,
    };
    let retained = match record {
        TelemetryRecord::ObjectDefinition(row) => match &row.fact {
            ObjectDefinitionFact::MssqlRetainedPlan(value) => Some(json!({
                "context_valid":value.context.is_valid(),
                "statement_valid":value.statement.is_valid(),
                "plan_valid":value.batch_plan.is_valid(),
                "has_relational_proof":value.relational_input_proof.is_some(),
                "has_parameter_proof":value.parameter_context_proof.is_some(),
                "proofs_match_source":row.fact.native_plan_proofs_match(source),
                "statement_redaction_stable":stable(&value.statement.payload,DefinitionPayload::sql),
                "plan_redaction_stable":stable(&value.batch_plan.payload,DefinitionPayload::plan),
            })),
            _ => None,
        },
        _ => None,
    };
    json!({
        "kind":kind,
        "stream":record.stream(),
        "source_fit":source_fit,
        "record_valid":record.is_valid(),
        "payloads_valid":record.definition_payloads().all(|field|field.payload.is_valid()),
        "source_partition_consistent":source.partition_is_consistent(),
        "source_partition_qualified":source.is_partition_qualified(),
        "retained":retained,
    })
}
fn stable(payload: &DefinitionPayload, redact: fn(&str, culprit_core::Engine) -> DefinitionPayload) -> Option<bool> {
    match payload {
        DefinitionPayload::Available { text, .. } => Some(&redact(text, culprit_core::Engine::Mssql) == payload),
        DefinitionPayload::Unavailable { .. } => None,
    }
}
#[async_trait::async_trait]
impl TelemetrySource for SourceDiagnostics {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        let description = self.inner.describe().await.inspect_err(|error| failed("source-describe", error))?;
        self.description = Some(Box::new(description.clone()));
        Ok(description)
    }
    fn query_count(&self) -> u32 {
        self.inner.query_count()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.inner.read_metrics()
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        let result = self.inner.read_page(stream, cursor).await.inspect_err(|error| failed("source-page", error));
        if let Ok(CollectionRead::Page { records, .. }) = &result {
            self.inspect(records)
        }
        result
    }
    async fn finish(&mut self) -> Result<SourceCompletion> {
        self.inner.finish().await.inspect_err(|error| failed("source-finish", error))
    }
    async fn close(&mut self) -> SourceShutdown {
        self.inner.close().await
    }
}

#[cfg(test)]
mod tests;
