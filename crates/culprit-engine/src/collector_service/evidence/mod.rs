//! Read existing committed captures without creating history, acquiring write ownership or opening a source database.
mod config;
pub(super) mod stores;
pub use config::{EvidenceConfig, EvidenceStoreConfig};
pub use stores::EvidenceStoreProvider;

use super::CollectorRuntime;
use culprit_collector::{
    analytical::{
        evidence::{EvidenceCapture, EvidenceFamily, EvidenceReadBudget, EvidenceReader},
        indexes::ProjectionIndexes,
    },
    capture::CollectionCancellation,
    delivery::{
        contract::Contract,
        object_store::{ObjectRecoveryStore, transport::ObjectTransport},
        recovery::reopen,
    },
};
use culprit_core::{
    Result,
    telemetry::{CollectionStop, DeliveryProblem, valid_digest},
};
use std::{sync::Arc, time::Duration};

pub struct EvidenceRequest {
    pub range_id: String,
    pub families: Vec<EvidenceFamily>,
    pub budget: EvidenceReadBudget,
}
impl EvidenceRequest {
    pub fn validate(&self) -> Result<()> {
        self.budget.validate()?;
        if !valid_digest(&self.range_id)
            || self.families.is_empty()
            || self.families.len() > EvidenceFamily::ALL.len()
            || self.families.iter().enumerate().any(|(index, family)| self.families[..index].contains(family))
        {
            return Err(invalid());
        }
        Ok(())
    }
}

pub struct EvidenceService {
    recovery: Arc<dyn ObjectTransport>,
    contract: Contract,
    reader: EvidenceReader,
}
impl CollectorRuntime {
    pub fn with_evidence_store_provider(mut self, provider: Arc<dyn EvidenceStoreProvider>) -> Result<Self> {
        self.evidence_stores.register(provider)?;
        Ok(self)
    }

    /// Configures only warehouse and recovery identities. No source, runtime directory or remote history is opened.
    pub fn evidence(&self, config: &EvidenceConfig) -> Result<EvidenceService> {
        let credentials: Arc<dyn culprit_auth::CredentialResolver> = match &self.credentials {
            Some(credentials) => credentials.clone(),
            None => Arc::new(config.credentials()?),
        };
        let recovery = self.recovery.configure(&config.recovery, credentials.as_ref())?.transport;
        let store = self.evidence_stores.configure(&config.warehouse, credentials.as_ref())?;
        let contract = Contract::new(
            config.pipeline.clone(),
            config.shard.clone(),
            store.destination().into(),
            store.catalog().digest().into(),
        )?;
        let reader = EvidenceReader::new(store, Arc::new(ProjectionIndexes::new(recovery.clone())));
        Ok(EvidenceService { recovery, contract, reader })
    }
}
impl EvidenceService {
    /// The deadline and cancellation bound caller wait, including recovery proof reads. Native transports retain
    /// their own bounded request lifecycle; this API makes no claim that a warehouse query was cancelled remotely.
    pub async fn read(
        &self,
        request: &EvidenceRequest,
        cancellation: &CollectionCancellation,
    ) -> Result<EvidenceCapture> {
        request.validate()?;
        let reading = async {
            let recovery = ObjectRecoveryStore::open(self.recovery.clone(), self.contract.clone()).await?;
            let range = reopen(&recovery, &request.range_id).await?;
            self.reader.read_capture(&range, &request.families, request.budget).await
        };
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => Err(CollectionStop::Cancelled.error()),
            result = tokio::time::timeout(Duration::from_millis(u64::from(request.budget.timeout_ms)), reading) => {
                result.map_err(|_| CollectionStop::TimedOut.error())?
            }
        }
    }
}

fn invalid() -> culprit_core::Error {
    DeliveryProblem::InvalidContract.error()
}

#[cfg(test)]
mod tests;
