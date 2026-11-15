use super::*;
use crate::probe::{self, Probe};
use culprit_core::{DatabaseIdentity, Engine};
pub(crate) struct Metadata {
    pub probe: Probe,
    pub node: String,
    pub backend: u64,
    pub database: DatabaseIdentity,
}
impl Metadata {
    pub(crate) fn activity_epoch(&self) -> Option<String> {
        let started = self.probe.started.as_deref()?;
        canonical_time(started).ok()?;
        Some(digest(format!("{}|{}|{}|ysql-activity-v1", self.node, self.probe.oid, started).as_bytes()))
    }
    pub(crate) fn same_activity_source(&self, other: &Self) -> bool {
        self.database == other.database
            && self.node == other.node
            && self.backend == other.backend
            && self.probe.oid == other.probe.oid
            && self.probe.started == other.probe.started
            && self.probe.version == other.probe.version
            && self.probe.compatibility == other.probe.compatibility
    }
    pub(crate) fn same_source(&self, other: &Self) -> bool {
        self.database == other.database
            && self.node == other.node
            && self.backend == other.backend
            && self.probe.oid == other.probe.oid
            && self.probe.started == other.probe.started
            && self.probe.version == other.probe.version
            && self.probe.compatibility == other.probe.compatibility
            && self.probe.statement_role == other.probe.statement_role
            && self.probe.statement_schema == other.probe.statement_schema
            && self.probe.metrics == other.probe.metrics
            && self.probe.rpc_metrics == other.probe.rpc_metrics
            && self.probe.global == other.probe.global
    }
}
pub(crate) async fn read(reader: &mut impl QueryRead, host: &str, port: u16) -> Result<Metadata> {
    let probe = probe::read_from(reader).await?;
    from_probe(reader, host, port, probe).await
}
pub(crate) async fn from_probe(reader: &mut impl QueryRead, host: &str, port: u16, probe: Probe) -> Result<Metadata> {
    let node = probe.node.clone().ok_or_else(|| CollectionStop::SourceUnavailable.error())?;
    canonical_time(probe.started.as_deref().ok_or_else(invalid)?)?;
    let row = one(reader, "SELECT pg_catalog.pg_backend_pid()::text AS backend").await?;
    let backend = integer(&row, "backend")?;
    if backend == 0 {
        return Err(invalid());
    }
    Ok(Metadata {
        database: DatabaseIdentity::server(Engine::Yugabyte, &probe.database, host, port, None),
        probe,
        node,
        backend,
    })
}

impl Metadata {
    pub(crate) fn completed_action_context(&self, after: &Self) -> Option<NativeReadContext> {
        if !self.same_activity_source(after) || self.node.bytes().all(|byte| byte == b'0') {
            return None;
        }
        let context = NativeReadContext {
            target: self.database.clone().into(),
            partition: SourcePartition::InstanceLocal {
                member: Some(SharedResourceIdentity {
                    engine: Engine::Yugabyte,
                    kind: SharedResourceKind::Node,
                    namespace: NativeIdentity::new("yugabyte-tserver-uuid"),
                    native: NativeIdentity::new(&self.node),
                }),
                epoch: Some(self.activity_epoch()?),
            },
            observed_from: canonical_time(&self.probe.now).ok()?,
            observed_until: canonical_time(&after.probe.now).ok()?,
            database_writable: None,
        };
        context.has_qualified_local_owner().then_some(context)
    }
}
