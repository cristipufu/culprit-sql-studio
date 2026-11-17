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
    let (mode, focus) = selection(request)?;
    pid(&focus.original.key.session.id)?;
    let mut request = request.clone();
    request.budget = match mode {
        reader::Mode::CurrentWork => current_work_budget(&request.budget),
        _ => bounded_budget(&request.budget),
    };
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
        let (mode, focus) = selection(request)?;
        let captured = reader::capture(
            &mut self.io,
            &self.host,
            self.port,
            pid(&focus.original.key.session.id)?,
            Some(focus),
            &request.budget,
            mode,
        )
        .await?;
        let frozen = export(captured.progress)?;
        if !frozen.description.matches_request(request) {
            return Err(CollectionStop::SourceChanged.error());
        }
        self.verified = Some(frozen.description.clone());
        Ok(frozen)
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

fn selection(request: &CollectionRequest) -> Result<(reader::Mode, &ActivityRequestFocus)> {
    let (mode, focus) = match &request.selection {
        CollectionSelection::MaintenanceProgress { focus } => (reader::Mode::Retained, focus),
        CollectionSelection::CurrentRequestWork { focus } if focus.is_current_request_work_focus() => {
            (reader::Mode::CurrentWork, focus)
        }
        _ => return Err(native::invalid()),
    };
    if focus.target.engine() != Engine::Postgres {
        return Err(native::invalid());
    }
    Ok((mode, focus))
}

fn export(captured: reader::CapturedProgress) -> Result<FrozenObservation> {
    let (context, coverage, result, native_source, logical_source, dataset, facts) = match captured {
        reader::CapturedProgress::Maintenance(snapshot) => (
            snapshot.context,
            snapshot.coverage,
            snapshot.result,
            snapshot.source,
            maintenance::SOURCE,
            SnapshotDataset::MaintenanceProgress,
            snapshot
                .observations
                .into_iter()
                .map(|point| Ok((point.key()?, OperationalFact::MaintenanceProgress(Box::new(point)))))
                .collect::<Result<Vec<_>>>()?,
        ),
        reader::CapturedProgress::CurrentWork(snapshot) => (
            snapshot.context,
            snapshot.coverage,
            snapshot.result,
            snapshot.source,
            copy::SOURCE,
            SnapshotDataset::CurrentRequestWork,
            snapshot
                .observations
                .into_iter()
                .map(|point| Ok((point.key()?, OperationalFact::CurrentRequestWork(Box::new(point)))))
                .collect::<Result<Vec<_>>>()?,
        ),
    };
    let records = facts
        .into_iter()
        .map(|(key, fact)| {
            TelemetryRecord::Operational(OperationalObservation { source: native_source.clone(), key, fact })
        })
        .collect::<Vec<_>>();
    let snapshot = StateSnapshotDescription {
        dataset,
        observed_from: context.observed_from,
        observed_until: context.observed_until,
        records: records.len() as u32,
        sources: vec![coverage],
    };
    let observation = match dataset {
        SnapshotDataset::CurrentRequestWork => SourceObservation::CurrentRequestWork {
            profile: CurrentRequestWorkProfile::PostgresCopy,
            snapshot,
            result: Box::new(result),
        },
        SnapshotDataset::MaintenanceProgress => {
            SourceObservation::MaintenanceProgress { snapshot, result: Box::new(result) }
        }
        _ => return Err(native::invalid()),
    };
    let description = CollectionDescription {
        target: context.target,
        partition: context.partition,
        source: logical_source.into(),
        continuity: SourceContinuity::Unverified,
        observation,
    };
    Ok(FrozenObservation { description, records })
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
