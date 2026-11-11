//! Local observation artifacts. A manifest is incomplete until source verification and file sealing finish.
use crate::{collection_references::ObservationReferences, durable_fs};
use chrono::Utc;
use culprit_core::{
    Error, Result, t,
    telemetry::{
        CaptureIdentity, CollectedFile, CollectionDescription, CollectionManifest, CollectionRequest, CollectionStatus,
        CollectionStop, EXPORT_FORMAT_VERSION, ExportedRecord, NativeRecordIdentity, ReferenceCoverage,
        TelemetryRecord,
    },
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Write},
    path::{Path, PathBuf},
};

struct OutputFile {
    handle: Option<File>,
    digest: Sha256,
    seen_rows: HashSet<NativeRecordIdentity>,
}

struct EncodedPage {
    bytes: Vec<u8>,
    records: Vec<TelemetryRecord>,
}

pub(crate) struct ObservationFiles {
    directory: PathBuf,
    pub(crate) manifest: CollectionManifest,
    outputs: Vec<OutputFile>,
    source_rechecked: bool,
    references: ObservationReferences,
}

impl ObservationFiles {
    pub(crate) fn create(directory: &Path, request: CollectionRequest, capture: CaptureIdentity) -> Result<Self> {
        let streams = request.collection().streams();
        durable_fs::create_directory(directory).map_err(file_error)?;
        let mut writer = Self {
            directory: directory.into(),
            manifest: CollectionManifest {
                format_version: EXPORT_FORMAT_VERSION,
                capture: capture.clone(),
                started_at: Utc::now().to_rfc3339(),
                finished_at: None,
                request,
                description: None,
                status: CollectionStatus::Incomplete(CollectionStop::Pending),
                source_queries: 0,
                read_metrics: None,
                directory_synced: cfg!(unix),
                files: streams
                    .iter()
                    .map(|&stream| {
                        Ok(CollectedFile {
                            batch_id: culprit_core::telemetry::batch_id(&capture.observation_id, stream)?,
                            stream,
                            semantics: stream.semantics(),
                            file: format!("{}.part", stream.file_name()),
                            status: CollectionStatus::Incomplete(CollectionStop::Pending),
                            error_code: None,
                            rows: 0,
                            bytes: 0,
                            pages: 0,
                            sha256: None,
                        })
                    })
                    .collect::<Result<Vec<_>>>()?,
                references: ReferenceCoverage::default(),
            },
            outputs: vec![],
            source_rechecked: false,
            references: ObservationReferences::default(),
        };
        writer.persist()?;
        for &stream in streams {
            writer.outputs.push(OutputFile {
                handle: Some(
                    durable_fs::create_file(&directory.join(format!("{}.part", stream.file_name())))
                        .map_err(file_error)?,
                ),
                digest: Sha256::new(),
                seen_rows: HashSet::new(),
            });
        }
        durable_fs::sync_directory(directory).map_err(file_error)?;
        Ok(writer)
    }
    pub(crate) fn describe(&mut self, description: CollectionDescription) -> Result<()> {
        if !description.matches_request(&self.manifest.request) {
            return Err(Error::new(culprit_core::ErrorCode::QueryFailed, t!("collection.invalid_source"))
                .with_detail(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::InvalidPage)));
        }
        self.manifest.description = Some(description);
        self.persist()
    }

    pub(crate) fn begin_page(&mut self, index: usize) -> std::result::Result<(), CollectionStop> {
        let budget = &self.manifest.request.budget;
        if matches!(self.manifest.request.selection, culprit_core::telemetry::CollectionSelection::Snapshot { .. })
            && self.manifest.files.iter().map(|file| u64::from(file.rows)).sum::<u64>()
                >= u64::from(budget.max_snapshot_rows)
        {
            return Err(CollectionStop::RowLimit);
        }
        if self.manifest.files.iter().map(|file| file.pages).sum::<u32>() >= budget.max_pages {
            return Err(CollectionStop::PageLimit);
        }
        self.manifest.files[index].pages += 1;
        Ok(())
    }
    pub(crate) fn write_page(&mut self, index: usize, records: &[TelemetryRecord]) -> Result<Option<CollectionStop>> {
        let page = match self.encode_page(index, records) {
            Ok(page) => page,
            Err(reason) => return Ok(Some(reason)),
        };
        self.append_page(index, &page.records, &page.bytes)?;
        Ok(None)
    }

    fn encode_page(&self, index: usize, records: &[TelemetryRecord]) -> Result<EncodedPage, CollectionStop> {
        let budget = &self.manifest.request.budget;
        if matches!(self.manifest.request.selection, culprit_core::telemetry::CollectionSelection::Snapshot { .. })
            && self.manifest.files.iter().map(|file| u64::from(file.rows)).sum::<u64>() + records.len() as u64
                > u64::from(budget.max_snapshot_rows)
        {
            return Err(CollectionStop::RowLimit);
        }
        if records.len() > budget.page_rows as usize {
            return Err(CollectionStop::RowLimit);
        }
        if !self.references.accepts_page(records) {
            return Err(CollectionStop::SourceChanged);
        }
        let mut buffer = durable_fs::LimitedBuffer::new(budget.page_bytes as usize);
        let stream = self.manifest.files[index].stream;
        let mut page_ids = HashSet::new();
        let source = self.manifest.description.as_ref().ok_or(CollectionStop::InvalidPage)?;
        let mut prepared = Vec::with_capacity(records.len());
        for record in records {
            let exported = ExportedRecord::new(source, &self.manifest.capture, record.clone())
                .map_err(|_| CollectionStop::InvalidPage)?;
            let record = &exported.record;
            if record.stream() != stream
                || !record.is_valid()
                || !page_ids.insert(record.native_identity())
                || self.outputs[index].seen_rows.contains(&record.native_identity())
            {
                return Err(CollectionStop::InvalidPage);
            }
            if serde_json::to_writer(&mut buffer, &exported).is_err() || buffer.write_all(b"\n").is_err() {
                return Err(if buffer.exceeded() { CollectionStop::ByteLimit } else { CollectionStop::InvalidPage });
            }
            prepared.push(exported.record);
        }
        let previous = self.manifest.files.iter().map(|file| u64::from(file.bytes)).sum::<u64>();
        if previous + buffer.len() as u64 > u64::from(budget.total_bytes) {
            return Err(CollectionStop::ByteLimit);
        }
        Ok(EncodedPage { bytes: buffer.into_bytes(), records: prepared })
    }

    fn append_page(&mut self, index: usize, records: &[TelemetryRecord], bytes: &[u8]) -> Result<()> {
        let output = &mut self.outputs[index];
        output
            .handle
            .as_mut()
            .ok_or_else(|| Error::internal(t!("collection.file_closed")))?
            .write_all(bytes)
            .map_err(file_error)?;
        output.digest.update(bytes);
        output.seen_rows.extend(records.iter().map(TelemetryRecord::native_identity));
        for record in records {
            self.references.record(record);
        }
        let file = &mut self.manifest.files[index];
        file.rows += records.len() as u32;
        file.bytes += bytes.len() as u32;
        Ok(())
    }
    pub(crate) fn seal(&mut self, index: usize) -> Result<()> {
        let output = &mut self.outputs[index];
        if let Some(mut handle) = output.handle.take() {
            handle.flush().map_err(file_error)?;
            handle.sync_all().map_err(file_error)?;
        }
        let file = &mut self.manifest.files[index];
        let name = file.stream.file_name();
        fs::rename(self.directory.join(&file.file), self.directory.join(name)).map_err(file_error)?;
        durable_fs::sync_directory(&self.directory).map_err(file_error)?;
        file.file = name.into();
        file.sha256 = Some(output.digest.clone().finalize().iter().map(|byte| format!("{byte:02x}")).collect());
        file.status = CollectionStatus::Complete;
        self.persist()
    }
    pub(crate) fn stop(&mut self, index: usize, reason: CollectionStop) {
        self.manifest.files[index].status = CollectionStatus::Incomplete(reason);
    }

    pub(crate) fn fail_stream(&mut self, index: usize, error: &Error) {
        self.stop(index, CollectionStop::from_error(error));
        self.manifest.files[index].error_code = Some(error.code);
    }

    pub(crate) fn confirm_source(&mut self, completion: culprit_core::telemetry::SourceCompletion) -> Result<()> {
        let initial =
            self.manifest.description.as_ref().ok_or_else(|| Error::internal(t!("collection.invalid_source")))?;
        let Some(description) = initial.completed(completion)? else {
            self.invalidate(CollectionStop::SourceChanged);
            return Ok(());
        };
        if !description.matches_request(&self.manifest.request) {
            return Err(CollectionStop::InvalidPage.error());
        }
        self.manifest.description = Some(description);
        self.source_rechecked = true;
        if !self
            .manifest
            .description
            .as_ref()
            .is_some_and(|source| source.observation.matches_file_counts(&self.manifest.files))
        {
            self.invalidate(CollectionStop::SourceChanged);
        }
        if let Some(source) = &self.manifest.description
            && !source.observation.covers_request()
        {
            let reason = match source.observation {
                culprit_core::telemetry::SourceObservation::Activity(_) => CollectionStop::SourceUnavailable,
                _ => CollectionStop::RetentionGap,
            };
            self.invalidate(reason);
        }
        Ok(())
    }
    pub(crate) fn invalidate(&mut self, reason: CollectionStop) {
        for file in &mut self.manifest.files {
            file.status = CollectionStatus::Incomplete(reason);
        }
    }
    pub(crate) fn finish(mut self, queries: u32) -> Result<CollectionManifest> {
        if self.manifest.read_metrics.as_ref().is_some_and(|metrics| !metrics.is_valid()) {
            self.manifest.read_metrics = None;
            for file in &mut self.manifest.files {
                if file.status == CollectionStatus::Complete {
                    file.status = CollectionStatus::Incomplete(CollectionStop::InvalidPage);
                }
            }
        }
        for output in &mut self.outputs {
            if let Some(mut handle) = output.handle.take() {
                handle.flush().map_err(file_error)?;
                handle.sync_all().map_err(file_error)?;
            }
        }
        self.manifest.source_queries = queries;
        self.manifest.finished_at = Some(Utc::now().to_rfc3339());
        self.manifest.references = self.references.coverage(self.source_rechecked, self.manifest.description.as_ref());
        self.manifest.status = self
            .manifest
            .files
            .iter()
            .find_map(|file| match file.status {
                CollectionStatus::Incomplete(reason) => Some(CollectionStatus::Incomplete(reason)),
                _ => None,
            })
            .unwrap_or(CollectionStatus::Complete);
        if matches!(self.manifest.status, CollectionStatus::Complete) && !self.manifest.references.complete {
            self.manifest.status = CollectionStatus::Incomplete(if !self.source_rechecked {
                CollectionStop::SourceChanged
            } else if matches!(
                self.manifest.request.collection(),
                culprit_core::telemetry::TelemetryCollection::Activity
                    | culprit_core::telemetry::TelemetryCollection::Snapshot(_)
            ) {
                CollectionStop::InvalidPage
            } else {
                CollectionStop::MissingDefinitions
            });
        }
        self.persist()?;
        Ok(self.manifest)
    }
    pub(crate) fn derive_activity(&mut self, before: &[culprit_core::telemetry::ActivityIncidentState]) -> Result<()> {
        use culprit_core::telemetry::*;
        let source = self.manifest.description.as_ref().ok_or_else(|| CollectionStop::InvalidPage.error())?;
        let update = ActivityIncidentUpdate::observe(
            source,
            &self.manifest.capture,
            &self.references.activity.observations,
            before,
        )?;
        for (stream, records) in [
            (
                TelemetryStream::ActivityIncidents,
                update.changes.into_iter().map(TelemetryRecord::Incident).collect::<Vec<_>>(),
            ),
            (
                TelemetryStream::ActivityCheckpoints,
                update.active.into_iter().map(TelemetryRecord::ActivityCheckpoint).collect(),
            ),
        ] {
            let index = self
                .manifest
                .files
                .iter()
                .position(|file| file.stream == stream)
                .ok_or_else(|| CollectionStop::InvalidPage.error())?;
            // Pack derived output with the exact envelope encoder, rather than duplicating its size policy.
            let mut offset = 0;
            loop {
                if let Err(reason) = self.begin_page(index) {
                    self.stop(index, reason);
                    break;
                }
                let mut count = (records.len() - offset).min(self.manifest.request.budget.page_rows as usize);
                while count > 1 && self.encode_page(index, &records[offset..offset + count]).is_err() {
                    count /= 2;
                }
                if let Some(reason) = self.write_page(index, &records[offset..offset + count])? {
                    self.stop(index, reason);
                    break;
                }
                offset += count;
                if offset == records.len() {
                    self.seal(index)?;
                    break;
                }
            }
        }
        Ok(())
    }
    fn persist(&self) -> Result<()> {
        let bytes = durable_fs::json_bytes(&self.manifest, 128 * 1024).map_err(file_error)?;
        durable_fs::publish(&self.directory.join("manifest.json"), &bytes).map_err(file_error)
    }
}

