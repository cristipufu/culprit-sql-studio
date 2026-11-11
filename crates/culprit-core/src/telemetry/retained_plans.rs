//! Native plan-cache evidence has an independent lifetime from a statement's running totals.
use super::*;
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct NativeRetainedPlan {
    pub definition: PlanDefinition,
    pub counters: NativeCounterSet,
    pub first_used: Option<String>,
    pub last_used: Option<String>,
}
impl NativeRetainedPlan {
    pub fn is_valid(&self) -> bool {
        self.definition.is_valid()
            && self.counters.is_valid()
            && [&self.first_used, &self.last_used].into_iter().flatten().all(|at| canonical_time(at).is_ok())
            && self.counters.identity.id==self.definition.id
            && self.first_used.as_ref().zip(self.last_used.as_ref()).is_none_or(|(first,last)|matches!((DateTime::parse_from_rfc3339(first),DateTime::parse_from_rfc3339(last)),(Ok(first),Ok(last)) if first<=last))
    }
    pub(super) fn redact(&mut self, engine: crate::Engine) {
        if let DefinitionPayload::Available { text, .. } = &self.definition.plan {
            self.definition.plan = DefinitionPayload::plan(text, engine);
        }
    }
}
