//! Native execution cost has a resource scope; a shared snapshot never invents an activity population.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum RecipeCostScope {
    Activity(DiagnosticScope),
    NativeRead(NativeSourceReadScope),
    SharedResource { target: SharedResourceIdentity, partition: SourcePartition, source: String },
}

impl From<DiagnosticScope> for RecipeCostScope {
    fn from(scope: DiagnosticScope) -> Self {
        Self::Activity(scope)
    }
}

impl RecipeCostScope {
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Activity(scope) => scope.is_valid() && scope.target.database().is_some(),
            Self::NativeRead(scope) => scope.is_valid(),
            Self::SharedResource { target, partition, source } => {
                target.is_valid()
                    && valid_key(source)
                    && partition.is_valid_for(&CollectionTarget::Shared(target.clone()))
                    && matches!(partition, SourcePartition::InstanceLocal { member: Some(member), epoch: Some(_) }
                        if member == target)
            }
        }
    }

    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        hash(&("recipe-cost-scope-v1", self))
    }

    pub fn target(&self) -> CollectionTarget {
        match self {
            Self::Activity(scope) => scope.target.clone(),
            Self::NativeRead(scope) => CollectionTarget::Database(scope.target.clone()),
            Self::SharedResource { target, .. } => CollectionTarget::Shared(target.clone()),
        }
    }

    pub fn subject(&self) -> CollectionSubject {
        match self {
            Self::Activity(_) | Self::NativeRead(_) => CollectionSubject::Database,
            Self::SharedResource { target, .. } => CollectionSubject::Shared { resource: target.clone() },
        }
    }

    pub fn as_activity(&self) -> Option<&DiagnosticScope> {
        match self {
            Self::Activity(scope) => Some(scope),
            Self::SharedResource { .. } | Self::NativeRead(_) => None,
        }
    }

    pub fn partition(&self) -> &SourcePartition {
        match self {
            Self::Activity(scope) => &scope.partition,
            Self::NativeRead(scope) => &scope.partition,
            Self::SharedResource { partition, .. } => partition,
        }
    }

    pub fn source(&self) -> Option<&str> {
        match self {
            Self::Activity(scope) => Some(&scope.source),
            Self::NativeRead(_) => None,
            Self::SharedResource { source, .. } => Some(source),
        }
    }

    pub fn native_epoch(&self) -> Option<&str> {
        match self {
            Self::Activity(scope) => scope.native_epoch.as_deref(),
            Self::NativeRead(scope) => scope.partition.native_epoch(),
            Self::SharedResource { partition: SourcePartition::InstanceLocal { epoch, .. }, .. } => epoch.as_deref(),
            Self::SharedResource { .. } => None,
        }
    }

    /// Compare the authority source, not a different source's cost-counter payload or later result dataset.
    pub fn matches_source(&self, source: &CollectionDescription) -> bool {
        if !self.is_valid() || !source.partition_is_consistent() {
            return false;
        }
        match self {
            Self::Activity(expected) => {
                DiagnosticScope::from_source(source).is_ok_and(|current| current.key().ok() == expected.key().ok())
            }
            Self::NativeRead(expected) => match &source.observation {
                SourceObservation::Activity(_) => {
                    NativeReadContext::from_activity(source).is_ok_and(|context| expected.matches_context(&context))
                }
                _ => source.observation.execution_context().is_some_and(|context| expected.matches_context(context)),
            },
            Self::SharedResource { target, partition, source: expected } => {
                source.target == CollectionTarget::Shared(target.clone())
                    && source.partition == *partition
                    && source.source == *expected
                    && matches!(&source.observation, SourceObservation::Snapshot(snapshot)
                        if snapshot.is_valid() && snapshot.dataset == SnapshotDataset::Operational)
            }
        }
    }
}
