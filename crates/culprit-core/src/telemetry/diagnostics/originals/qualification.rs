//! One original-input scope policy serves the queue and its source-free qualification disclosure.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum DiagnosticInputIneligibility {
    OriginalSourceUnavailable,
    SharedOwnershipUnavailable,
    SharedOwnerMismatch,
    NativeEpochUnavailable,
    DatabaseReadOwnerUnavailable,
    HistoricalNamespaceUnavailable,
    UnsupportedSource,
    InvalidSource,
}
#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum DiagnosticInputQualification {
    Qualified { source: DiagnosticSourceKind },
    Ineligible { reason: DiagnosticInputIneligibility },
}
impl DiagnosticInputQualification {
    pub fn source(self) -> Option<DiagnosticSourceKind> {
        match self {
            Self::Qualified { source } => Some(source),
            Self::Ineligible { .. } => None,
        }
    }
    pub fn explanation(self) -> String {
        match self {
            Self::Qualified { .. } => t!("view.incident_input.qualified"),
            Self::Ineligible { reason } => match reason {
                DiagnosticInputIneligibility::OriginalSourceUnavailable => {
                    t!("view.incident_input.original_unavailable")
                }
                DiagnosticInputIneligibility::SharedOwnershipUnavailable => {
                    t!("view.incident_input.shared_unavailable")
                }
                DiagnosticInputIneligibility::SharedOwnerMismatch => t!("view.incident_input.shared_mismatch"),
                DiagnosticInputIneligibility::NativeEpochUnavailable => t!("view.incident_input.epoch_unavailable"),
                DiagnosticInputIneligibility::DatabaseReadOwnerUnavailable => {
                    t!("view.incident_input.read_owner_unavailable")
                }
                DiagnosticInputIneligibility::HistoricalNamespaceUnavailable => {
                    t!("view.incident_input.history_namespace_unavailable")
                }
                DiagnosticInputIneligibility::UnsupportedSource => t!("view.incident_input.unsupported"),
                DiagnosticInputIneligibility::InvalidSource => t!("view.incident_input.invalid"),
            },
        }
    }
}
pub fn diagnostic_original_input_qualification(source: Option<&CollectionDescription>) -> DiagnosticInputQualification {
    let result = source.ok_or(DiagnosticInputIneligibility::OriginalSourceUnavailable).and_then(qualify);
    match result {
        Ok(source) => DiagnosticInputQualification::Qualified { source },
        Err(reason) => DiagnosticInputQualification::Ineligible { reason },
    }
}
fn qualify(source: &CollectionDescription) -> std::result::Result<DiagnosticSourceKind, DiagnosticInputIneligibility> {
    use DiagnosticInputIneligibility as Issue;
    if !source.target.is_valid() {
        return Err(Issue::InvalidSource);
    }
    if let SourceObservation::QueryHistory(history) = &source.observation {
        if source.source != crate::source_id::QUERY_STORE || source.target.engine() != crate::Engine::Mssql {
            return Err(Issue::UnsupportedSource);
        }
        if query_store_database_namespace(&source.target, &source.partition).is_none() {
            return Err(Issue::HistoricalNamespaceUnavailable);
        }
        let owner = history.acquisition_owner.as_deref().ok_or(Issue::DatabaseReadOwnerUnavailable)?;
        if !owner.has_qualified_local_owner() || owner.target != source.target {
            return Err(Issue::DatabaseReadOwnerUnavailable);
        }
        return (history.is_valid() && source.partition_is_consistent())
            .then_some(DiagnosticSourceKind::QueryHistory)
            .ok_or(Issue::InvalidSource);
    }
    let snapshot = source.observation.snapshot().ok_or(Issue::UnsupportedSource)?;
    let kind = match snapshot.dataset {
        SnapshotDataset::MemoryFailures if source.target.engine() == crate::Engine::Mssql => {
            DiagnosticSourceKind::MemoryFailures
        }
        SnapshotDataset::NativeFailures
            if matches!(source.target.engine(), crate::Engine::Mysql | crate::Engine::Postgres) =>
        {
            DiagnosticSourceKind::NativeFailures
        }
        SnapshotDataset::Cleanup if source.target.engine() == crate::Engine::Mssql => DiagnosticSourceKind::Cleanup,
        SnapshotDataset::TaskSpace if source.target.engine() == crate::Engine::Mssql => DiagnosticSourceKind::TaskSpace,
        _ => return Err(Issue::UnsupportedSource),
    };
    if !snapshot.is_valid() || !source.partition.is_valid_for(&source.target) {
        return Err(Issue::InvalidSource);
    }
    match (&source.target, kind) {
        (
            CollectionTarget::Shared(owner),
            DiagnosticSourceKind::MemoryFailures | DiagnosticSourceKind::NativeFailures,
        ) => {
            // The existing pool header has no member/process binding. Its native rows remain useful, but a pool
            // name, target-only namespace or instance member does not fill that missing pool authority.
            if owner.kind != SharedResourceKind::Instance {
                return Err(Issue::SharedOwnershipUnavailable);
            }
            let SourcePartition::InstanceLocal { member, epoch } = &source.partition else {
                return Err(Issue::SharedOwnershipUnavailable);
            };
            let member = member.as_ref().ok_or(Issue::SharedOwnershipUnavailable)?;
            if member != owner {
                return Err(Issue::SharedOwnerMismatch);
            }
            if epoch.is_none() {
                return Err(Issue::NativeEpochUnavailable);
            }
        }
        (CollectionTarget::Database(_), DiagnosticSourceKind::Cleanup | DiagnosticSourceKind::TaskSpace) => {
            let SourcePartition::InstanceLocal { member: Some(member), epoch } = &source.partition else {
                return Err(Issue::DatabaseReadOwnerUnavailable);
            };
            if member.kind != SharedResourceKind::Instance {
                return Err(Issue::DatabaseReadOwnerUnavailable);
            }
            if epoch.is_none() {
                return Err(Issue::NativeEpochUnavailable);
            }
        }
        (_, DiagnosticSourceKind::MemoryFailures | DiagnosticSourceKind::NativeFailures) => {
            return Err(Issue::SharedOwnershipUnavailable);
        }
        _ => return Err(Issue::DatabaseReadOwnerUnavailable),
    }
    source.partition_is_consistent().then_some(kind).ok_or(Issue::InvalidSource)
}
