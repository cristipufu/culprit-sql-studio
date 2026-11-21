//! Logical history and local resource acquisitions have different native identity requirements.
use super::*;

/// A local database/process epoch is sufficient for local resources. It does not become a globally
/// portable database incarnation, a historical Query Store executor or a shared tempdb owner.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct LocalResourceReadScope {
    pub target: crate::DatabaseIdentity,
    pub partition: SourcePartition,
    pub database_writable: bool,
}
impl LocalResourceReadScope {
    pub fn from_context(context: &NativeReadContext) -> Result<Self> {
        let CollectionTarget::Database(target) = &context.target else { return Err(invalid()) };
        let value = Self {
            target: target.clone(),
            partition: context.partition.clone(),
            database_writable: context.database_writable.ok_or_else(invalid)?,
        };
        if !context.has_qualified_local_owner() || !value.is_valid() {
            return Err(invalid());
        }
        Ok(value)
    }
    pub fn is_valid(&self) -> bool {
        let target = CollectionTarget::Database(self.target.clone());
        target.is_valid()
            && self.partition.is_valid_for(&target)
            && matches!(&self.partition, SourcePartition::InstanceLocal { member: Some(_), epoch: Some(_) })
            && self.partition.is_qualified()
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        hash(&("local-resource-read-scope-v1", self.target.telemetry_id()?, &self.partition, self.database_writable))
    }
    pub fn matches_context(&self, context: &NativeReadContext) -> bool {
        self.matches_original_context(context) && context.database_writable == Some(self.database_writable)
    }
    /// Historical updateability stays independently recorded/unknown. Only the new source execution requires
    /// its own known current role; an earlier role never grants or denies that new execution by itself.
    pub fn matches_original_context(&self, context: &NativeReadContext) -> bool {
        self.is_valid()
            && context.has_qualified_local_owner()
            && context.target.database() == Some(&self.target)
            && context.partition == self.partition
    }
}

/// Closed native authority choices. Adding a question never weakens another family's identity requirement.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", content = "scope", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum SourceReadExecutionScope {
    Logical(NativeSourceReadScope),
    Local(LocalResourceReadScope),
}
impl From<NativeSourceReadScope> for SourceReadExecutionScope {
    fn from(scope: NativeSourceReadScope) -> Self {
        Self::Logical(scope)
    }
}
impl SourceReadExecutionScope {
    /// A source's acquisition owner is independent of historical query/compile execution populations.
    pub fn context_from_source(source: &CollectionDescription) -> Result<NativeReadContext> {
        if !source.partition_is_consistent() {
            return Err(invalid());
        }
        if let Some(context) = source.observation.execution_context() {
            return Ok(context.clone());
        }
        if let Some(history) = source.query_history() {
            let context = history.acquisition_owner.as_deref().ok_or_else(invalid)?;
            if !history.is_valid() || context.target != source.target {
                return Err(invalid());
            }
            return Ok(context.clone());
        }
        NativeReadContext::from_activity(source).or_else(|_| NativeReadContext::from_snapshot(source))
    }
    pub fn from_context(question: SourceReadQuestion, context: &NativeReadContext) -> Result<Self> {
        match question {
            SourceReadQuestion::CompilationHistory | SourceReadQuestion::HyperscaleLogComponents => {
                NativeSourceReadScope::from_context(context).map(Self::Logical)
            }
            SourceReadQuestion::TemporarySpaceContributors => {
                LocalResourceReadScope::from_context(context).map(Self::Local)
            }
        }
    }
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Logical(scope) => scope.is_valid(),
            Self::Local(scope) => scope.is_valid(),
        }
    }
    pub fn accepts_question(&self, question: SourceReadQuestion) -> bool {
        self.is_valid()
            && matches!(
                (self, question),
                (
                    Self::Logical(_),
                    SourceReadQuestion::CompilationHistory | SourceReadQuestion::HyperscaleLogComponents
                ) | (Self::Local(_), SourceReadQuestion::TemporarySpaceContributors)
            )
    }
    pub fn logical(&self) -> Option<&NativeSourceReadScope> {
        match self {
            Self::Logical(scope) => Some(scope),
            Self::Local(_) => None,
        }
    }
    pub fn target(&self) -> &crate::DatabaseIdentity {
        match self {
            Self::Logical(scope) => &scope.target,
            Self::Local(scope) => &scope.target,
        }
    }
    pub fn partition(&self) -> &SourcePartition {
        match self {
            Self::Logical(scope) => &scope.partition,
            Self::Local(scope) => &scope.partition,
        }
    }
    pub fn database_writable(&self) -> bool {
        match self {
            Self::Logical(scope) => scope.database_writable,
            Self::Local(scope) => scope.database_writable,
        }
    }
    pub fn key(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(invalid());
        }
        hash(&("source-read-execution-scope-v1", self))
    }
    pub fn matches_context(&self, context: &NativeReadContext) -> bool {
        match self {
            Self::Logical(scope) => scope.matches_context(context),
            Self::Local(scope) => scope.matches_context(context),
        }
    }
    pub fn matches_original_context(&self, context: &NativeReadContext) -> bool {
        match self {
            Self::Logical(scope) => scope.matches_context(context),
            Self::Local(scope) => scope.matches_original_context(context),
        }
    }
    pub fn matches_target(&self, target: &CollectionTarget) -> bool {
        self.is_valid() && target.telemetry_id().ok() == self.target().telemetry_id().ok()
    }
}

/// Only these already implemented resource profiles preserve role on ordinary snapshot originals.
pub fn local_resource_header_is_valid(snapshot: &StateSnapshotDescription) -> bool {
    snapshot.is_valid() && matches!(snapshot.dataset, SnapshotDataset::Cleanup | SnapshotDataset::TaskSpace)
}
