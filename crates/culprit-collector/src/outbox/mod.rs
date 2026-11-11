//! One local owner, immutable capture artifacts and a transactional commit boundary.
mod archive;
mod artifacts;
mod baselines;
mod eviction;
mod journal;
mod runtime;
pub use runtime::{CaptureLease, Outbox, OutboxDelivery};
#[cfg(test)]
mod tests;

use crate::durable_fs;
use culprit_core::{Error, ErrorCode, Result, telemetry::*};
use std::{
    fs,
    path::{Path, PathBuf},
};

const IDENTITY_VERSION: u32 = 6;
const MANIFEST_LIMIT: usize = 128 * 1024;
const CAPTURE_ALLOWANCE: u64 = 256 * 1024;
const CAPTURE_ACCOUNTING: u64 = 4096;

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    version: u32,
    id: String,
    authority: RevisionAuthority,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
enum RevisionAuthority {
    Local,
    Recovery { store_id: String },
}

struct Files {
    root: PathBuf,
    journal: journal::Journal,
    _owner: durable_fs::ExclusiveFile,
    epoch: Option<i64>,
    recovered: bool,
    authority: RevisionAuthority,
}

struct Reservation {
    entry: journal::Capture,
    directory: PathBuf,
    baseline: Option<crate::collection_activity::ActivityBaseline>,
}

impl Files {
    pub fn create(path: &Path, limits: OutboxLimits) -> Result<Self> {
        Self::create_with_identity(
            path,
            limits,
            Identity { version: IDENTITY_VERSION, id: durable_fs::random_id()?, authority: RevisionAuthority::Local },
        )
    }
    fn create_remote(path: &Path, limits: OutboxLimits, store_id: &str) -> Result<Self> {
        if !crate::delivery::contract::valid_digest(store_id) {
            return Err(problem(OutboxProblem::Integrity));
        }
        Self::create_with_identity(
            path,
            limits,
            Identity {
                version: IDENTITY_VERSION,
                id: digest(store_id.as_bytes())[..32].to_owned(),
                authority: RevisionAuthority::Recovery { store_id: store_id.into() },
            },
        )
    }
    fn create_with_identity(path: &Path, limits: OutboxLimits, identity: Identity) -> Result<Self> {
        validate_limits(&limits)?;
        durable_fs::create_directory(path).map_err(storage)?;
        let owner_path = path.join("owner.lock");
        durable_fs::create_file(&owner_path).map_err(storage)?.sync_all().map_err(storage)?;
        let owner = lock(&owner_path)?;
        durable_fs::create_directory(&path.join("captures")).map_err(storage)?;
        durable_fs::create_directory(&path.join("baselines")).map_err(storage)?;
        let journal = journal::Journal::create(&path.join("journal.sqlite3"), &identity.id, limits)?;
        durable_fs::publish(&path.join("identity.json"), &durable_fs::json_bytes(&identity, 4096).map_err(storage)?)
            .map_err(storage)?;
        Ok(Self {
            root: path.into(),
            journal,
            _owner: owner,
            epoch: None,
            recovered: false,
            authority: identity.authority,
        })
    }
    pub fn open(path: &Path) -> Result<Self> {
        let owner = lock(&path.join("owner.lock"))?;
        let identity = read_identity(path)?;
        let journal = journal::Journal::open(&path.join("journal.sqlite3"), &identity.id, true)?;
        Ok(Self {
            root: path.into(),
            journal,
            _owner: owner,
            epoch: None,
            recovered: false,
            authority: identity.authority,
        })
    }
    pub fn inspect(path: &Path, limit: u32, cursor: Option<&str>) -> Result<OutboxStatus> {
        let identity = read_identity(path)?;
        journal::Journal::open(&path.join("journal.sqlite3"), &identity.id, false)?.status(limit, cursor)
    }
    pub fn status(&self, limit: u32, cursor: Option<&str>) -> Result<OutboxStatus> {
        self.journal.status(limit, cursor)
    }

