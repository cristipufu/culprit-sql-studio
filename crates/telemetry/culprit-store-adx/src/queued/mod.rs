//! Established Kusto queued ingestion through service-issued storage resources; no Event Hub is provisioned.
mod resources;
mod storage;
#[cfg(test)]
mod tests;

use crate::{
    authentication::{TokenProvider, kusto_origin},
    client::AdxConfig,
    http,
    ingestion::{IngestionBatch, IngestionTransport},
};
use async_trait::async_trait;
use culprit_core::{Result, telemetry::DeliveryProblem};
use resources::Resources;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::{Mutex, Semaphore};

const MAX_PARALLEL_DELIVERIES: usize = 2;

pub struct QueuedDispatcher {
    database: String,
    service: http::Service,
    resources: Mutex<Option<Arc<Resources>>>,
    next_resource: AtomicUsize,
    deliveries: Arc<Semaphore>,
}
impl QueuedDispatcher {
    pub fn new(query: &AdxConfig, endpoint: Option<&str>, credential: Arc<dyn TokenProvider>) -> Result<Self> {
        let endpoint = ingestion_endpoint(query, endpoint)?;
        Ok(Self {
            database: query.database.clone(),
            service: http::Service::new(&endpoint, credential)?,
            resources: Mutex::new(None),
            next_resource: AtomicUsize::new(0),
            deliveries: Arc::new(Semaphore::new(MAX_PARALLEL_DELIVERIES)),
        })
    }
    async fn resources(&self) -> Result<Arc<Resources>> {
        let mut cached = self.resources.lock().await;
        if let Some(resources) = cached.as_ref().filter(|resources| resources.usable()) {
            return Ok(resources.clone());
        }
        let resources = self
            .service
            .execute(|origin, token| {
                let locations = http::command(origin, token, ".get ingestion resources")?;
                let identity = http::command(origin, token, ".get kusto identity token")?;
                Resources::parse(&locations, &identity)
            })
            .await?;
        let resources = Arc::new(resources);
        *cached = Some(resources.clone());
        Ok(resources)
    }
}
#[async_trait]
impl IngestionTransport for QueuedDispatcher {
    async fn submit(&self, batch: Arc<IngestionBatch>) -> Result<()> {
        if batch.bytes() == 0 {
            return Ok(());
        }
        let permit = self.deliveries.clone().acquire_owned().await.map_err(|_| DeliveryProblem::Transport.error())?;
        let resources = self.resources().await?;
        let index = self.next_resource.fetch_add(1, Ordering::Relaxed);
        let database = self.database.clone();
        // The native owner retains the encoded file, concurrency slot and resource credentials through completion.
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let result = storage::deliver(&resources, index, &database, &batch);
            if result.is_err() {
                resources.invalidate();
            }
            result
        })
        .await
        .map_err(|_| DeliveryProblem::Transport.error())?
    }
}

fn ingestion_endpoint(query: &AdxConfig, explicit: Option<&str>) -> Result<String> {
    query.destination_id()?;
    if let Some(endpoint) = explicit {
        return kusto_origin(endpoint);
    }
    let origin = kusto_origin(&query.endpoint)?;
    let host = origin.strip_prefix("https://").ok_or_else(|| DeliveryProblem::InvalidContract.error())?;
    if host.starts_with("ingest-") {
        return Err(DeliveryProblem::InvalidContract.error());
    }
    Ok(format!("https://ingest-{host}"))
}
