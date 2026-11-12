//! Cheap, bounded native resource history, independent of Query Store and application statements.
use super::{decode, description, resource_rows::ResourcePager};
use crate::conn::Pool;
use crate::read::{NativeRead, ReadError, ReadSession as Session};
use crate::resources::{self, Inventory, Window};
use async_trait::async_trait;
use culprit_core::{Result, telemetry::*};
use std::sync::Arc;

const ENCODED_ROW_ALLOWANCE: u32 = 4096;

pub(super) struct ResourceSource<R = Session> {
    request: CollectionRequest,
    host: String,
    port: u16,
    session: R,
    description: Option<CollectionDescription>,
    physical_incarnation: Option<String>,
    inventory: Option<Inventory>,
    pager: ResourcePager,
    progress: super::progress::PagedProgress,
}
impl ResourceSource {
    pub(super) fn new(pool: Arc<Pool>, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        Self::with_io(Session::new(pool, request.budget.clone()), request, host, port)
    }
}
impl<R: NativeRead> ResourceSource<R> {
    fn with_io(session: R, request: &CollectionRequest, host: String, port: u16) -> Result<Self> {
        request.validate()?;
        if request.collection() != TelemetryCollection::ResourceHistory {
            return Err(decode::malformed());
        }
        Ok(Self {
            request: request.clone(),
            host,
            port,
            session,
            description: None,
            physical_incarnation: None,
            inventory: None,
            pager: ResourcePager::default(),
            progress: super::progress::PagedProgress::new(request.collection()),
        })
    }
    async fn inventory(&mut self) -> Result<Inventory> {
        let (from, until) = self.request.bounds()?;
        let inventory = resources::inventory(&mut self.session, &Window::new(from, until)?).await?;
        if inventory.replica_role.is_some_and(|role| role != 0) {
            return Err(CollectionStop::SourceChanged.error());
        }
        Ok(inventory)
    }
}

#[async_trait]
impl<R: NativeRead> TelemetrySource for ResourceSource<R> {
    async fn describe(&mut self) -> Result<CollectionDescription> {
        self.progress.describing()?;
        description::ensure_supported(&mut self.session).await?;
        let (database, physical) = description::identity(&mut self.session, &self.host, self.port).await?;
        let inventory = self.inventory().await?;
        let description = CollectionDescription {
            target: database.into(),
            source: culprit_core::source_id::RESOURCES.into(),
            observation: SourceObservation::RetainedSamples(inventory.history.clone()),
            continuity: SourceContinuity::Unverified,
        };
        if !description.matches_request(&self.request) {
            return Err(decode::malformed());
        }
        self.physical_incarnation = Some(physical);
        self.inventory = Some(inventory);
        self.description = Some(description.clone());
        self.progress.described();
        Ok(description)
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.session.read_metrics()
    }
    fn query_count(&self) -> u32 {
        self.session.queries()
    }
    async fn read_page(&mut self, stream: TelemetryStream, cursor: Option<&str>) -> Result<CollectionRead> {
        self.progress.reading(stream, cursor)?;
        self.pager.check_cursor(cursor)?;
        let (from, until) = self.request.bounds()?;
        let rows_per_page =
            self.request.budget.page_rows.min((self.request.budget.page_bytes / ENCODED_ROW_ALLOWANCE).max(1));
        let window = Window::new(from, until)?;
        let rows = resources::windowed(&mut self.session, &window, cursor, rows_per_page).await;
        let result = match rows {
            Ok(rows) => self.pager.accept(rows, rows_per_page),
            Err(ReadError::Stopped(reason)) => Ok(CollectionRead::Stopped(reason)),
            Err(ReadError::Source(error)) => Err(error),
        };
        self.progress.read(stream, result)
    }
    async fn finish(&mut self) -> Result<culprit_core::telemetry::SourceCompletion> {
        self.progress.verifying()?;
        let (database, physical) = description::identity(&mut self.session, &self.host, self.port).await?;
        let current = self.inventory().await?;
        self.progress.verified();
        Ok(culprit_core::telemetry::SourceCompletion::verified_if(
            self.description.as_ref().is_some_and(|description| description.target.database() == Some(&database))
                && self.physical_incarnation.as_ref() == Some(&physical)
                && self.inventory.as_ref().is_some_and(|before| {
                    before.same_rows(&current) && self.pager.complete(before.history.expected_samples)
                }),
        ))
    }
    async fn close(&mut self) -> SourceShutdown {
        self.progress.close();
        self.session.close().await
    }
}

#[cfg(test)]
mod tests;
