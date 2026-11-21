//! Saved Settings keep their original stream and use the common immutable snapshot receipt owner.
use super::*;

pub fn validate_mssql_pool_settings_read<'a>(
    source: &'a CollectionDescription,
    rows: &[&SettingObservation],
    read_complete: bool,
) -> Result<&'a MssqlPoolSettingsMembership> {
    let membership = mssql_pool_settings_membership(source)?.ok_or_else(invalid)?;
    if !source.partition_is_consistent()
        || !source.observation.covers_request()
        || rows.len() > MSSQL_POOL_SETTING_NAMES.len()
    {
        return Err(invalid());
    }
    let native = source.observation.snapshot().ok_or_else(invalid)?;
    let mut names = std::collections::BTreeSet::new();
    if rows.iter().any(|row| !row.fits_source(source) || !names.insert(row.name.as_str()))
        || rows.len() > native.records as usize
        || read_complete && rows.len() != native.records as usize
    {
        return Err(invalid());
    }
    Ok(membership)
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct CapturedPoolSettingsRead {
    pub origin: CapturedSnapshotOrigin,
    pub description: CollectionDescription,
    pub observations: Vec<SettingObservation>,
    pub read_complete: bool,
}
impl CapturedPoolSettingsRead {
    /// The caller authenticates these original files/positions. No receipt is issued for another body or scope.
    pub fn captured(
        description: CollectionDescription,
        capture: DiagnosticCaptureRef,
        positions: Vec<CapturedSnapshotPosition>,
        observations: Vec<SettingObservation>,
        read_complete: bool,
    ) -> Result<Self> {
        let origin = CapturedSnapshotOrigin::captured(&description, capture, positions)?;
        let read = Self { origin, description, observations, read_complete };
        read.is_valid().then_some(read).ok_or_else(invalid)
    }
    pub fn is_valid(&self) -> bool {
        validate_mssql_pool_settings_read(
            &self.description,
            &self.observations.iter().collect::<Vec<_>>(),
            self.read_complete,
        )
        .is_ok()
            && self.origin.validate(&self.description, self.records(), self.read_complete).is_ok()
    }
    pub fn captured_references(&self) -> Result<Vec<DiagnosticEvidenceRef>> {
        if !self.is_valid() {
            return Err(invalid());
        }
        self.origin.references(&self.description, self.records(), self.read_complete)
    }
    fn records(&self) -> impl Iterator<Item = TelemetryRecord> + '_ {
        self.observations.iter().cloned().map(TelemetryRecord::Setting)
    }
}

/// One explicitly selected original, or its withdrawal. The original remains independently retained in Outbox.
#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
#[ts(export)]
pub enum RecordedPoolSettingsRead {
    Captured { read: Box<CapturedPoolSettingsRead> },
    Unavailable { reason: crate::i18n::Message },
}

fn invalid() -> Error {
    CollectionStop::InvalidPage.error()
}
