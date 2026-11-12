//! The subject of a collection is independent of the connection used to reach it.
use super::{NativeIdentity, digest};
use crate::{DatabaseIdentity, Engine, Result};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

#[derive(Serialize, Deserialize, TS, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
#[ts(export)]
pub enum SharedResourceKind {
    Instance,
    Pool,
    Cluster,
    Node,
}

/// The adapter supplies both the resource's native identity and its namespace. A pool name, node number,
/// or process-local identifier alone cannot establish identity across unrelated servers or clusters.
/// Resource incarnation belongs here; counter resets and process/session lifetimes remain measurement evidence.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[ts(export)]
pub struct SharedResourceIdentity {
    pub engine: Engine,
    pub kind: SharedResourceKind,
    pub namespace: NativeIdentity,
    pub native: NativeIdentity,
}

impl SharedResourceIdentity {
    pub fn is_valid(&self) -> bool {
        self.namespace.is_valid() && self.native.is_valid()
    }

    pub fn telemetry_id(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(super::CollectionStop::InvalidPage.error());
        }
        let encoded = serde_json::to_vec(&("shared-resource-v1", self))
            .map_err(|_| super::CollectionStop::InvalidPage.error())?;
        Ok(digest(&encoded))
    }
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq, Eq)]
#[serde(tag = "scope", content = "identity", rename_all = "kebab-case")]
#[ts(export)]
pub enum CollectionTarget {
    Database(DatabaseIdentity),
    Shared(SharedResourceIdentity),
}

impl CollectionTarget {
    pub fn measurement_scope(&self) -> crate::MeasurementScope {
        use crate::MeasurementScope;
        match self {
            Self::Database(_) => MeasurementScope::Database,
            Self::Shared(resource) => match resource.kind {
                SharedResourceKind::Instance => MeasurementScope::Server,
                SharedResourceKind::Pool => MeasurementScope::Pool,
                SharedResourceKind::Cluster => MeasurementScope::Cluster,
                SharedResourceKind::Node => MeasurementScope::Node,
            },
        }
    }

    pub fn engine(&self) -> Engine {
        match self {
            Self::Database(database) => database.engine,
            Self::Shared(resource) => resource.engine,
        }
    }

    pub fn database(&self) -> Option<&DatabaseIdentity> {
        match self {
            Self::Database(database) => Some(database),
            Self::Shared(_) => None,
        }
    }

    pub fn is_valid(&self) -> bool {
        match self {
            Self::Database(database) => !database.database.trim().is_empty(),
            Self::Shared(resource) => resource.is_valid(),
        }
    }

    pub fn telemetry_id(&self) -> Result<String> {
        if !self.is_valid() {
            return Err(super::CollectionStop::InvalidPage.error());
        }
        match self {
            Self::Database(database) => database.telemetry_id(),
            Self::Shared(resource) => resource.telemetry_id(),
        }
    }
}

impl From<DatabaseIdentity> for CollectionTarget {
    fn from(database: DatabaseIdentity) -> Self {
        Self::Database(database)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resource(kind: SharedResourceKind, namespace: &str) -> SharedResourceIdentity {
        SharedResourceIdentity {
            engine: Engine::Mssql,
            kind,
            namespace: NativeIdentity::new(namespace),
            native: NativeIdentity::new("native-resource").qualified("generation", "first"),
        }
    }

    #[test]
    fn native_resource_identity_keeps_namespace_kind_engine_and_generation() {
        let pool = resource(SharedResourceKind::Pool, "instance-a");
        let original = pool.telemetry_id().unwrap();
        assert_ne!(original, resource(SharedResourceKind::Pool, "instance-b").telemetry_id().unwrap());
        assert_ne!(original, resource(SharedResourceKind::Node, "instance-a").telemetry_id().unwrap());
        let mut changed = pool.clone();
        changed.engine = Engine::Postgres;
        assert_ne!(original, changed.telemetry_id().unwrap());
        changed = pool;
        changed.native = changed.native.qualified("generation", "recreated");
        assert_ne!(original, changed.telemetry_id().unwrap());
    }

    #[test]
    fn shared_resources_never_provide_an_invented_database_context() {
        let target = CollectionTarget::Shared(resource(SharedResourceKind::Pool, "instance"));
        assert!(target.is_valid());
        assert!(target.database().is_none());
        assert_eq!(target.engine(), Engine::Mssql);
        let database = DatabaseIdentity::server(Engine::Postgres, "app", "fixture.invalid", 5432, None);
        let id = database.telemetry_id().unwrap();
        let target = CollectionTarget::Database(database);
        assert_eq!(target.telemetry_id().unwrap(), id);
        assert_eq!(target.database().unwrap().database, "app");
    }

    #[test]
    fn missing_native_scope_cannot_enter_resource_identity_or_trusted_collection() {
        let mut resource = resource(SharedResourceKind::Cluster, "");
        assert!(!resource.is_valid());
        assert!(resource.telemetry_id().is_err());
        resource.namespace = NativeIdentity::new("cluster");
        resource.native = NativeIdentity::new("");
        assert!(!CollectionTarget::Shared(resource).is_valid());
    }
}
