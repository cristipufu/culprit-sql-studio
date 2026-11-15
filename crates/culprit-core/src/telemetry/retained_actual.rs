//! A retained plan's current statement relation and read provenance do not establish a historical execution group.
use super::*;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct RetainedActualPlanRequest {
    pub query_id: String,
    pub plan_id: Option<String>,
    pub expected_target: Option<CollectionTarget>,
    pub expected_query: Option<NativeQueryIdentity>,
    pub expected_plan: Option<NativePlanIdentity>,
}

impl RetainedActualPlanRequest {
    pub fn is_valid(&self) -> bool {
        valid_key(&self.query_id)
            && self
                .expected_target
                .as_ref()
                .is_none_or(|target| matches!(target, CollectionTarget::Database(_)) && target.is_valid())
            && !self.query_id.trim().is_empty()
            && self.plan_id.as_deref().is_none_or(|id| valid_key(id) && !id.trim().is_empty())
            && self.expected_query.as_ref().is_none_or(|query| query.is_valid() && query.query_id == self.query_id)
            && self.expected_plan.as_ref().is_none_or(|plan| {
                plan.is_valid()
                    && self.plan_id.as_ref() == Some(&plan.plan_id)
                    && plan.query.query_id == self.query_id
                    && self.expected_query.as_ref().is_none_or(|query| query == &plan.query)
            })
    }

    pub fn validate(&self) -> Result<()> {
        if self.is_valid() { Ok(()) } else { Err(Error::invalid(t!("plan.retained.invalid_request"))) }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct RetainedPlanProvenance {
    pub read: NativeReadContext,
    pub statement: CachedStatementContext,
    /// A source-established CURRENT statement relation; it does not transfer historical runtime or schema facts.
    pub query: Option<NativeQueryIdentity>,
    pub native_plan: Option<NativePlanIdentity>,
}

impl RetainedPlanProvenance {
    pub fn is_valid(&self) -> bool {
        self.read.is_valid()
            && matches!(self.read.target, CollectionTarget::Database(_))
            && matches!(self.read.partition, SourcePartition::InstanceLocal { .. })
            && self.statement.is_valid()
            && self.statement.identity.has_plan_handle()
            && self.statement.identity.has_sql_handle()
            && self.query.as_ref().is_none_or(NativeQueryIdentity::is_valid)
            && self.native_plan.as_ref().is_none_or(|plan| plan.is_valid() && self.query.as_ref() == Some(&plan.query))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct RetainedActualPlan {
    pub plan: crate::Plan,
    pub provenance: RetainedPlanProvenance,
}

impl RetainedActualPlan {
    pub fn is_valid(&self) -> bool {
        self.provenance.is_valid()
            && self.plan.kind == crate::PlanKind::Actual
            && self.plan.engine == self.provenance.read.target.engine()
    }

    /// A valid retained plan is still independent evidence until its native selectors match this lookup.
    pub fn matches_request(&self, request: &RetainedActualPlanRequest) -> bool {
        if !self.is_valid() || !request.is_valid() {
            return false;
        }
        let Some(query) = &self.provenance.query else { return false };
        query.query_id == request.query_id
            && request.expected_query.as_ref().is_none_or(|expected| expected == query)
            && request
                .plan_id
                .as_ref()
                .is_none_or(|id| self.provenance.native_plan.as_ref().is_some_and(|plan| &plan.plan_id == id))
            && request
                .expected_plan
                .as_ref()
                .is_none_or(|expected| self.provenance.native_plan.as_ref() == Some(expected))
            && request.expected_target.as_ref().is_none_or(|target| {
                target.telemetry_id().is_ok_and(|expected| {
                    self.provenance.read.target.telemetry_id().is_ok_and(|current| current == expected)
                })
            })
    }
}

#[cfg(test)]
mod tests;
