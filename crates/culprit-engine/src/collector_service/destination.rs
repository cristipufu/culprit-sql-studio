//! The sole destination selection point. Secret references are resolved only when a sink is opened.
use super::config::TargetConfig;
use culprit_auth::CredentialResolver;
use culprit_collector::{
    analytical::sink::AnalyticalSink,
    delivery::{contract::Contract, object_store::transport::ObjectTransport, recovery::RecoveryStore},
};
use culprit_core::Result;
use std::{path::Path, sync::Arc};

pub(super) trait SinkFactory: Send + Sync {
    fn destination(&self) -> &str;
    fn schema_digest(&self) -> &str;
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
        TargetConfig::Adx { query, ready, credential, ready_credential } => {
            let _ = validation;
            Ok(Box::new(adx::Destination::new(
                query,
                ready,
                credentials.bearer(credential)?,
                credentials.bearer(ready_credential)?,
            )?))
        }
        #[cfg(feature = "store-clickhouse")]
        TargetConfig::ClickHouse { query, credential } => {
            Ok(Box::new(clickhouse::Destination::new(query, credential, credentials, validation)?))
        }
    }
}

#[cfg(feature = "store-adx")]
mod adx {
    use super::*;
    use culprit_blob_azure::{AzureBlobConfig, AzureBlobTransport};
    use culprit_store_adx::{
        catalog::SchemaCatalog,
        client::{AdxClient, AdxConfig, QueryTransport},
    };
    pub(super) struct Destination {
        query: Arc<AdxClient>,
        ready: Arc<AzureBlobTransport>,
        catalog: SchemaCatalog,
        staging_scope: String,
    }
    impl Destination {
        pub(super) fn new(
            query: &AdxConfig,
            ready: &AzureBlobConfig,
            credential: Arc<dyn culprit_auth::BearerTokenProvider>,
            ready_credential: Arc<dyn culprit_auth::BearerTokenProvider>,
        ) -> Result<Self> {
            Ok(Self {
                staging_scope: ready.access_scope()?,
                query: Arc::new(AdxClient::new(query.clone(), credential.clone())?),
                ready: Arc::new(AzureBlobTransport::new(ready.clone(), ready_credential)?),
                catalog: SchemaCatalog::new()?,
            })
        }
    }
    impl SinkFactory for Destination {
        fn staging_scope(&self) -> Option<&str> {
            Some(&self.staging_scope)
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
            culprit_store_adx::sink::new(contract, recovery, self.ready, self.query, scratch)
        }
    }
}

#[cfg(feature = "store-clickhouse")]
mod clickhouse {
    use super::*;
    use culprit_store_clickhouse::{
        catalog::SchemaCatalog,
        client::{ClickHouseClient, ClickHouseConfig, Credentials},
    };
    pub(super) struct Destination {
        id: String,
        query: ClickHouseConfig,
        profile: String,
        credentials: Arc<dyn CredentialResolver>,
        validation: Arc<dyn ObjectTransport>,
        catalog: SchemaCatalog,
    }
    impl Destination {
        pub(super) fn new(
            query: &ClickHouseConfig,
            profile: &str,
            credentials: Arc<dyn CredentialResolver>,
            validation: Arc<dyn ObjectTransport>,
        ) -> Result<Self> {
            credentials.validate_password(profile)?;
            Ok(Self {
                id: query.destination_id()?,
                query: query.clone(),
                profile: profile.into(),
                credentials,
                validation,
                catalog: SchemaCatalog::new()?,
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
            let client = Arc::new(ClickHouseClient::new(
                self.query,
                Credentials::new(credential.username, credential.password)?,
            )?);
            culprit_store_clickhouse::sink::new(contract, recovery, self.validation, client, scratch)
        }
    }
}