    pub fn recover(&mut self) -> Result<OutboxRecovery> {
        self.recovered = false;
        self.recover_baselines()?;
        self.validate_baselines()?;
        self.check_inventory()?;
        self.resume_eviction()?;
        let mut report = OutboxRecovery::default();
        for capture in self.journal.unfinished()? {
            let directory = self.directory(&capture.identity.observation_id);
            if capture.source_work == SourceWorkState::Quiescent && directory.join("manifest.json").is_file() {
                match artifacts::validate(&directory, &capture, None) {
                    Ok(validated) => {
                        self.journal.prepare_commit(&capture, &validated)?.commit().map_err(storage)?;
                        report.committed += 1;
                        continue;
                    }
                    Err(error) if is_incomplete(&directory, &capture) => {
                        let _ = error;
                    }
                    Err(_) => {
                        self.journal.fail(
                            &capture,
                            OutboxCaptureState::Quarantined,
                            Some(OutboxProblem::Integrity),
                            measure(&directory)?,
                        )?;
                        report.quarantined += 1;
                        continue;
                    }
                }
            }
            self.journal.fail(&capture, OutboxCaptureState::Interrupted, None, measure(&directory)?)?;
            report.interrupted += 1;
        }
        report.source_debts = self.journal.debts()?;
        self.recovered = true;
        Ok(report)
    }
    pub fn acknowledge_source_quiescence(&mut self, observation_id: &str) -> Result<()> {
        self.journal.acknowledge(observation_id)
    }
    pub fn verify(&mut self, observation_id: &str) -> Result<CollectionManifest> {
        self.validated(observation_id).map(|validated| validated.manifest)
    }
    fn validated(&mut self, observation_id: &str) -> Result<artifacts::ValidatedCapture> {
        let capture = self.journal.capture(observation_id)?;
        if capture.state != OutboxCaptureState::Ready {
            return Err(problem(OutboxProblem::Integrity));
        }
        let verified = artifacts::validate(&self.directory(observation_id), &capture, None).and_then(|validated| {
            self.journal.check_receipt(&capture, &validated)?;
            Ok(validated)
        });
        match verified {
            Ok(validated) => Ok(validated),
            Err(error) => {
                self.journal.fail(
                    &capture,
                    OutboxCaptureState::Quarantined,
                    Some(OutboxProblem::Integrity),
                    capture.charged_bytes,
                )?;
                Err(error)
            }
        }
    }
    pub fn prepare_delivery(
        &mut self,
        observation_id: &str,
        contract: crate::delivery::contract::Contract,
        advances: Vec<crate::delivery::evidence::StreamAdvance>,
    ) -> Result<crate::delivery::evidence::PreparedRange> {
        let validated = self.validated(observation_id)?;
        crate::delivery::evidence::PreparedRange::from_validated(
            &self.directory(observation_id),
            &validated.manifest,
            contract,
            advances,
        )
    }
    pub fn captured_range(&self, key: &CapturedRange) -> Result<Option<CaptureIdentity>> {
        self.journal.head(key)
    }

    pub fn replay(path: &Path, observation_id: &str, destination: &Path) -> Result<OutboxReplay> {
        let identity = read_identity(path)?;
        let journal = journal::Journal::open(&path.join("journal.sqlite3"), &identity.id, false)?;
        let capture = journal.capture(observation_id)?;
        if capture.state != OutboxCaptureState::Ready {
            return Err(problem(OutboxProblem::Integrity));
        }
        durable_fs::create_directory(destination).map_err(storage)?;
        let validated = artifacts::validate(&path.join("captures").join(observation_id), &capture, Some(destination))?;
        journal.check_receipt(&capture, &validated)?;
        durable_fs::publish(&destination.join("manifest.json"), &validated.manifest_bytes).map_err(storage)?;
        Ok(OutboxReplay {
            capture: capture.identity,
            batches: validated.manifest.files.len() as u32,
            bytes: validated.bytes,
            directory_synced: durable_fs::sync_directory(destination).map_err(storage)?,
        })
    }

