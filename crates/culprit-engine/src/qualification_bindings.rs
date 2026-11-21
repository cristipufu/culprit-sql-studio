//! Per-saved-connection installed authority; only the authenticated restoration service can populate it.
use culprit_collector::outbox::IssuedQualification;
use culprit_core::{Result, telemetry::*};
use std::sync::RwLock;

#[derive(Default)]
pub(crate) struct QualificationBindings {
    entries: RwLock<Vec<IssuedQualification>>,
}
impl QualificationBindings {
    pub(crate) fn install(&self, binding: IssuedQualification) -> Result<()> {
        let mut entries = self.entries.write().map_err(|_| invalid())?;
        entries.retain(|entry| {
            entry.connection() != binding.connection() || entry.receipt().recipe_id != binding.receipt().recipe_id
        });
        entries.push(binding);
        Ok(())
    }
    pub(crate) fn replace(&self, entries: Vec<IssuedQualification>) -> Result<()> {
        *self.entries.write().map_err(|_| invalid())? = entries;
        Ok(())
    }
    pub(crate) fn remove(&self, connection: &str, recipe_id: &str) -> Result<()> {
        self.entries
            .write()
            .map_err(|_| invalid())?
            .retain(|entry| entry.connection() != connection || entry.receipt().recipe_id != recipe_id);
        Ok(())
    }
    pub(crate) fn receipts(&self, connection: &str, route: &str) -> Result<Vec<RecipeCostReceipt>> {
        Ok(self
            .entries
            .read()
            .map_err(|_| invalid())?
            .iter()
            .filter(|entry| entry.is_active() && entry.connection() == connection && entry.route_revision() == route)
            .map(|entry| entry.receipt().clone())
            .collect())
    }
    pub(crate) fn profile_binding(
        &self,
        connection: &str,
        route: &str,
        receipt: &RecipeCostReceipt,
    ) -> Result<Option<IssuedQualification>> {
        Ok(self
            .entries
            .read()
            .map_err(|_| invalid())?
            .iter()
            .find(|entry| {
                entry.is_active()
                    && entry.connection() == connection
                    && entry.route_revision() == route
                    && entry.receipt() == receipt
                    && receipt.purpose == RecipePurpose::Baseline
            })
            .cloned())
    }
    pub(crate) fn route_revision(&self, connection: &str, route: &str) -> Result<String> {
        let mut entries = self
            .entries
            .read()
            .map_err(|_| invalid())?
            .iter()
            .filter(|entry| entry.is_active() && entry.connection() == connection && entry.route_revision() == route)
            .map(|entry| entry.id())
            .collect::<Result<Vec<_>>>()?;
        if entries.is_empty() {
            return Ok(route.into());
        }
        entries.sort();
        serde_json::to_vec(&("qualified-saved-route-v1", route, entries))
            .map(|bytes| digest(&bytes))
            .map_err(|_| invalid())
    }
}
fn invalid() -> culprit_core::Error {
    culprit_core::Error::invalid(culprit_core::t!("engine.registry.invalid_qualification"))
}
