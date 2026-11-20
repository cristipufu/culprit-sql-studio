//! One inventory of definition payloads and their locations in the native export contract.
use super::*;
use std::borrow::Cow;

pub struct DefinitionReference<'a> {
    pub pointer: Cow<'static, str>,
    pub payload: &'a DefinitionPayload,
}
fn reference<'a>(pointer: impl Into<Cow<'static, str>>, payload: &'a DefinitionPayload) -> DefinitionReference<'a> {
    DefinitionReference { pointer: pointer.into(), payload }
}
impl TelemetryRecord {
    pub fn definition_payloads(&self) -> impl Iterator<Item = DefinitionReference<'_>> {
        let payload = match self {
            Self::QueryDefinition(row) => Some(reference("/record/data/sql", &row.sql)),
            Self::PlanDefinition(row) => Some(reference("/record/data/plan", &row.plan)),
            Self::Activity(row) => Some(reference("/record/data/statement", &row.statement)),
            Self::Operational(row) => return row.fact.definition_fields().into_iter(),
            Self::Tuning(row) => return row.fact.definition_fields().into_iter(),
            Self::ObjectDefinition(row) => return row.fact.definition_fields().into_iter(),
            Self::QueryRuntime(_)
            | Self::QueryWaits(_)
            | Self::ResourceSample(_)
            | Self::DiagnosticCheckpoint(_)
            | Self::Deadlock(_)
            | Self::WaitCounter(_)
            | Self::Replication(_)
            | Self::Setting(_) => None,
        };
        payload.into_iter().collect::<Vec<_>>().into_iter()
    }
}
impl ObjectDefinitionFact {
    pub fn definition_payloads(&self) -> impl Iterator<Item = DefinitionReference<'_>> {
        self.definition_fields().into_iter()
    }
    fn definition_fields(&self) -> Vec<DefinitionReference<'_>> {
        let payload = match self {
            Self::NativeCatalog(catalog) => {
                return [
                    catalog
                        .definition
                        .as_ref()
                        .map(|value| reference("/record/data/fact/data/definition/payload", &value.payload)),
                    catalog
                        .predicate
                        .as_ref()
                        .map(|value| reference("/record/data/fact/data/predicate/payload", &value.payload)),
                ]
                .into_iter()
                .flatten()
                .collect();
            }
            Self::Statement(statement) => Some(reference("/record/data/fact/data/sql", &statement.sql)),
            Self::MssqlColumn(column) => {
                return [
                    column
                        .default
                        .as_ref()
                        .map(|value| reference("/record/data/fact/data/default/payload", &value.payload)),
                    column
                        .computed
                        .as_ref()
                        .map(|value| reference("/record/data/fact/data/computed/payload", &value.payload)),
                ]
                .into_iter()
                .flatten()
                .collect();
            }
            Self::MssqlCheckConstraint(value) => {
                Some(reference("/record/data/fact/data/definition/payload", &value.definition.payload))
            }
            Self::MssqlPartitionBoundary(value) => value
                .display_value
                .as_ref()
                .map(|value| reference("/record/data/fact/data/display_value/payload", &value.payload)),
            Self::MssqlCachedDefinition(value) => {
                return [
                    Some(reference("/record/data/fact/data/statement/payload", &value.statement.payload)),
                    Some(reference("/record/data/fact/data/estimated_plan/payload", &value.estimated_plan.payload)),
                ]
                .into_iter()
                .flatten()
                .collect();
            }
            Self::RetainedPlan(value) => {
                Some(reference("/record/data/fact/data/definition/plan", &value.definition.plan))
            }
            Self::MssqlRetainedPlan(value) => {
                return [
                    Some(reference("/record/data/fact/data/statement/payload", &value.statement.payload)),
                    Some(reference("/record/data/fact/data/batch_plan/payload", &value.batch_plan.payload)),
                ]
                .into_iter()
                .flatten()
                .collect();
            }
            Self::MssqlForeignKey(_) | Self::MssqlObject(_) | Self::MssqlDmlTrigger(_) => None,
        };
        payload.into_iter().collect()
    }
}
impl OperationalFact {
    fn definition_fields(&self) -> Vec<DefinitionReference<'_>> {
        let payload = match self {
            Self::CatalogCounters(value) => {
                return [
                    value
                        .catalog
                        .definition
                        .as_ref()
                        .map(|value| reference("/record/data/fact/data/catalog/definition/payload", &value.payload)),
                    value
                        .catalog
                        .predicate
                        .as_ref()
                        .map(|value| reference("/record/data/fact/data/catalog/predicate/payload", &value.payload)),
                ]
                .into_iter()
                .flatten()
                .collect();
            }
            Self::MssqlCacheEntryMemory(value) => {
                return value
                    .statements
                    .iter()
                    .enumerate()
                    .map(|(index, statement)| {
                        reference(format!("/record/data/fact/data/statements/{index}/statement"), &statement.statement)
                    })
                    .collect();
            }
            Self::MssqlIndex(value) => {
                value.filter.as_ref().map(|value| reference("/record/data/fact/data/filter/payload", &value.payload))
            }
            Self::ExecutionGrant(value) => value
                .request
                .as_ref()
                .map(|request| reference("/record/data/fact/data/request/statement", &request.statement)),
            Self::TaskSpace(value) => value
                .request
                .as_ref()
                .map(|request| reference("/record/data/fact/data/request/statement", &request.statement)),
            Self::PageWait(value) => {
                Some(reference("/record/data/fact/data/request/statement", &value.request.statement))
            }
            Self::PageTaskHolders(value) => {
                let mut fields = Vec::new();
                if let Some(page) = &value.page {
                    fields.push(reference("/record/data/fact/data/page/request/statement", &page.request.statement));
                }
                for (bound, point) in [("before", &value.before), ("after", &value.after)] {
                    if let Some(request) = &point.holder.request {
                        fields.push(reference(
                            format!("/record/data/fact/data/{bound}/holder/request/statement"),
                            &request.statement,
                        ));
                    }
                }
                return fields;
            }
            Self::PageWaitWriters(value) => {
                let mut fields = Vec::new();
                if let Some(page) = &value.page {
                    fields.push(reference("/record/data/fact/data/page/request/statement", &page.request.statement));
                }
                for (index, candidate) in value.candidates.iter().enumerate() {
                    fields.push(reference(format!("/record/data/fact/data/candidates/{index}/plan"), &candidate.plan));
                    if let Some(request) = &candidate.request {
                        fields.push(reference(
                            format!("/record/data/fact/data/candidates/{index}/request/statement"),
                            &request.statement,
                        ));
                    }
                }
                return fields;
            }
            Self::CompilerWaiter(value) => {
                Some(reference("/record/data/fact/data/request/statement", &value.request.statement))
            }
            Self::PostgresLog(value) => Some(reference("/record/data/fact/data/statement", &value.statement)),
            Self::CurrentRequestWork(value) => Some(reference("/record/data/fact/data/statement", &value.statement)),
            Self::MssqlStatementGrants(value) => Some(reference("/record/data/fact/data/statement", &value.statement)),
            Self::MssqlQueryCompilation(value) => Some(reference("/record/data/fact/data/statement", &value.statement)),
            Self::MssqlInstanceResources(_)
            | Self::MssqlDatabaseFile(_)
            | Self::Space(_)
            | Self::MaintenanceProgress(_)
            | Self::MysqlScheduledEvent(_)
            | Self::MssqlAgentJob(_)
            | Self::MssqlAgentStep(_)
            | Self::MssqlAgentHistory(_)
            | Self::MssqlAgentContext(_)
            | Self::TimescaleJobState(_)
            | Self::TimescaleJobRun(_)
            | Self::TimescaleContinuousAggregate(_)
            | Self::SessionMemory(_)
            | Self::Sequence(_)
            | Self::FileIo(_)
            | Self::Counter(_)
            | Self::StatementCounters(_)
            | Self::RuntimeDistribution(_)
            | Self::DiagnosticRevision(_)
            | Self::CounterSet(_)
            | Self::Retention(_)
            | Self::RetentionCoverage(_)
            | Self::Queue(_)
            | Self::MssqlCdc(_)
            | Self::MssqlBackupHistory(_)
            | Self::MssqlColumnstoreRowgroup(_)
            | Self::MssqlTableMemory(_)
            | Self::MssqlFullTextPopulation(_)
            | Self::MssqlCleanupHorizon(_)
            | Self::ConfiguredLogRateLimits(_)
            | Self::HyperscaleLogComponent(_)
            | Self::DatabaseProperties(_)
            | Self::MssqlPersistentVersionStore(_)
            | Self::MssqlIndexOperations(_)
            | Self::MssqlTable(_)
            | Self::MssqlStatistics(_)
            | Self::MssqlMissingIndex(_)
            | Self::MssqlCachedRuntime(_)
            | Self::MssqlOutOfMemory(_)
            | Self::MysqlErrorLog(_)
            | Self::SuspectPages(_) => None,
        };
        payload.into_iter().collect()
    }
}
impl TuningFact {
    fn definition_fields(&self) -> Vec<DefinitionReference<'_>> {
        let payload = match self {
            Self::Recommendation(value) => Some(reference("/record/data/fact/data/script", &value.script)),
            Self::QueryHint(value) => Some(reference("/record/data/fact/data/hint", &value.hint)),
            Self::AutomaticOption(_) | Self::ForcedPlan(_) => None,
        };
        payload.into_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn named_payloads_resolve_to_the_exact_native_value_without_inspecting_arbitrary_json_fields() {
        let column =
            serde_json::from_str::<MssqlColumnDefinition>(include_str!("../../../../fixtures/telemetry-column.json"))
                .unwrap();
        let record = TelemetryRecord::ObjectDefinition(
            ObjectDefinitionRecord::new("columns".into(), ObjectDefinitionFact::MssqlColumn(Box::new(column))).unwrap(),
        );
        let value = serde_json::json!({"record":record});
        let fields = record.definition_payloads().collect::<Vec<_>>();
        assert_eq!(fields.len(), 1);
        for field in fields {
            assert_eq!(value.pointer(field.pointer.as_ref()).unwrap(), &serde_json::to_value(field.payload).unwrap());
        }
    }
}
