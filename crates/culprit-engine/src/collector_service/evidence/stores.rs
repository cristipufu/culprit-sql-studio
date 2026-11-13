//! Native stores own their query configuration and credentials. Common readers receive only read capabilities.
use super::*;
use culprit_auth::CredentialResolver;
use culprit_collector::analytical::read_store::AnalyticalReadStore;
use serde_json::Value;
use std::collections::BTreeMap;

pub trait EvidenceStoreProvider: Send + Sync {
    fn name(&self) -> &str;
    fn configure(
        &self,
        options: &Value,
        credential: &str,
        credentials: &dyn CredentialResolver,
    ) -> Result<Arc<dyn AnalyticalReadStore>>;
}

#[derive(Default)]
pub(in crate::collector_service) struct StoreRegistry(BTreeMap<String, Arc<dyn EvidenceStoreProvider>>);
impl StoreRegistry {
    pub fn builtins() -> Self {
        let providers: Vec<Arc<dyn EvidenceStoreProvider>> = vec![
            #[cfg(feature = "store-adx")]
            Arc::new(Adx),
            #[cfg(feature = "store-clickhouse")]
            Arc::new(ClickHouse),
        ];
        Self(providers.into_iter().map(|provider| (provider.name().into(), provider)).collect())
    }
    pub fn register(&mut self, provider: Arc<dyn EvidenceStoreProvider>) -> Result<()> {
        let name = provider.name();
        if !super::super::config::valid_provider_name(name) || self.0.contains_key(name) {
            return Err(invalid());
        }
        self.0.insert(name.into(), provider);
        Ok(())
    }
    pub fn configure(
        &self,
        target: &EvidenceStoreConfig,
        credentials: &dyn CredentialResolver,
    ) -> Result<Arc<dyn AnalyticalReadStore>> {
        self.0.get(&target.provider).ok_or_else(invalid)?.configure(&target.options, &target.credential, credentials)
    }
}

#[cfg(feature = "store-adx")]
struct Adx;
#[cfg(feature = "store-adx")]
impl EvidenceStoreProvider for Adx {
    fn name(&self) -> &str {
        "adx"
    }
    fn configure(
        &self,
        options: &Value,
        credential: &str,
        credentials: &dyn CredentialResolver,
    ) -> Result<Arc<dyn AnalyticalReadStore>> {
        let query = serde_json::from_value(options.clone()).map_err(|_| invalid())?;
        let client = Arc::new(culprit_store_adx::client::AdxClient::new(query, credentials.bearer(credential)?)?);
        Ok(Arc::new(culprit_store_adx::evidence::AdxReadStore::new(client)?))
    }
}

#[cfg(feature = "store-clickhouse")]
struct ClickHouse;
#[cfg(feature = "store-clickhouse")]
impl EvidenceStoreProvider for ClickHouse {
    fn name(&self) -> &str {
        "clickhouse"
    }
    fn configure(
        &self,
        options: &Value,
        credential: &str,
        credentials: &dyn CredentialResolver,
    ) -> Result<Arc<dyn AnalyticalReadStore>> {
        let query = serde_json::from_value(options.clone()).map_err(|_| invalid())?;
        let password = credentials.password(credential)?;
        let client = Arc::new(culprit_store_clickhouse::client::ClickHouseClient::new(
            query,
            culprit_store_clickhouse::client::Credentials::new(password.username, password.password)?,
        )?);
        Ok(Arc::new(culprit_store_clickhouse::evidence::ClickHouseReadStore::new(client)?))
    }
}