    pub fn reserve(&mut self, connection: &str, request: &CollectionRequest) -> Result<Reservation> {
        if matches!(self.authority, RevisionAuthority::Recovery { .. }) && self.epoch.is_none() {
            return Err(problem(OutboxProblem::RecoveryRequired));
        }
        request.validate()?;
        if connection.is_empty() || connection.len() > 256 || connection.chars().any(char::is_control) {
            return Err(problem(OutboxProblem::Integrity));
        }
        if !self.recovered || !self.journal.unfinished()?.is_empty() {
            self.recover()?;
        }
        let baseline = if request.collection() == TelemetryCollection::Activity {
            match self.journal.latest(connection, TelemetryCollection::Activity)? {
                Some(previous) if previous.request.selection == request.selection => {
                    let validated = self.validated(&previous.identity.observation_id)?;
                    Some(crate::collection_activity::ActivityBaseline {
                        capture: validated.manifest.capture.clone(),
                        source: validated.manifest.description.ok_or_else(|| problem(OutboxProblem::Integrity))?,
                        states: validated.activity_states,
                    })
                }
                _ => None,
            }
        } else {
            None
        };
        let capture = self.journal.reserve(connection, request, &mut self.epoch)?;
        let directory = self.directory(&capture.identity.observation_id);
        Ok(Reservation { entry: capture, directory, baseline })
    }
    fn directory(&self, id: &str) -> PathBuf {
        self.root.join("captures").join(id)
    }
    fn begin_remote_epoch(&mut self, fence: &crate::delivery::recovery::Fence, high_sequence: u64) -> Result<()> {
        let RevisionAuthority::Recovery { store_id } = &self.authority else {
            return Err(problem(OutboxProblem::Integrity));
        };
        if store_id != fence.store_id() {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        if self.epoch.is_some() {
            return Err(DeliveryProblem::Fenced.error());
        }
        if !self.recovered {
            return Err(problem(OutboxProblem::RecoveryRequired));
        }
        if self.journal.debts()? > 0 {
            return Err(problem(OutboxProblem::SourceUnconfirmed));
        }
        if !self.journal.unfinished()?.is_empty() {
            return Err(problem(OutboxProblem::RecoveryRequired));
        }
        self.journal.begin_remote_epoch(fence.epoch(), high_sequence)?;
        self.epoch = Some(i64::try_from(fence.epoch()).map_err(|_| problem(OutboxProblem::Integrity))?);
        Ok(())
    }
    fn check_inventory(&self) -> Result<()> {
        let known = self.journal.ids()?;
        let mut seen = 0;
        for entry in fs::read_dir(self.root.join("captures")).map_err(storage)? {
            seen += 1;
            if seen > self.journal.limits.max_captures {
                return Err(problem(OutboxProblem::Integrity));
            }
            let entry = entry.map_err(storage)?;
            if !entry.file_type().map_err(storage)?.is_dir()
                || !entry.file_name().to_str().is_some_and(|id| known.contains(id))
            {
                return Err(problem(OutboxProblem::Integrity));
            }
        }
        Ok(())
    }
}

impl Files {
    fn finish(&mut self, reservation: Reservation) -> Result<()> {
        let Reservation { entry, directory, .. } = reservation;
        let capture = self.journal.capture(&entry.identity.observation_id)?;
        if !directory.exists() || is_incomplete(&directory, &capture) {
            return self.journal.fail(&capture, OutboxCaptureState::Incomplete, None, measure(&directory)?);
        }
        match artifacts::validate(&directory, &capture, None) {
            Ok(validated) => self.journal.prepare_commit(&capture, &validated)?.commit().map_err(storage),
            Err(error) => {
                self.journal.fail(
                    &capture,
                    OutboxCaptureState::Quarantined,
                    Some(OutboxProblem::Integrity),
                    measure(&directory)?,
                )?;
                Err(error)
            }
        }
    }
}

fn is_incomplete(directory: &Path, capture: &journal::Capture) -> bool {
    artifacts::manifest(directory).is_ok_and(|(manifest, _)| {
        manifest.capture == capture.identity
            && manifest.request == capture.request
            && !matches!(manifest.status, CollectionStatus::Complete)
    })
}
fn measure(directory: &Path) -> Result<u64> {
    if !directory.exists() {
        return Ok(CAPTURE_ACCOUNTING);
    }
    let mut bytes = CAPTURE_ACCOUNTING;
    let mut count = 0;
    let names: std::collections::BTreeSet<String> = TelemetryStream::ALL
        .into_iter()
        .flat_map(|stream| [stream.file_name().to_owned(), format!("{}.part", stream.file_name())])
        .chain(["manifest.json".into(), ".manifest.json.new".into()])
        .collect();
    for entry in fs::read_dir(directory).map_err(storage)? {
        count += 1;
        if count > TelemetryStream::ALL.len() * 2 + 2 {
            return Err(problem(OutboxProblem::Integrity));
        }
        let entry = entry.map_err(storage)?;
        if !entry.file_type().map_err(storage)?.is_file()
            || !entry.file_name().to_str().is_some_and(|name| names.contains(name))
        {
            return Err(problem(OutboxProblem::Integrity));
        }
        bytes = bytes
            .checked_add(entry.metadata().map_err(storage)?.len())
            .ok_or_else(|| problem(OutboxProblem::Integrity))?;
    }
    Ok(bytes)
}
fn read_identity(root: &Path) -> Result<Identity> {
    use std::io::Read;
    if !fs::symlink_metadata(root.join("captures")).map_err(storage)?.file_type().is_dir() {
        return Err(problem(OutboxProblem::Integrity));
    }
    let mut bytes = Vec::new();
    durable_fs::regular_file(&root.join("identity.json"))
        .map_err(storage)?
        .take(4097)
        .read_to_end(&mut bytes)
        .map_err(storage)?;
    let identity: Identity = serde_json::from_slice(&bytes).map_err(|_| problem(OutboxProblem::Integrity))?;
    if bytes.len() > 4096 || !valid_id(&identity.id) {
        return Err(problem(OutboxProblem::Integrity));
    }
    if identity.version != IDENTITY_VERSION {
        return Err(problem(OutboxProblem::UnsupportedFormat));
    }
    if let RevisionAuthority::Recovery { store_id } = &identity.authority
        && (!crate::delivery::contract::valid_digest(store_id) || identity.id != digest(store_id.as_bytes())[..32])
    {
        return Err(problem(OutboxProblem::Integrity));
    }
    Ok(identity)
}
fn lock(path: &Path) -> Result<durable_fs::ExclusiveFile> {
    durable_fs::own_file(path).map_err(|error| match error {
        durable_fs::OwnershipError::InvalidFile => problem(OutboxProblem::Integrity),
        durable_fs::OwnershipError::Contended => problem(OutboxProblem::Locked),
        durable_fs::OwnershipError::Io(error) => storage(error),
    })
}
fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
fn validate_limits(limits: &OutboxLimits) -> Result<()> {
    if !limits.is_valid() {
        return Err(problem(OutboxProblem::InvalidLimits));
    }
    Ok(())
}
fn problem(kind: OutboxProblem) -> Error {
    let message = match kind {
        OutboxProblem::Locked => culprit_core::t!("outbox.locked"),
        OutboxProblem::Capacity => culprit_core::t!("outbox.capacity"),
        OutboxProblem::RecoveryRequired => culprit_core::t!("outbox.recovery_required"),
        OutboxProblem::SourceUnconfirmed => culprit_core::t!("outbox.source_unconfirmed"),
        OutboxProblem::Integrity => culprit_core::t!("outbox.integrity"),
        OutboxProblem::UnsupportedFormat => culprit_core::t!("outbox.unsupported_format"),
        OutboxProblem::MissingCapture => culprit_core::t!("outbox.missing_capture"),
        OutboxProblem::Storage => culprit_core::t!("outbox.storage"),
        OutboxProblem::InvalidLimits => culprit_core::t!("outbox.invalid_limits"),
    };
    Error::new(ErrorCode::QueryFailed, message).with_detail(culprit_core::ErrorDetail::Outbox(kind))
}
fn storage(error: impl std::fmt::Display) -> Error {
    problem(OutboxProblem::Storage).with_hint(culprit_core::t!("outbox.storage_detail", error = error.to_string()))
}
