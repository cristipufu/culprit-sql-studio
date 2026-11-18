//! Read-only witnesses for a planner refusal, never a second planner or native owner lookup.
use super::*;
use serde::Serialize;

#[derive(Serialize)]
struct PlannerRefusalReceipt<'a> {
    phase: &'static str,
    planner_issue: DiagnosticAnalysisIssue,
    original_read_complete: bool,
    original_scope_qualified: bool,
    page_scope_qualified: bool,
    original_epoch_present: bool,
    original_observed_from: &'a str,
    original_observed_at: &'a str,
    original_observed_until: &'a str,
    original_request: Option<OriginalRequestWitness>,
    raw_current_request_token: TokenComparison,
    qualified_page_statement_token: TokenComparison,
    statement_payload: StatementPayloadWitness,
    page_resolution: PageWaitResolution,
    page_observed_from: &'a str,
    page_observed_until: &'a str,
    page_source_coverage: Option<&'a [StateSourceCoverage]>,
    before_check: Option<&'a PageWaitRecheck>,
    after_check: Option<&'a PageWaitRecheck>,
    /// These are the retained native columns, including NULL, zero and sentinel values. A diagnostic
    /// resource suffix does not fill these fields or establish a resource owner.
    native_task: Option<&'a MssqlPageTaskWait>,
}

#[derive(Serialize)]
struct OriginalRequestWitness {
    receipt_present: bool,
    statement_token_present: bool,
    scope: ActivityScopeBasis,
    generation_policy: GenerationPolicyWitness,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum GenerationPolicyWitness {
    RecordedRefusal,
    /// The existing whole-frame policy has no recorded refusal; this is not new establishment proof.
    NoRecordedRefusal,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum TokenComparison {
    BothAbsent,
    OriginalAbsent,
    CurrentAbsent,
    Equal,
    Different,
}

impl TokenComparison {
    fn new(original: Option<&NativeIdentity>, current: Option<&NativeIdentity>) -> Self {
        match (original, current) {
            (None, None) => Self::BothAbsent,
            (None, Some(_)) => Self::OriginalAbsent,
            (Some(_), None) => Self::CurrentAbsent,
            (Some(original), Some(current)) if original == current => Self::Equal,
            (Some(_), Some(_)) => Self::Different,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
enum StatementPayloadWitness {
    Available,
    Unavailable { reason: PayloadUnavailable },
}

impl<'a> PlannerRefusalReceipt<'a> {
    fn from_retained(
        frame: &'a ActivityFrameEvidence<'_>,
        page_source: &'a CollectionDescription,
        page: &'a PageWaitObservation,
        planner_issue: DiagnosticAnalysisIssue,
    ) -> Self {
        let original = frame.row(&page.request.key);
        let original_token = original.and_then(|row| row.observation.statement_identity.as_ref());
        Self {
            phase: "owned-native-task-planner-refusal",
            planner_issue,
            original_read_complete: frame.read_complete(),
            original_scope_qualified: frame.context().has_qualified_local_owner(),
            page_scope_qualified: page_source.is_partition_qualified()
                && matches!(page_source.partition, SourcePartition::InstanceLocal { .. }),
            original_epoch_present: frame.native().epoch.is_some(),
            original_observed_from: &frame.native().observed_from,
            original_observed_at: &frame.native().at,
            original_observed_until: &frame.native().observed_until,
            original_request: original.map(|row| OriginalRequestWitness {
                receipt_present: row.reference.is_some(),
                statement_token_present: row.observation.statement_identity.is_some(),
                scope: row.observation.scope,
                generation_policy: if frame.has_session_generation_authority(&row.observation.key.session) {
                    GenerationPolicyWitness::NoRecordedRefusal
                } else {
                    GenerationPolicyWitness::RecordedRefusal
                },
            }),
            raw_current_request_token: TokenComparison::new(original_token, page.request.statement_identity.as_ref()),
            qualified_page_statement_token: TokenComparison::new(original_token, page.statement_identity.as_ref()),
            statement_payload: match &page.request.statement {
                DefinitionPayload::Available { .. } => StatementPayloadWitness::Available,
                DefinitionPayload::Unavailable { reason } => StatementPayloadWitness::Unavailable { reason: *reason },
            },
            page_resolution: page.resolution,
            page_observed_from: &page.observed_from,
            page_observed_until: &page.observed_until,
            page_source_coverage: page_source.observation.snapshot().map(|snapshot| snapshot.sources.as_slice()),
            before_check: page.before_check.as_ref(),
            after_check: page.after_check.as_ref(),
            native_task: page.task_wait.as_ref(),
        }
    }

    fn count_missing_prerequisites(&self, outcomes: &mut BTreeMap<String, u32>) {
        let original = self.original_request.as_ref();
        // These are independent observed prerequisites, not a guessed explanation for why the source
        // withheld its qualified token. The original checks and native task remain alongside them.
        for (key, missing) in [
            ("original-scope", !self.original_scope_qualified),
            ("current-page-scope", !self.page_scope_qualified),
            ("original-request-key", original.is_none()),
            ("original-receipt", original.is_some_and(|row| !row.receipt_present)),
            ("original-statement-token", original.is_some_and(|row| !row.statement_token_present)),
            ("original-epoch", !self.original_epoch_present),
            (
                "original-generation-policy",
                original.is_some_and(|row| row.generation_policy == GenerationPolicyWitness::RecordedRefusal),
            ),
            (
                "current-page-statement-token",
                matches!(
                    self.qualified_page_statement_token,
                    TokenComparison::BothAbsent | TokenComparison::CurrentAbsent
                ),
            ),
        ] {
            if missing {
                *outcomes.entry(format!("missing-identity:{key}")).or_default() += 1;
            }
        }
    }
}

pub(super) fn note(
    outcomes: &mut BTreeMap<String, u32>,
    frame: &ActivityFrameEvidence<'_>,
    page_source: &CollectionDescription,
    page: &PageWaitObservation,
    planner_issue: DiagnosticAnalysisIssue,
) {
    let Some(receipt) = next_receipt(outcomes, frame, page_source, page, planner_issue) else { return };
    // No SQL, statement token, original request key or authentication receipt is serialized. The same
    // max-four record now retains the actual source rechecks/clocks and native nullable holder fields.
    eprintln!("{}", serde_json::json!(receipt));
}

fn next_receipt<'a>(
    outcomes: &mut BTreeMap<String, u32>,
    frame: &'a ActivityFrameEvidence<'_>,
    page_source: &'a CollectionDescription,
    page: &'a PageWaitObservation,
    planner_issue: DiagnosticAnalysisIssue,
) -> Option<PlannerRefusalReceipt<'a>> {
    let receipt = PlannerRefusalReceipt::from_retained(frame, page_source, page, planner_issue);
    receipt.count_missing_prerequisites(outcomes);
    let samples = outcomes.entry("planner-refusal-diagnostic-samples".into()).or_default();
    if *samples >= MAX_PLANNER_DIAGNOSTIC_SAMPLES {
        return None;
    }
    *samples += 1;
    Some(receipt)
}

#[cfg(test)]
mod tests;
