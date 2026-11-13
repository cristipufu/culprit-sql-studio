//! One inventory of definition payloads and their locations in the native export contract.
use super::*;

pub struct DefinitionReference<'a> {
    pub pointer: &'static str,
    pub payload: &'a DefinitionPayload,
}
fn reference<'a>(pointer: &'static str, payload: &'a DefinitionPayload) -> DefinitionReference<'a> {
    DefinitionReference { pointer, payload }
}
impl TelemetryRecord {
    pub fn definition_payloads(&self) -> impl Iterator<Item = DefinitionReference<'_>> {
        let payload = match self {
            Self::QueryDefinition(row) => Some(reference("/record/data/sql", &row.sql)),
            Self::PlanDefinition(row) => Some(reference("/record/data/plan", &row.plan)),
            Self::Activity(row) => Some(reference("/record/data/statement", &row.statement)),
            Self::Incident(row) => {
                Some(reference("/record/data/state/observation/statement", &row.state.observation.statement))
            }
            Self::ActivityCheckpoint(row) => {
                Some(reference("/record/data/observation/statement", &row.observation.statement))
            }
            Self::Operational(row) => return row.fact.definition_fields().into_iter().flatten(),
            Self::Tuning(row) => return row.fact.definition_fields().into_iter().flatten(),
            Self::ObjectDefinition(row) => return row.fact.definition_fields().into_iter().flatten(),
            Self::QueryRuntime(_)
            | Self::QueryWaits(_)
            | Self::ResourceSample(_)
            | Self::Blocking(_)
            | Self::Deadlock(_)
            | Self::WaitCounter(_)
            | Self::Replication(_)
            | Self::Setting(_) => None,
        };
        [payload, None].into_iter().flatten()
    }
}
impl ObjectDefinitionFact {
    pub fn definition_payloads(&self) -> impl Iterator<Item = DefinitionReference<'_>> {
        self.definition_fields().into_iter().flatten()
    }
    fn definition_fields(&self) -> [Option<DefinitionReference<'_>>; 2] {
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
                ];
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
                ];
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
                ];
            }
            Self::RetainedPlan(value) => {
                Some(reference("/record/data/fact/data/definition/plan", &value.definition.plan))
            }
            Self::MssqlRetainedPlan(value) => {
                Some(reference("/record/data/fact/data/batch_plan/payload", &value.batch_plan.payload))
            }
            Self::MssqlForeignKey(_) | Self::MssqlObject(_) | Self::MssqlDmlTrigger(_) => None,
        };
        [payload, None]
    }
}
impl OperationalFact {
    fn definition_fields(&self) -> [Option<DefinitionReference<'_>>; 2] {
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
                ];
            }
            Self::MssqlIndex(value) => {
                value.filter.as_ref().map(|value| reference("/record/data/fact/data/filter/payload", &value.payload))
            }
            Self::ExecutionGrant(value) => value
                .request
                .as_ref()
                .map(|request| reference("/record/data/fact/data/request/statement", &request.statement)),
            Self::Space(_)
            | Self::Sequence(_)
            | Self::FileIo(_)
            | Self::Counter(_)
            | Self::StatementCounters(_)
            | Self::CounterSet(_)
            | Self::Retention(_)
            | Self::RetentionCoverage(_)
            | Self::Queue(_)
            | Self::MssqlCleanupHorizon(_)
            | Self::MssqlPersistentVersionStore(_)
            | Self::MssqlStatementGrants(_)
            | Self::MssqlIndexOperations(_)
            | Self::MssqlTable(_)
            | Self::MssqlStatistics(_)
            | Self::MssqlMissingIndex(_)
            | Self::MssqlQueryCompilation(_)
            | Self::MssqlCachedRuntime(_)
            | Self::MssqlOutOfMemory(_)
            | Self::SuspectPages { .. } => None,
        };
        [payload, None]
    }
}
impl TuningFact {
    fn definition_fields(&self) -> [Option<DefinitionReference<'_>>; 2] {
        let payload = match self {
            Self::Recommendation(value) => Some(reference("/record/data/fact/data/script", &value.script)),
            Self::QueryHint(value) => Some(reference("/record/data/fact/data/hint", &value.hint)),
            Self::AutomaticOption(_) | Self::ForcedPlan(_) => None,
        };
        [payload, None]
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
            assert_eq!(value.pointer(field.pointer).unwrap(), &serde_json::to_value(field.payload).unwrap());
        }
    }
}
