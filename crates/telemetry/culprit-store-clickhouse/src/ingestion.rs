//! Insert policy is explicit and separate from the endpoint/database identity used by read-only clients.
use crate::client::{ClickHouseClient, ClickHouseConfig, Credentials, InsertData, QueryTransport};
use async_trait::async_trait;
use culprit_collector::delivery::sink::Submission;
use culprit_core::{Result, telemetry::DeliveryProblem};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Copy, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WritePolicy {
    /// A replica quorum for ReplicatedMergeTree; omit for single-node MergeTree delivery.
    #[serde(default)]
    pub insert_quorum: Option<u32>,
}
impl WritePolicy {
    pub fn validate(self) -> Result<()> {
        if self.insert_quorum.is_some_and(|quorum| !(1..=128).contains(&quorum)) {
            return Err(DeliveryProblem::InvalidContract.error());
        }
        Ok(())
    }
}

#[async_trait]
pub trait IngestionTransport: Send + Sync {
    fn destination(&self) -> &str;
    async fn insert(&self, table: &str, data: InsertData) -> Result<Submission>;
}
pub struct ClickHouseIngestor {
    query: Arc<ClickHouseClient>,
    policy: WritePolicy,
}
impl ClickHouseIngestor {
    pub fn new(config: ClickHouseConfig, credentials: Credentials, policy: WritePolicy) -> Result<Self> {
        policy.validate()?;
        let query =
            Arc::new(ClickHouseClient::new(config, credentials)?.with_sequential_reads(policy.insert_quorum.is_some()));
        Ok(Self { query, policy })
    }
    /// Writer verification retains the native sequential-consistency requirement when quorum inserts are selected.
    pub fn query_transport(&self) -> Arc<dyn QueryTransport> {
        self.query.clone()
    }
}
#[async_trait]
impl IngestionTransport for ClickHouseIngestor {
    fn destination(&self) -> &str {
        self.query.destination()
    }
    async fn insert(&self, table: &str, data: InsertData) -> Result<Submission> {
        self.query.insert(table, data, self.policy).await
    }
}
