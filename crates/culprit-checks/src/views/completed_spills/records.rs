use super::*;

/// The storage owner authenticates these original rows. Pure validation also checks their canonical receipt binding.
pub struct CompletedSpillRecord<'a> {
    pub reference: CompletedSpillReference,
    record: NativeRow<'a>,
}

#[derive(Clone, Copy)]
enum NativeRow<'a> {
    Runtime(&'a OperationalObservation),
    Definition(&'a ObjectDefinitionRecord),
}

impl<'a> CompletedSpillRecord<'a> {
    pub fn captured(reference: DiagnosticEvidenceRef, record: &'a TelemetryRecord) -> Result<Self> {
        let record = match record {
            TelemetryRecord::Operational(row) => NativeRow::Runtime(row),
            TelemetryRecord::ObjectDefinition(row) => NativeRow::Definition(row),
            _ => return Err(invalid()),
        };
        Ok(Self { reference: CompletedSpillReference::Captured { reference: Box::new(reference) }, record })
    }

    pub fn captured_runtime(reference: DiagnosticEvidenceRef, row: &'a OperationalObservation) -> Self {
        Self {
            reference: CompletedSpillReference::Captured { reference: Box::new(reference) },
            record: NativeRow::Runtime(row),
        }
    }

    pub fn native_runtime(read_key: String, row: &'a OperationalObservation) -> Result<Self> {
        let record = NativeRow::Runtime(row);
        let observation_key = record.native_key()?;
        Ok(Self { reference: CompletedSpillReference::NativeRead { read_key, observation_key }, record })
    }

    pub(super) fn native_key(&self) -> Result<String> {
        self.record.native_key()
    }

    pub(super) fn source(&self) -> &str {
        match self.record {
            NativeRow::Runtime(row) => &row.source,
            NativeRow::Definition(row) => &row.source,
        }
    }
    pub(super) fn stream(&self) -> TelemetryStream {
        match self.record {
            NativeRow::Runtime(_) => TelemetryStream::OperationalSamples,
            NativeRow::Definition(_) => TelemetryStream::ObjectDefinitions,
        }
    }
    pub(super) fn is_valid(&self) -> bool {
        match self.record {
            NativeRow::Runtime(row) => row.is_valid(),
            NativeRow::Definition(row) => row.is_valid(),
        }
    }
    pub(super) fn operational(&self) -> Option<&OperationalObservation> {
        match self.record {
            NativeRow::Runtime(row) => Some(row),
            _ => None,
        }
    }
    pub(super) fn runtime(&self) -> Option<&CachedStatementRuntime> {
        match self.record {
            NativeRow::Runtime(OperationalObservation {
                fact: OperationalFact::MssqlCachedRuntime(sample), ..
            }) => Some(sample),
            _ => None,
        }
    }
    pub(super) fn definition(&self) -> Option<&ObjectDefinitionRecord> {
        match self.record {
            NativeRow::Definition(row) => Some(row),
            _ => None,
        }
    }
    pub(super) fn to_record(&self) -> TelemetryRecord {
        match self.record {
            NativeRow::Runtime(row) => TelemetryRecord::Operational(row.clone()),
            NativeRow::Definition(row) => TelemetryRecord::ObjectDefinition(row.clone()),
        }
    }
}

impl NativeRow<'_> {
    pub(super) fn native_key(&self) -> Result<String> {
        let identity = match self {
            Self::Runtime(row) => {
                NativeRecordIdentity::Operational { source: row.source.clone(), key: row.key.clone() }
            }
            Self::Definition(row) => {
                NativeRecordIdentity::ObjectDefinition { source: row.source.clone(), key: row.key.clone() }
            }
        };
        serde_json::to_vec(&identity).map(|bytes| digest(&bytes)).map_err(|_| invalid())
    }
}
