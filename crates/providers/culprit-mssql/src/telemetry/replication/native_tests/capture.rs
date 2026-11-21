use super::*;

pub(super) struct Capture {
    pub(super) observation: Result<culprit_read::FrozenObservation>,
    pub(super) verification: Option<Result<bool>>,
    pub(super) shutdown: SourceShutdown,
    pub(super) scope: NativeReadContext,
    pub(super) accounting: Value,
    pub(super) queries: u32,
    pub(super) profile_elapsed_ms: f64,
    pub(super) source_elapsed_ms: f64,
    pub(super) close_elapsed_ms: f64,
}

pub(super) async fn open(fixture: &OwnedSourceFixture, budget: CollectionBudget) -> Result<Reader<MeasuredRead>> {
    let io = crate::test_support::measured_read(
        fixture.pool.clone(),
        fixture.port,
        fixture.password(),
        &fixture.database,
        budget,
    )
    .await?;
    Ok(Reader { io, host: "127.0.0.1".into(), port: fixture.port, owner: None, topology: None })
}

pub(super) async fn capture(fixture: &OwnedSourceFixture, budget: &CollectionBudget) -> Result<Capture> {
    let started = Instant::now();
    let mut reader = open(fixture, budget.clone()).await?;
    let scope = reader.io.scope().clone();
    let source_started = Instant::now();
    let observation = reader.observe(&request(budget.clone())).await;
    let verification = match &observation {
        Ok(observed) => Some(reader.verify(&observed.description).await),
        Err(_) => None,
    };
    // Close precedes assertions, receipt publication and any later experiment.
    let close_started = Instant::now();
    let shutdown = reader.close().await;
    let close_elapsed_ms = close_started.elapsed().as_secs_f64() * 1000.0;
    let source_elapsed_ms = source_started.elapsed().as_secs_f64() * 1000.0;
    if shutdown == SourceShutdown::Unconfirmed
        || observation
            .as_ref()
            .err()
            .is_some_and(|error| CollectionStop::from_error(error) == CollectionStop::QueryLimit)
    {
        assert_no_followup(&mut reader.io).await;
    }
    Ok(Capture {
        observation,
        verification,
        shutdown,
        scope,
        accounting: reader.io.report(),
        queries: reader.query_count(),
        profile_elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
        source_elapsed_ms,
        close_elapsed_ms,
    })
}

impl Capture {
    pub(super) fn assert_complete(&self, fixture: &OwnedSourceFixture, budget: &CollectionBudget) {
        assert_eq!(self.shutdown, SourceShutdown::Quiescent);
        assert!(self.accounting["measurement_complete"].as_bool().unwrap_or(false), "{}", self.accounting);
        assert!(matches!(&self.verification, Some(Ok(true))));
        let observed =
            self.observation.as_ref().unwrap_or_else(|error| panic!("native replication read failed: {error:?}"));
        let source = &observed.description;
        let header = source.observation.snapshot().expect("native replication snapshot");
        assert_eq!(source.target, self.scope.target);
        assert_eq!(source.partition, self.scope.partition);
        assert!(self.scope.has_qualified_local_owner() && source.partition_is_consistent());
        assert_eq!(source.target.database().unwrap().database, fixture.database);
        assert_eq!(header.dataset, SnapshotDataset::Replication);
        assert!(header.is_valid());
        assert_eq!(header.records as usize, observed.records.len());
        assert!(header.records <= budget.max_snapshot_rows);
        assert!(self.queries <= budget.max_queries);
        assert_eq!(header.sources.len(), 1, "a boxed fixture cannot qualify MI or Azure Geo families");
        assert_eq!(header.sources[0].source, crate::replication::Selection::AvailabilityGroup.source());
        assert_eq!(header.sources[0].records, header.records);
        if observed.records.is_empty() {
            assert_eq!(header.sources[0].state, culprit_core::SourceState::Empty);
            assert!(header.sources[0].complete_scope, "known native absence differs from an unavailable interface");
        }
        let capture = CaptureIdentity { observation_id: "owned-replication-cost".into(), revision: None };
        for record in &observed.records {
            let exported = ExportedRecord::new(source, &capture, record.clone()).unwrap();
            assert!(exported.record_id.is_some());
            let TelemetryRecord::Replication(row) = &exported.record else { panic!("replication row") };
            assert!(matches!(&row.fact, ReplicationFact::AvailabilityGroup(_)) && row.is_valid());
        }
        let metrics: ReadWorkMetrics = serde_json::from_value(self.accounting["source_read_metrics"].clone()).unwrap();
        assert!(metrics.is_valid() && !metrics.has_unconfirmed_work());
        assert_eq!(metrics.active_tasks, 0);
        assert!(metrics.templates.iter().all(|phase| phase.failures == 0));
    }

    pub(super) fn receipt(&self, label: &str, budget: &CollectionBudget, expected_ag: bool) -> Value {
        let observed = self.observation.as_ref().ok();
        json!({
            "schema_version":1,"case":label,"recipe":recipe(),"tested_budget":budget,
            "scope":self.scope,"profile_including_preparation_client_elapsed_ms":self.profile_elapsed_ms,
            "source_with_observer_and_close_client_elapsed_ms":self.source_elapsed_ms,
            "close_client_elapsed_ms":self.close_elapsed_ms,
            "description":observed.map(|frame|&frame.description),
            "records":observed.map(|frame|&frame.records),
            "error":self.observation.as_ref().err().map(|error|error.code),
            "stopped":self.observation.as_ref().err().map(CollectionStop::from_error),
            "verified":self.verification.as_ref().and_then(|result|result.as_ref().ok()),
            "verification_error":self.verification.as_ref().and_then(|result|result.as_ref().err()).map(|error|error.code),
            "shutdown":shutdown_state(self.shutdown),"source_queries":self.queries,
            "published":false,
            "source_completion_usable":observed.is_some() && matches!(&self.verification,Some(Ok(true)))
                && self.shutdown==SourceShutdown::Quiescent,
            "expanded_ag_select_expected":expected_ag,
            "expanded_ag_select_exercised":expanded_template(&self.accounting).is_some(),
            "visible_ag_rows":observed.map(|frame|frame.records.len()),
            "populated_ag_cost_qualified":false,"mi_geo_cost_qualified":false,
            "accounting":self.accounting,"production_admission":"unqualified",
            "native_cpu_io_cost_qualified":false,"cache_temperature":"uncontrolled",
        })
    }
}
