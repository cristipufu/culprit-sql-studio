use super::*;

pub(super) struct Evidence {
    pub source: CollectionDescription,
    pub records: Vec<TelemetryRecord>,
    pub references: Vec<DiagnosticEvidenceRef>,
    pub sources: Vec<SourceOutcome>,
    pub complete: bool,
}
impl Evidence {
    pub fn runtime(pages: Option<&str>) -> Self {
        let mut counters = CachedRuntimeMetric::ALL
            .into_iter()
            .filter(|metric| metric.is_required())
            .map(|metric| (metric, distribution("17")))
            .collect::<BTreeMap<_, _>>();
        if let Some(pages) = pages {
            counters.insert(CachedRuntimeMetric::SpilledPages, distribution(pages));
        }
        let point = CachedStatementRuntime {
            context: context(),
            query_plan_hash: Some("0x0123456789ABCDEF".into()),
            executions: Some("17".into()),
            counters,
            last_execution_without_offset: Some("1900-01-01T00:00:00.003".into()),
            reset: culprit_core::ResetMarker::Unknown,
        };
        let row = TelemetryRecord::Operational(OperationalObservation {
            source: "native-cache".into(),
            key: point.context.identity.key().unwrap(),
            fact: OperationalFact::MssqlCachedRuntime(Box::new(point)),
        });
        Self::new(SnapshotDataset::CachedRuntime, vec![row])
    }
    pub fn definition() -> Self {
        let raw = "SELECT amount FROM dbo.orders WHERE id=9264";
        let point = MssqlCachedDefinition {
            context: context(),
            query_plan_hash: Some("0x0123456789ABCDEF".into()),
            statement: MssqlCachedPayload {
                source_utf16_bytes: Some((raw.encode_utf16().count() * 2).to_string()),
                native_sha256: Some(digest(raw.as_bytes())),
                payload: DefinitionPayload::sql(raw, Engine::Mssql),
            },
            estimated_plan: MssqlCachedPayload {
                source_utf16_bytes: None,
                native_sha256: None,
                payload: DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected },
            },
            module_parameters: None,
            relational_input_proof: None,
        };
        Self::new(
            SnapshotDataset::CachedDefinitions,
            vec![TelemetryRecord::ObjectDefinition(
                ObjectDefinitionRecord::new(
                    "native-cache".into(),
                    ObjectDefinitionFact::MssqlCachedDefinition(Box::new(point)),
                )
                .unwrap(),
            )],
        )
    }
    fn new(dataset: SnapshotDataset, records: Vec<TelemetryRecord>) -> Self {
        let mut database = culprit_core::DatabaseIdentity::server(Engine::Mssql, "db", "fixture.invalid", 1433, None);
        database.incarnation = Some("native-database-incarnation".into());
        let source = CollectionDescription {
            target: database.into(),
            source: "fixture-reader".into(),
            continuity: SourceContinuity::Unverified,
            partition: SourcePartition::InstanceLocal {
                member: Some(SharedResourceIdentity {
                    engine: Engine::Mssql,
                    kind: SharedResourceKind::Instance,
                    namespace: NativeIdentity::new("native-namespace"),
                    native: NativeIdentity::new("native-member"),
                }),
                epoch: Some("native-process".into()),
            },
            observation: SourceObservation::Snapshot(StateSnapshotDescription {
                dataset,
                observed_from: "2020-01-01T12:00:00Z".into(),
                observed_until: "2020-01-01T12:00:01Z".into(),
                records: records.len() as u32,
                sources: vec![StateSourceCoverage::collected("native-cache".into(), records.len() as u32)],
            }),
        };
        let mut result = Self { source, records, references: vec![], sources: vec![], complete: true };
        result.refresh();
        result
    }
    pub fn refresh(&mut self) {
        let snapshot = self.source.observation.snapshot().unwrap();
        let sequence = if snapshot.dataset == SnapshotDataset::CachedRuntime { "1" } else { "2" };
        let capture = CaptureIdentity {
            observation_id: digest(sequence.as_bytes())[..32].into(),
            revision: Some(CaptureRevision {
                outbox_id: "a".repeat(32),
                owner_epoch: "1".into(),
                sequence: sequence.into(),
            }),
        };
        self.references = self
            .records
            .iter()
            .enumerate()
            .map(|(index, record)| {
                let row = ExportedRecord::new(&self.source, &capture, record.clone()).unwrap();
                DiagnosticEvidenceRef::from_export(&self.source, &digest(b"original-manifest"), &row, index as u32)
                    .unwrap()
            })
            .collect();
        self.sources = snapshot
            .sources
            .iter()
            .map(|source| SourceOutcome {
                source: source.source.clone(),
                state: source.state,
                reason: None,
                reason_message: None,
            })
            .collect();
    }
    pub fn rows(&self) -> Vec<CompletedSpillRecord<'_>> {
        self.references
            .iter()
            .zip(&self.records)
            .map(|(reference, record)| CompletedSpillRecord { reference: reference.clone(), record })
            .collect()
    }
    pub fn view<'a>(&'a self, rows: &'a [CompletedSpillRecord<'a>]) -> CompletedSpillEvidence<'a> {
        CompletedSpillEvidence { source: &self.source, rows, sources: &self.sources, read_complete: self.complete }
    }
    pub fn change_definition(&mut self, change: impl FnOnce(&mut MssqlCachedDefinition)) {
        let TelemetryRecord::ObjectDefinition(row) = &self.records[0] else { panic!() };
        let mut fact = row.fact.clone();
        let ObjectDefinitionFact::MssqlCachedDefinition(point) = &mut fact else { panic!() };
        change(point);
        self.records[0] =
            TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(row.source.clone(), fact).unwrap());
        self.refresh();
    }
}
fn context() -> CachedStatementContext {
    CachedStatementContext {
        identity: CachedStatementIdentity {
            plan_handle: "0xaabbccdd".into(),
            sql_handle: "0xeeff0011".into(),
            statement_start_bytes: 0,
            statement_end_bytes: -1,
            created_without_offset: "2020-01-01T11:00:00.003".into(),
            plan_generation: "2".into(),
        },
        database_id: 7,
        query_hash: Some("0x0123456789ABCDEF".into()),
        origin: StatementOrigin::Application,
        monitoring_template_id: None,
        source_text_bytes: None,
        source_text_complete: false,
    }
}
fn distribution(value: &str) -> CachedCounterDistribution {
    CachedCounterDistribution { total: value.into(), last: value.into(), minimum: "0".into(), maximum: value.into() }
}

pub(super) fn report(runtime: &Evidence, definitions: Option<&Evidence>) -> CompletedSpillReport {
    let rows = runtime.rows();
    let definition_rows = definitions.map(Evidence::rows);
    explain(runtime.view(&rows), definitions.zip(definition_rows.as_ref()).map(|(input, rows)| input.view(rows)))
        .unwrap()
}
