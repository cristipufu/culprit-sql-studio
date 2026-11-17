//! Optional SQL explains this historical query incarnation, never an operator or current owner.
use super::*;
use crate::views::query_store_definitions::{QueryStoreDefinitionBinding, StatementEvidence};
use culprit_core::query_store_statement_report::*;

pub(super) fn statement(binding: QueryStoreDefinitionBinding<'_>) -> Box<QueryStoreStatementExplanation> {
    let qualification = match &binding {
        QueryStoreDefinitionBinding::Matched(_) => t!("view.page_server_reads.statement.matched"),
        QueryStoreDefinitionBinding::NotObserved => t!("view.page_server_reads.statement.not_observed"),
        QueryStoreDefinitionBinding::SourceUnverified => t!("view.page_server_reads.statement.unverified"),
        QueryStoreDefinitionBinding::SourceChanged => t!("view.page_server_reads.statement.changed"),
        QueryStoreDefinitionBinding::IdentityUnavailable => t!("view.page_server_reads.statement.identity_unavailable"),
    };
    let state = binding.state();
    let missing = culprit_core::telemetry::DefinitionPayload::Unavailable {
        reason: culprit_core::telemetry::PayloadUnavailable::NotCollected,
    };
    let evidence = match binding {
        QueryStoreDefinitionBinding::Matched(row) => StatementEvidence {
            binding: state,
            payload: &row.definition.sql,
            origin: row.definition.origin,
            monitoring_template_id: row.definition.monitoring_template_id.clone(),
            definition: Some(row.reference.clone()),
        },
        _ => StatementEvidence {
            binding: state,
            payload: &missing,
            origin: culprit_core::telemetry::StatementOrigin::Unknown,
            monitoring_template_id: None,
            definition: None,
        },
    };
    let origin = match evidence.origin {
        culprit_core::telemetry::StatementOrigin::Application => {
            t!("view.page_server_reads.statement.origin.application")
        }
        culprit_core::telemetry::StatementOrigin::Culprit
        | culprit_core::telemetry::StatementOrigin::CulpritMonitoring => {
            t!("view.page_server_reads.statement.origin.culprit")
        }
        culprit_core::telemetry::StatementOrigin::EngineInternal => {
            t!("view.page_server_reads.statement.origin.internal")
        }
        culprit_core::telemetry::StatementOrigin::Unknown => t!("view.page_server_reads.statement.origin.unknown"),
    };
    Box::new(evidence.explanation(format!("{qualification} {origin}")))
}

pub(super) fn append_limitations(read: &QueryStoreDefinitionRead, limitations: &mut Vec<String>) {
    limitations.extend([
        t!("view.page_server_reads.statement.limit.selection"),
        t!("view.page_server_reads.statement.limit.continuity"),
    ]);
    match read.namespace {
        QueryStoreDefinitionNamespace::Matched => (),
        QueryStoreDefinitionNamespace::Unverified => {
            limitations.push(t!("view.page_server_reads.statement.unverified"))
        }
        QueryStoreDefinitionNamespace::Changed => limitations.push(t!("view.page_server_reads.statement.changed")),
    }
    if !read.read_complete {
        limitations.push(t!("view.page_server_reads.statement.limit.partial"));
    }
}
