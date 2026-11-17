//! Focused capture uses the same selected maintenance reader as Live, with one native owner and budget.
use super::*;
use culprit_read::{FrozenObservation, FrozenSource, ObservationReader};

struct Reader<R = ReadSession> {
    io: R,
    host: String,
    port: u16,
    verified: Option<CollectionDescription>,
}

pub(crate) fn source(connector: Connector, request: &CollectionRequest) -> Result<Box<dyn TelemetrySource>> {
    request.validate()?;
    request.subject.require_database()?;
    let CollectionSelection::MaintenanceProgress { focus } = &request.selection else { return Err(native::invalid()) };
    if focus.target.engine() != Engine::Postgres {
        return Err(native::invalid());
    }
    pid(&focus.original.key.session.id)?;
    let mut request = request.clone();
    request.budget = bounded_budget(&request.budget);
    let reader = Reader {
        host: connector.host.clone(),
        port: connector.port,
        io: ReadSession::new(connector, request.budget.clone()),
        verified: None,
    };
    Ok(Box::new(FrozenSource::new(reader, &request)?))
}

#[async_trait::async_trait]
impl<R: NativeRead> ObservationReader for Reader<R> {
    async fn observe(&mut self, request: &CollectionRequest) -> Result<FrozenObservation> {
        let CollectionSelection::MaintenanceProgress { focus } = &request.selection else {
            return Err(native::invalid());
        };
        let captured = capture_from(
            &mut self.io,
            &self.host,
            self.port,
            &focus.original.key.session.id,
            Some(focus),
            &request.budget,
        )
        .await?;
        let records = captured
            .observations
            .into_iter()
            .map(|row| {
                Ok(TelemetryRecord::Operational(OperationalObservation {
                    source: captured.source.clone(),
                    key: row.key()?,
                    fact: OperationalFact::MaintenanceProgress(Box::new(row)),
                }))
            })
            .collect::<Result<Vec<_>>>()?;
        let description = CollectionDescription {
            target: captured.context.target,
            partition: captured.context.partition,
            source: maintenance::SOURCE.into(),
            continuity: SourceContinuity::Unverified,
            observation: SourceObservation::MaintenanceProgress {
                snapshot: StateSnapshotDescription {
                    dataset: SnapshotDataset::MaintenanceProgress,
                    observed_from: captured.context.observed_from,
                    observed_until: captured.context.observed_until,
                    records: records.len() as u32,
                    sources: vec![captured.coverage],
                },
                result: Box::new(captured.result),
            },
        };
        if !description.matches_request(request) {
            return Err(CollectionStop::SourceChanged.error());
        }
        self.verified = Some(description.clone());
        Ok(FrozenObservation { description, records })
    }
    async fn verify(&mut self, description: &CollectionDescription) -> Result<bool> {
        Ok(self.verified.as_ref() == Some(description))
    }
    fn query_count(&self) -> u32 {
        self.io.queries()
    }
    fn read_metrics(&self) -> Option<ReadWorkMetrics> {
        self.io.metrics()
    }
    async fn close(&mut self) -> SourceShutdown {
        self.io.close().await
    }
}

#[cfg(test)]
pub(crate) fn fixture_source(io: super::test_support::Reader, request: &CollectionRequest) -> Box<dyn TelemetrySource> {
    Box::new(
        FrozenSource::new(Reader { io, host: "fixture.invalid".into(), port: 9, verified: None }, request).unwrap(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::progress::test_support::{self, Reader as NativeReader};
    fn frozen(io: NativeReader) -> Box<dyn TelemetrySource> {
        let request = CollectionRequest {
            subject: CollectionSubject::Database,
            selection: CollectionSelection::MaintenanceProgress { focus: Box::new(test_support::original_focus()) },
            budget: bounded_budget(&CollectionBudget::default()),
        };
        fixture_source(io, &request)
    }
    #[tokio::test]
    async fn native_owner_is_held_through_frozen_paging_and_closed_without_a_second_reader() {
        let mut source = frozen(test_support::scripted_reader());
        let description = source.describe().await.unwrap();
        assert!(description.partition_is_consistent());
        let queries = source.query_count();
        assert!(matches!(
            source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap(),
            CollectionRead::Page { next_cursor: None, .. }
        ));
        assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
        assert_eq!(source.query_count(), queries, "paging and verification cannot reacquire native work");
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert!(source.describe().await.is_err());
        assert_eq!(source.query_count(), queries);
    }
    #[tokio::test]
    async fn cancellation_and_unconfirmed_close_are_explicit_and_never_retried() {
        let mut io = test_support::scripted_reader();
        io.replies[3] = Err(CollectionStop::Cancelled.error());
        let mut source = frozen(io);
        assert_eq!(CollectionStop::from_error(&source.describe().await.unwrap_err()), CollectionStop::Cancelled);
        let queries = source.query_count();
        assert!(source.finish().await.is_err());
        assert_eq!(source.close().await, SourceShutdown::Quiescent);
        assert_eq!(source.query_count(), queries);
        let mut io = test_support::scripted_reader();
        io.unconfirmed = true;
        let mut source = frozen(io);
        source.describe().await.unwrap();
        assert_eq!(source.close().await, SourceShutdown::Unconfirmed);
    }

    #[tokio::test]
    async fn non_index_native_rows_keep_their_source_and_exact_identity_through_frozen_export() {
        for (empty, row, native_source) in [
            (1, test_support::vacuum_progress(180_000, &[]), vacuum::SOURCE),
            (2, test_support::analyze_progress(&[]), analyze::SOURCE),
            (3, test_support::cluster_progress("VACUUM FULL", &[]), cluster::SOURCE),
        ] {
            let mut source = frozen(test_support::progress_reader(180_000, empty, Some(row)));
            assert_eq!(source.query_count(), 0);
            let description = source.describe().await.unwrap();
            let SourceObservation::MaintenanceProgress { snapshot, result } = &description.observation else {
                panic!()
            };
            assert_eq!(snapshot.sources[0].source, native_source);
            assert_eq!(snapshot.sources[0].state, culprit_core::SourceState::Partial);
            assert_eq!(result.membership, FocusedRequestMembership::Observed);
            let CollectionRead::Page { records, next_cursor: None, .. } =
                source.read_page(TelemetryStream::OperationalSamples, None).await.unwrap()
            else {
                panic!()
            };
            assert_eq!(records.len(), 1);
            let capture = CaptureIdentity { observation_id: "manual-progress".into(), revision: None };
            let exported = ExportedRecord::new(&description, &capture, records[0].clone()).unwrap();
            let TelemetryRecord::Operational(row) = &exported.record else { panic!() };
            assert_eq!(row.source, native_source);
            let OperationalFact::MaintenanceProgress(point) = &row.fact else { panic!() };
            assert_eq!(point.actor, test_support::original_focus().original.key);
            assert_eq!(point.statement.as_ref(), Some(&test_support::original_focus().statement_identity));
            let queries = source.query_count();
            assert_eq!(source.finish().await.unwrap(), SourceCompletion::Verified);
            assert_eq!(source.query_count(), queries);
            assert_eq!(source.close().await, SourceShutdown::Quiescent);
        }
    }
}
