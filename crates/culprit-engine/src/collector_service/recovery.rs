//! Recovery transport composition owns its authentication; it never borrows the source or warehouse identity.
use super::config::RecoveryConfig;
use culprit_auth::CredentialResolver;
use culprit_blob_azure::AzureBlobTransport;
use culprit_collector::delivery::blob_store::transport::BlobTransport;
use culprit_core::Result;
use std::sync::Arc;

impl RecoveryConfig {
    pub(super) fn transport(&self, credentials: &dyn CredentialResolver) -> Result<Arc<dyn BlobTransport>> {
        match self {
            Self::AzureBlob { storage, credential } => {
                let credential = credentials.bearer(credential)?;
                Ok(Arc::new(AzureBlobTransport::new(storage.clone(), credential)?))
            }
        }
    }
}
