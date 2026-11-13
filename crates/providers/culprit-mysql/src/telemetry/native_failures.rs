//! The explicit shared-instance route freezes one bounded retained buffer before delivering pages.
use crate::{
    connect::{Connector, Scope},
    native::{self, error_log, error_log::context},
    native_failures,
    read::{OwnedRead, ReadSession},
    sources,
};
use culprit_core::{Engine, Error, ErrorCode, Result, telemetry::*};
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};
use std::sync::Arc;

pub(super) fn source(connector: Arc<Connector>, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    expected_resource(request)?;
    Ok(Box::new(FrozenSource::new(
        Reader { io: ReadSession::new(connector, request.budget.clone(), Scope::Server), context: None },
        request,
    )?))
}

fn expected_resource(request: &CollectionRequest) -> Result<&SharedResourceIdentity> {
    if request.collection() == TelemetryCollection::Snapshot(SnapshotDataset::NativeFailures)
        && let CollectionSubject::Shared { resource } = &request.subject
        && resource.engine == Engine::Mysql
        && resource.kind == SharedResourceKind::Instance
    {
        return Ok(resource);
    }
    Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")))
}

struct Reader<R: OwnedRead> {
    io: R,
    context: Option<context::Context>,
}

#[async_trait::async_trait]
impl<R: OwnedRead> ObservationReader for Reader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let resource = expected_resource(request)?;
        let limit = error_log::MAX_ROWS.min(request.budget.max_snapshot_rows as usize);
        let captured = native_failures::capture(&mut self.io, limit, Some(resource)).await?;
        let source_name = sources::NATIVE_FAILURES.name;
        let description = CollectionDescription {
            target: CollectionTarget::Shared(captured.context.resource.clone()),
            source: "mysql-error-log".into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::NativeFailures,
                observed_from: captured.observed_from,
                observed_until: captured.observed_until,
                records: captured.observations.rows.len() as u32,
                sources: vec![StateSourceCoverage::observed(
                    source_name.into(),
                    captured.observations.rows.len() as u32,
                    !captured.observations.truncated,
                )],
            }),
        };
        let records = captured
            .observations
            .rows
            .into_iter()
            .map(|observation| {
                Ok(TelemetryRecord::Operational(OperationalObservation {
                    source: source_name.into(),
                    key: observation.key()?,
                    fact: OperationalFact::MysqlErrorLog(Box::new(observation)),
                }))
            })
            .collect::<Result<_>>()?;
        self.context = Some(captured.context);
        Ok(FrozenObservation { description, records })
    }

    async fn verify(&mut self, _: &CollectionDescription) -> Result<bool> {
        let before = self.context.as_ref().ok_or_else(native::invalid)?;
        let (after, _) = context::read(&mut self.io).await?;
        Ok(before == &after)
    }

    fn query_count(&self) -> u32 {
        self.io.query_count()
    }

    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.read_metrics()
    }

    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}

pub(crate) async fn discover(
    connector: Arc<Connector>,
    request: &ResourceDiscoveryRequest,
) -> Result<Vec<CollectionResource>> {
    request.budget.validate()?;
    if request.kind != SharedResourceKind::Instance {
        return Err(Error::new(ErrorCode::Unsupported, culprit_core::t!("collection.unsupported")));
    }
    let mut reader = ReadSession::new(connector, request.budget.clone(), Scope::Server);
    discover_from(&mut reader, request).await
}

async fn discover_from<R: OwnedRead>(
    reader: &mut R,
    request: &ResourceDiscoveryRequest,
) -> Result<Vec<CollectionResource>> {
    let result = async {
        let (before, _) = context::read(reader).await?;
        let (after, observed_at) = context::read(reader).await?;
        if before != after {
            return Err(CollectionStop::SourceChanged.error());
        }
        let resources = vec![CollectionResource {
            identity: before.resource,
            collections: vec![TelemetryCollection::Snapshot(SnapshotDataset::NativeFailures)],
            observed_at,
        }];
        request.validate_result(&resources)?;
        Ok(resources)
    }
    .await;
    if reader.close().await == SourceShutdown::Unconfirmed {
        return Err(CollectionStop::CancellationUnconfirmed.error());
    }
    result
}

#[cfg(test)]
mod tests;
