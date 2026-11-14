//! Fresh native observations bind a saved route only for this runner activation.
use culprit_core::{Result, telemetry::*};
use std::{collections::BTreeMap, sync::Mutex};

#[derive(Default)]
pub(in crate::fleet) struct RouteBindings {
    routes: BTreeMap<String, RouteBinding>,
    planning: Mutex<(Option<String>, usize)>,
}

pub(in crate::fleet) struct RouteBinding {
    pub source: CollectionDescription,
    pub revision: String,
}

impl RouteBinding {
    pub fn matches_scope(&self, scope: &DiagnosticScope) -> bool {
        DiagnosticScope::from_source(&self.source)
            .is_ok_and(|current| current.native_epoch.is_some() && current.key().ok() == scope.key().ok())
    }
}

impl RouteBindings {
    pub fn get(&self, target: &str) -> Option<&RouteBinding> {
        self.routes.get(target)
    }

    pub fn invalidate(&mut self, target: &str) {
        self.routes.remove(target);
    }

    pub fn observe(&mut self, target: &str, source: &CollectionDescription, revision: &str) -> Result<()> {
        let scope = DiagnosticScope::from_source(source)?;
        if scope.native_epoch.is_none()
            || revision.is_empty()
            || revision.len() > 256
            || revision.chars().any(char::is_control)
        {
            return Err(CollectionStop::InvalidPage.error());
        }
        self.routes.insert(target.into(), RouteBinding { source: source.clone(), revision: revision.into() });
        Ok(())
    }

    pub fn planning_position(&self) -> (Option<String>, usize) {
        self.planning.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub fn set_planning_position(&self, head: Option<String>, episode: usize) {
        // This cursor supplies fairness only. It never supplies route identity, qualification or native permission.
        *self.planning.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = (head, episode);
    }
}
