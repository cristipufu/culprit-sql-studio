//! A connection is a route; the requested subject is the resource whose native identity the adapter must verify.
use super::*;

pub const MAX_RESOURCE_COLLECTIONS: usize = 32;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum CollectionSubject {
    Database,
    Shared { resource: SharedResourceIdentity },
}

impl CollectionSubject {
    pub fn validate(&self) -> Result<()> {
        if let Self::Shared { resource } = self
            && !resource.is_valid()
        {
            return Err(Error::invalid(t!("collection.invalid_subject")));
        }
        Ok(())
    }

    pub fn accepts(&self, actual: &CollectionTarget) -> bool {
        match (self, actual) {
            (Self::Database, CollectionTarget::Database(database)) => !database.database.trim().is_empty(),
            (Self::Shared { resource }, CollectionTarget::Shared(actual)) => resource
                .telemetry_id()
                .ok()
                .zip(actual.telemetry_id().ok())
                .is_some_and(|(expected, actual)| expected == actual),
            _ => false,
        }
    }

    pub fn require_database(&self) -> Result<()> {
        match self {
            Self::Database => Ok(()),
            Self::Shared { .. } => Err(Error::new(ErrorCode::Unsupported, t!("collection.unsupported"))),
        }
    }

    /// Shared ownership is independent of the chosen connection alias. A database route is resolved by its adapter.
    pub fn assignment_key(&self, connection: &str) -> Result<String> {
        self.validate()?;
        match self {
            Self::Database => serde_json::to_vec(&("database-route", connection))
                .map(|bytes| digest(&bytes))
                .map_err(|_| CollectionStop::InvalidPage.error()),
            Self::Shared { resource } => resource.telemetry_id(),
        }
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct ResourceDiscoveryRequest {
    pub kind: SharedResourceKind,
    pub budget: CollectionBudget,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug)]
#[ts(export)]
pub struct CollectionResource {
    pub identity: SharedResourceIdentity,
    /// The adapter advertises native routes, independently of the connection used for future captures.
    pub collections: Vec<TelemetryCollection>,
    pub observed_at: String,
}

impl CollectionResource {
    pub fn is_valid(&self) -> bool {
        self.identity.is_valid()
            && canonical_time(&self.observed_at).is_ok()
            && !self.collections.is_empty()
            && self.collections.len() <= MAX_RESOURCE_COLLECTIONS
            && self
                .collections
                .iter()
                .enumerate()
                .all(|(index, collection)| !self.collections[..index].contains(collection))
    }
}

pub const MAX_DISCOVERED_RESOURCES: usize = 128;

impl ResourceDiscoveryRequest {
    pub fn validate_result(&self, resources: &[CollectionResource]) -> Result<()> {
        self.budget.validate()?;
        let mut seen = std::collections::BTreeSet::new();
        if resources.len() > MAX_DISCOVERED_RESOURCES
            || resources.len() > self.budget.page_rows as usize
            || resources.iter().any(|resource| {
                !resource.is_valid()
                    || resource.identity.kind != self.kind
                    || !resource.identity.telemetry_id().is_ok_and(|id| seen.insert(id))
            })
            || !serde_json::to_vec(resources).is_ok_and(|bytes| bytes.len() <= self.budget.page_bytes as usize)
        {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool(namespace: &str) -> SharedResourceIdentity {
        SharedResourceIdentity {
            engine: crate::Engine::Mssql,
            kind: SharedResourceKind::Pool,
            namespace: NativeIdentity::new(namespace),
            native: NativeIdentity::new("pool"),
        }
    }

    #[test]
    fn shared_requests_require_the_exact_native_resource_and_cannot_fall_back_to_the_connection_database() {
        let wanted = pool("server-one");
        let subject = CollectionSubject::Shared { resource: wanted.clone() };
        let matching = CollectionTarget::Shared(wanted);
        assert!(subject.accepts(&matching));
        assert_eq!(subject.assignment_key("connection-a").unwrap(), subject.assignment_key("connection-b").unwrap());
        assert!(!subject.accepts(&CollectionTarget::Shared(pool("server-two"))));
        let database = CollectionTarget::Database(crate::DatabaseIdentity::server(
            crate::Engine::Mssql,
            "member",
            "fixture.invalid",
            1433,
            None,
        ));
        assert!(!subject.accepts(&database));
        assert!(!CollectionSubject::Database.accepts(&matching));
        assert!(subject.require_database().is_err());
        let changed = CollectionSubject::Shared { resource: pool("server-two") };
        assert_ne!(subject.assignment_key("connection-a").unwrap(), changed.assignment_key("connection-a").unwrap());
    }

    #[test]
    fn discovery_rejects_duplicate_wrong_scope_or_unbounded_resource_descriptions() {
        let request = ResourceDiscoveryRequest { kind: SharedResourceKind::Pool, budget: Default::default() };
        let valid = CollectionResource {
            identity: pool("server-one"),
            collections: vec![TelemetryCollection::ResourceHistory],
            observed_at: "2026-11-13T10:00:00Z".into(),
        };
        request.validate_result(std::slice::from_ref(&valid)).unwrap();
        assert!(request.validate_result(&[valid.clone(), valid.clone()]).is_err());
        let mut invalid = valid.clone();
        invalid.identity.kind = SharedResourceKind::Instance;
        assert!(request.validate_result(&[invalid]).is_err());
        let mut invalid = valid.clone();
        invalid.collections.push(TelemetryCollection::ResourceHistory);
        assert!(request.validate_result(&[invalid]).is_err());
        let mut invalid = valid.clone();
        invalid.identity.namespace.id.clear();
        assert!(request.validate_result(&[invalid]).is_err());
        let mut invalid = valid;
        invalid.observed_at = "unknown".into();
        assert!(request.validate_result(&[invalid]).is_err());
    }
}