fn file_error(error: io::Error) -> Error {
    Error::internal(t!("collection.file_error", error = error.to_string()))
        .with_detail(culprit_core::ErrorDetail::CollectionStopped(CollectionStop::StorageFailed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use culprit_core::telemetry::CollectionBudget;

    fn request() -> CollectionRequest {
        CollectionRequest {
            selection: culprit_core::telemetry::CollectionSelection::QueryHistory {
                from: "2020-01-01T06:00:00Z".into(),
                until: "2020-01-01T07:00:00Z".into(),
            },
            budget: CollectionBudget::default(),
        }
    }
    #[test]
    fn paged_snapshots_stream_before_counts_exist_and_commit_only_with_matching_final_coverage() {
        use culprit_core::telemetry::*;
        for (reported, limit) in [(1, 2), (2, 2), (2, 1)] {
            let temporary = tempfile::TempDir::new().unwrap();
            let request = CollectionRequest {
                selection: CollectionSelection::Snapshot { dataset: SnapshotDataset::Operational },
                budget: CollectionBudget { max_snapshot_rows: limit, ..Default::default() },
            };
            let mut files = ObservationFiles::create(
                &temporary.path().join("observation"),
                request,
                CaptureIdentity { observation_id: durable_fs::random_id().unwrap(), revision: None },
            )
            .unwrap();
            files
                .describe(CollectionDescription {
                    database: culprit_core::DatabaseIdentity::server(
                        culprit_core::Engine::Mssql,
                        "sample",
                        "fixture.invalid",
                        1433,
                        None,
                    ),
                    source: "paged-fixture".into(),
                    continuity: SourceContinuity::Unverified,
                    observation: SourceObservation::PagedSnapshot(PagedSnapshotDescription {
                        dataset: SnapshotDataset::Operational,
                        observed_from: "2020-01-01T00:00:00Z".into(),
                        sources: vec!["fixture".into()],
                    }),
                })
                .unwrap();
            let record = |key: &str| {
                TelemetryRecord::Operational(OperationalObservation {
                    source: "fixture".into(),
                    key: key.into(),
                    fact: OperationalFact::Counter(NativeCounterSample::gauge(
                        key,
                        7,
                        CounterUnit::Count,
                        culprit_core::MeasurementScope::Database,
                        "native",
                        "count",
                    )),
                })
            };
            files.begin_page(0).unwrap();
            assert!(files.write_page(0, &[record("one")]).unwrap().is_none());
            if limit == 1 {
                assert_eq!(files.begin_page(0), Err(CollectionStop::RowLimit));
                assert_eq!(files.write_page(0, &[record("two")]).unwrap(), Some(CollectionStop::RowLimit));
            }
            assert_eq!(files.manifest.files[0].rows, 1);
            files.seal(0).unwrap();
            assert!(!files.source_rechecked);
            let completion = SourceCompletion::Snapshot(StateSnapshotDescription {
                dataset: SnapshotDataset::Operational,
                observed_from: "2020-01-01T00:00:00Z".into(),
                observed_until: "2020-01-01T00:00:01Z".into(),
                records: reported,
                sources: vec![StateSourceCoverage::collected("fixture".into(), reported)],
            });
            if reported == 1 {
                files.confirm_source(completion).unwrap();
                let manifest = files.finish(0).unwrap();
                assert_eq!(manifest.status, CollectionStatus::Complete);
                assert!(manifest.references.complete);
                assert!(matches!(manifest.description.unwrap().observation, SourceObservation::Snapshot(_)));
            } else if limit == 2 {
                files.confirm_source(completion).unwrap();
                assert_eq!(
                    files.finish(0).unwrap().status,
                    CollectionStatus::Incomplete(CollectionStop::SourceChanged)
                );
            } else {
                assert!(files.confirm_source(completion).is_err(), "final counts cannot exceed admitted row budget");
                let manifest = files.finish(0).unwrap();
                assert_ne!(manifest.status, CollectionStatus::Complete);
            }
        }
    }
    #[test]
    fn object_definition_stream_counts_and_seals_typed_versions_without_becoming_an_operational_sample() {
        use culprit_core::telemetry::*;
        let foreign_key: MssqlForeignKey = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/telemetry-foreign-key.json"
        )))
        .unwrap();
        let column: MssqlColumnDefinition = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/telemetry-column.json"
        )))
        .unwrap();
        for (dataset, source, fact, unavailable) in [
            (
                SnapshotDataset::ForeignKeys,
                "foreign_keys",
                ObjectDefinitionFact::MssqlForeignKey(Box::new(foreign_key)),
                0,
            ),
            (SnapshotDataset::Columns, "columns", ObjectDefinitionFact::MssqlColumn(Box::new(column)), 1),
        ] {
            let temporary = tempfile::TempDir::new().unwrap();
            let capture = CaptureIdentity { observation_id: durable_fs::random_id().unwrap(), revision: None };
            let request =
                CollectionRequest { selection: CollectionSelection::Snapshot { dataset }, budget: Default::default() };
            let directory = temporary.path().join("observation");
            let mut files = ObservationFiles::create(&directory, request, capture).unwrap();
            files
                .describe(CollectionDescription {
                    database: culprit_core::DatabaseIdentity::server(
                        culprit_core::Engine::Mssql,
                        "db",
                        "fixture.invalid",
                        1433,
                        None,
                    ),
                    source: format!("{source}-state"),
                    continuity: SourceContinuity::Unverified,
                    observation: SourceObservation::PagedSnapshot(PagedSnapshotDescription {
                        dataset,
                        observed_from: "2026-11-10T08:00:00Z".into(),
                        sources: vec![source.into()],
                    }),
                })
                .unwrap();
            let record = TelemetryRecord::ObjectDefinition(ObjectDefinitionRecord::new(source.into(), fact).unwrap());
            files.begin_page(0).unwrap();
            assert!(files.write_page(0, &[record]).unwrap().is_none());
            files.seal(0).unwrap();
            files
                .confirm_source(SourceCompletion::Snapshot(StateSnapshotDescription {
                    dataset,
                    observed_from: "2026-11-10T08:00:00Z".into(),
                    observed_until: "2026-11-10T08:00:01Z".into(),
                    records: 1,
                    sources: vec![StateSourceCoverage::collected(source.into(), 1)],
                }))
                .unwrap();
            let manifest = files.finish(0).unwrap();
            assert_eq!(manifest.status, CollectionStatus::Complete);
            assert!(manifest.references.complete);
            assert_eq!(manifest.references.unavailable_payloads, unavailable);
            assert_eq!(manifest.files[0].stream, TelemetryStream::ObjectDefinitions);
            assert_eq!(manifest.files[0].rows, 1);
            assert!(directory.join("objects.ndjson").is_file());
            let row: ExportedRecord =
                serde_json::from_str(&std::fs::read_to_string(directory.join("objects.ndjson")).unwrap()).unwrap();
            assert_eq!(row.record.stream(), TelemetryStream::ObjectDefinitions);
            assert!(row.record.is_valid());
        }
    }
    #[test]
    fn failed_final_manifest_commit_leaves_the_previous_manifest_incomplete() {
        let temporary = tempfile::TempDir::new().unwrap();
        let directory = temporary.path().join("observation");
        let mut files = ObservationFiles::create(
            &directory,
            request(),
            CaptureIdentity { observation_id: durable_fs::random_id().unwrap(), revision: None },
        )
        .unwrap();
        for index in 0..files.manifest.files.len() {
            files.seal(index).unwrap();
        }
        files
            .describe(CollectionDescription {
                database: culprit_core::DatabaseIdentity::server(
                    culprit_core::Engine::Mssql,
                    "sample",
                    "fixture.invalid",
                    1433,
                    None,
                ),
                source: "query-store".into(),
                observation: culprit_core::telemetry::SourceObservation::QueryHistory(
                    culprit_core::telemetry::QueryHistoryDescription {
                        interval: culprit_core::telemetry::TelemetryInterval {
                            native_id: "1".into(),
                            from: request().bounds().unwrap().0.to_rfc3339(),
                            until: request().bounds().unwrap().1.to_rfc3339(),
                        },
                        dimensions: [("replica_group_id".into(), vec!["1".into()])].into(),
                        native_properties: [
                            ("capture_mode".into(), "AUTO".into()),
                            ("waits_capture_enabled".into(), (true).to_string()),
                        ]
                        .into(),
                    },
                ),
                continuity: culprit_core::telemetry::SourceContinuity::Unverified,
            })
            .unwrap();
        files.confirm_source(culprit_core::telemetry::SourceCompletion::Verified).unwrap();
        fs::write(directory.join(".manifest.json.new"), b"injected commit failure").unwrap();
        assert!(files.finish(0).is_err());
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(directory.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["status"]["state"], "incomplete");
    }
    #[test]
    fn sealed_files_alone_cannot_promote_source_completeness() {
        let temporary = tempfile::TempDir::new().unwrap();
        let directory = temporary.path().join("observation");
        let mut files = ObservationFiles::create(
            &directory,
            request(),
            CaptureIdentity { observation_id: durable_fs::random_id().unwrap(), revision: None },
        )
        .unwrap();
        for index in 0..files.manifest.files.len() {
            files.seal(index).unwrap();
        }
        let manifest = files.finish(0).unwrap();
        assert_eq!(manifest.status, CollectionStatus::Incomplete(CollectionStop::SourceChanged));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&directory).unwrap().permissions().mode() & 0o777, 0o700);
            assert_eq!(fs::metadata(directory.join("manifest.json")).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}
