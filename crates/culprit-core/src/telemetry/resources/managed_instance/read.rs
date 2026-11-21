//! One selected original resource-history read; native history never becomes an Operational snapshot.
use super::*;

pub fn validate_managed_instance_resource_rows<'a>(
    source: &'a CollectionDescription,
    rows: &[ResourceMeasurement],
    read_complete: bool,
) -> Result<&'a MssqlManagedInstanceResourceHistory> {
    let SourceObservation::MssqlManagedInstanceResources(history) = &source.observation else { return Err(invalid()) };
    if !history.matches_source(source)
        || rows.len() > history.coverage.records as usize
        || read_complete && rows.len() != history.coverage.records as usize
    {
        return Err(invalid());
    }
    let mut previous = None;
    for row in rows {
        if !row.is_valid() || !history.accepts(row) || previous.is_some_and(|at: &str| at >= row.at.as_str()) {
            return Err(invalid());
        }
        previous = Some(row.at.as_str());
    }
    Ok(history)
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CapturedManagedInstanceResourceRead {
    pub origin: CapturedSnapshotOrigin,
    pub description: CollectionDescription,
    pub observations: Vec<ResourceMeasurement>,
    pub read_complete: bool,
}
impl CapturedManagedInstanceResourceRead {
    /// The caller authenticates the original manifest and positions. No replacement receipt is issued.
    pub fn captured(
        description: CollectionDescription,
        capture: DiagnosticCaptureRef,
        positions: Vec<CapturedSnapshotPosition>,
        observations: Vec<ResourceMeasurement>,
        read_complete: bool,
    ) -> Result<Self> {
        let origin = CapturedSnapshotOrigin::captured(&description, capture, positions)?;
        let read = Self { origin, description, observations, read_complete };
        read.is_valid().then_some(read).ok_or_else(invalid)
    }
    pub fn is_valid(&self) -> bool {
        validate_managed_instance_resource_rows(&self.description, &self.observations, self.read_complete).is_ok()
            && self
                .origin
                .validate(
                    &self.description,
                    self.observations.iter().cloned().map(TelemetryRecord::ResourceSample),
                    self.read_complete,
                )
                .is_ok()
    }
}

/// A single explicitly selected history original, or complete withdrawal of that original when sharing.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum RecordedManagedInstanceResourceRead {
    Captured { read: Box<CapturedManagedInstanceResourceRead> },
    Unavailable { reason: crate::i18n::Message },
}
fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}
