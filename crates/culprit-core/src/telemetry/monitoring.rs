//! Standard enrollment is policy; only a selected registration and authenticated native evidence grant admission.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum MonitoringPolicy {
    Explicit,
    Standard { budget: CollectionBudget },
}
impl MonitoringPolicy {
    pub fn validate(&self, inventory: &FleetInventory, shard: &str) -> Result<()> {
        match self {
            Self::Explicit => inventory.validate(shard),
            Self::Standard { budget } => {
                budget.validate()?;
                inventory.validate_assignments(shard)
            }
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum MonitoringGapReason {
    Undeclared,
    Unqualified,
    MissingApproval,
    AdmissionUnsupported,
    RequiresSelectedTarget,
    AdditionalWorkUnqualified,
    InfeasibleCadence,
    ScheduleCapacity,
    ConflictingSchedule,
    SharedOwnerAlreadyScheduled,
    SharedAdmissionConflict,
    Capacity,
}

/// Configured logical labels only. Native proof, identifiers, SQL and credentials never enter enrollment output.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MonitoringGap {
    pub target: String,
    pub recipe_id: Option<String>,
    pub reason: MonitoringGapReason,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MonitoringScheduleSummary {
    pub target: String,
    pub recipe_id: String,
    pub every_seconds: u32,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct MonitoringEnrollmentSummary {
    pub assigned_targets: u32,
    pub explicit_schedules: u32,
    pub automatic: Vec<MonitoringScheduleSummary>,
    pub gaps: Vec<MonitoringGap>,
    pub omitted_gaps: u32,
}

/// A structural request for one approved source profile, independent of any episode or affected request.
/// It is deliberately not serializable or constructible by deserializing configuration. The selected provider
/// registration still authenticates the installed original approval before native construction and must bind
/// this expectation inside the existing native reader before its source work, including the final recheck.
#[derive(Clone)]
pub struct SourceProfileAdmission {
    route_revision: String,
    qualification: Box<RecipeCostReceipt>,
    budget: CollectionBudget,
    approval_is_active: std::sync::Arc<dyn Fn() -> bool + Send + Sync>,
    retired: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl SourceProfileAdmission {
    pub fn new(
        route_revision: String,
        qualification: RecipeCostReceipt,
        budget: CollectionBudget,
        approval_is_active: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Result<Self> {
        budget.validate()?;
        if !valid_digest(&route_revision)
            || !qualification.is_valid()
            || qualification.purpose != RecipePurpose::Baseline
            || budget.intersection(&qualification.tested_budget) != budget
        {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(Self {
            route_revision,
            qualification: Box::new(qualification),
            budget,
            approval_is_active: std::sync::Arc::new(approval_is_active),
            retired: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        })
    }
    /// Observed retirement is permanent for this original profile and all its clones. A later approval needs a
    /// new admitted profile; a callback returning true cannot revive a retired source lifetime.
    /// Liveness is necessary, not authority. Only the selected registration plus authenticated issued original
    /// may supply this check; a caller-created callback or structurally valid receipt grants no permission.
    pub fn approval_is_active(&self) -> bool {
        use std::sync::atomic::Ordering;
        if self.retired.load(Ordering::Acquire) {
            return false;
        }
        if !(self.approval_is_active)() {
            self.retired.store(true, Ordering::Release);
        }
        !self.retired.load(Ordering::Acquire)
    }
    pub fn route_revision(&self) -> &str {
        &self.route_revision
    }
    pub fn qualification(&self) -> &RecipeCostReceipt {
        &self.qualification
    }
    pub fn matches_request(&self, request: &CollectionRequest) -> bool {
        self.approval_is_active()
            && request.validate().is_ok()
            && self.budget == request.budget
            && self.qualification.selector.matches(request)
            && self.qualification.scope.subject() == request.subject
    }
    /// Exact current native owner and known role, not a logical alias, current database name or old event label.
    pub fn matches_native_context(&self, context: &NativeReadContext) -> bool {
        if !self.approval_is_active() || !context.is_valid() || !context.partition.is_qualified() {
            return false;
        }
        match &self.qualification.scope {
            RecipeCostScope::NativeRead(scope) => scope.matches_context(context),
            RecipeCostScope::SharedResource { target, partition, .. } => {
                context.target == CollectionTarget::Shared(target.clone()) && context.partition == *partition
            }
            RecipeCostScope::Activity(_) => false,
        }
    }
}

impl std::fmt::Debug for SourceProfileAdmission {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SourceProfileAdmission")
            .field("recipe_id", &self.qualification.recipe_id)
            .field("recipe_version", &self.qualification.recipe_version)
            .field("approval_active", &self.approval_is_active())
            .finish_non_exhaustive()
    }
}
