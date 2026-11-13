//! The sole destination selection point. Secret references are resolved only when a sink is opened.
use super::config::TargetConfig;
use culprit_auth::CredentialResolver;
use culprit_collector::{
    analytical::sink::AnalyticalSink,
    delivery::{
        contract::Contract, driver::DeliveryPolicy, object_store::transport::ObjectTransport, recovery::RecoveryStore,
    },
};
use culprit_core::Result;
use std::{path::Path, sync::Arc};

pub(super) trait SinkFactory: Send + Sync {
    fn destination(&self) -> &str;
    fn schema_digest(&self) -> &str;
    fn delivery_policy(&self) -> DeliveryPolicy {
        DeliveryPolicy::default()
    }
    fn staging_scope(&self) -> Option<&str> {
        None
    }
    fn open(
        self: Box<Self>,
        contract: Contract,
        recovery: Arc<dyn RecoveryStore>,
        scratch: &Path,
    ) -> Result<AnalyticalSink>;
}
pub(super) fn configure(
    target: &TargetConfig,
    credentials: Arc<dyn CredentialResolver>,
    validation: Arc<dyn ObjectTransport>,
) -> Result<Box<dyn SinkFactory>> {
    match target {
        #[cfg(feature = "store-adx")]
        TargetConfig::Adx { query, credential, delivery } => Ok(Box::new(adx::Destination::new(
            query,
            credentials.bearer(credential)?,
            delivery,
            credentials.bearer(&delivery.credential)?,
            validation,
        )?)),
        #[cfg(feature = "store-clickhouse")]
        TargetConfig::ClickHouse { query, credential, write } => {
            Ok(Box::new(clickhouse::Destination::new(query, credential, *write, credentials, validation)?))
        }
    }
}

#[cfg(feature = "store-adx")]
mod adx {
    use super::super::config::{AdxDeliveryConfig, AdxDeliveryTransport};
    use super::*;
    use culprit_blob_azure::AzureBlobTransport;
    use culprit_store_adx::{
        catalog::SchemaCatalog,
        client::{AdxClient, AdxConfig, QueryTransport},
        ingestion::IngestionTransport,
        queued::QueuedDispatcher,
        ready::ReadyDispatcher,
    };
    // ADX batches for up to five minutes by default. Allow that window before emitting another physical attempt.
    const VISIBILITY_TIMEOUT_MS: u32 = 6 * 60_000;
    pub(super) struct Destination {
        query: Arc<AdxClient>,
        ingestion: Arc<dyn IngestionTransport>,
        validation: Arc<dyn ObjectTransport>,
        catalog: SchemaCatalog,
        staging_scope: Option<String>,
    }
    impl Destination {
        pub(super) fn new(
            query: &AdxConfig,
            credential: Arc<dyn culprit_auth::BearerTokenProvider>,
            delivery: &AdxDeliveryConfig,
            delivery_credential: Arc<dyn culprit_auth::BearerTokenProvider>,
            validation: Arc<dyn ObjectTransport>,
        ) -> Result<Self> {
            let (ingestion, staging_scope): (Arc<dyn IngestionTransport>, _) = match &delivery.transport {
                AdxDeliveryTransport::Queued { endpoint } => {
                    (Arc::new(QueuedDispatcher::new(query, endpoint.as_deref(), delivery_credential)?), None)
                }
                AdxDeliveryTransport::EventGrid { ready } => {
                    let transport = Arc::new(AzureBlobTransport::new(ready.clone(), delivery_credential)?);
                    (Arc::new(ReadyDispatcher::new(transport)), Some(ready.access_scope()?))
                }
            };
            Ok(Self {
                query: Arc::new(AdxClient::new(query.clone(), credential)?),
                ingestion,
                validation,
                staging_scope,
                catalog: SchemaCatalog::new()?,
            })
        }
    }
    impl SinkFactory for Destination {
        fn delivery_policy(&self) -> DeliveryPolicy {
            DeliveryPolicy { visibility_timeout_ms: Some(VISIBILITY_TIMEOUT_MS), ..DeliveryPolicy::default() }
        }
        fn staging_scope(&self) -> Option<&str> {
            self.staging_scope.as_deref()
        }
        fn destination(&self) -> &str {
            self.query.destination()
        }
        fn schema_digest(&self) -> &str {
            self.catalog.digest()
        }
        fn open(
            self: Box<Self>,
            contract: Contract,
            recovery: Arc<dyn RecoveryStore>,
            scratch: &Path,
        ) -> Result<AnalyticalSink> {
            culprit_store_adx::sink::new(contract, recovery, self.validation, self.ingestion, self.query, scratch)
        }
    }
}

#[cfg(feature = "store-clickhouse")]
mod clickhouse {
    use super::*;
    use culprit_store_clickhouse::{
        catalog::SchemaCatalog,
        client::{ClickHouseConfig, Credentials},
        ingestion::{ClickHouseIngestor, WritePolicy},
    };
    pub(super) struct Destination {
        id: String,
        query: ClickHouseConfig,
        profile: String,
        credentials: Arc<dyn CredentialResolver>,
        validation: Arc<dyn ObjectTransport>,
        catalog: SchemaCatalog,
        write: WritePolicy,
    }
    impl Destination {
        pub(super) fn new(
            query: &ClickHouseConfig,
            profile: &str,
            write: WritePolicy,
            credentials: Arc<dyn CredentialResolver>,
            validation: Arc<dyn ObjectTransport>,
        ) -> Result<Self> {
            credentials.validate_password(profile)?;
            write.validate()?;
            Ok(Self {
                id: query.destination_id()?,
                query: query.clone(),
                profile: profile.into(),
                credentials,
                validation,
                catalog: SchemaCatalog::new()?,
                write,
            })
        }
    }
    impl SinkFactory for Destination {
        fn destination(&self) -> &str {
            &self.id
        }
        fn schema_digest(&self) -> &str {
            self.catalog.digest()
        }
        fn open(
            self: Box<Self>,
            contract: Contract,
            recovery: Arc<dyn RecoveryStore>,
            scratch: &Path,
        ) -> Result<AnalyticalSink> {
            let credential = self.credentials.password(&self.profile)?;
            let ingestion = Arc::new(ClickHouseIngestor::new(
                self.query,
                Credentials::new(credential.username, credential.password)?,
                self.write,
            )?);
            let query = ingestion.query_transport();
            culprit_store_clickhouse::sink::new(contract, recovery, self.validation, ingestion, query, scratch)
        }
    }
}
