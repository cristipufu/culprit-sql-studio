//! A compact prior read retains exact compilation counters, never SQL, plans or memory allocations.
use super::*;

pub const MAX_COMPILATION_BASELINE_BYTES: usize = 8 * 1024 * 1024;

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct CompilationBaseline {
    pub reference: Option<crate::ScanReference>,
    pub source: String,
    pub context: Option<NativeReadContext>,
    pub source_state: Option<crate::SourceState>,
    pub points: Vec<CompilationCounterPoint>,
}

#[derive(Serialize, Deserialize, TS, Clone, Debug, PartialEq)]
#[ts(export)]
pub struct CompilationCounterPoint {
    pub observation_key: String,
    pub identity: NativeQueryIdentity,
    pub compilations: String,
    pub last_compile_at: String,
    pub reset: crate::ResetMarker,
}

impl CompilationCounterPoint {
    pub fn from_observation(row: &OperationalObservation) -> Result<Self> {
        if !row.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        let OperationalFact::MssqlQueryCompilation(native) = &row.fact else {
            return Err(CollectionStop::InvalidPage.error());
        };
        Ok(Self {
            observation_key: row.key.clone(),
            identity: native.identity.clone(),
            compilations: native.compilations.clone(),
            last_compile_at: native.last_compile_at.clone(),
            reset: native.reset.clone(),
        })
    }

    pub fn is_valid(&self) -> bool {
        MssqlQueryCompilation::identity_key(&self.identity).is_ok_and(|key| key == self.observation_key)
            && valid_counter(&self.compilations)
            && canonical_time(&self.last_compile_at).is_ok()
            && valid_reset(&self.reset)
    }
}

impl CompilationBaseline {
    pub fn from_observations(
        source: &str,
        context: Option<NativeReadContext>,
        source_state: Option<crate::SourceState>,
        observations: &[&OperationalObservation],
        reference: Option<crate::ScanReference>,
    ) -> Result<Self> {
        if observations.len() > MAX_QUERY_COMPILATION_OBSERVATIONS
            || observations.iter().any(|row| row.source != source)
        {
            return Err(CollectionStop::InvalidPage.error());
        }
        let mut baseline = Self { reference, source: source.into(), context, source_state, points: Vec::new() };
        let mut retained = bounded_size(&baseline, MAX_COMPILATION_BASELINE_BYTES)?;
        for row in observations {
            let point = CompilationCounterPoint::from_observation(row)?;
            let comma = usize::from(!baseline.points.is_empty());
            let remaining = MAX_COMPILATION_BASELINE_BYTES.saturating_sub(retained).saturating_sub(comma);
            let bytes = bounded_size(&point, remaining)?;
            retained = retained
                .checked_add(comma)
                .and_then(|bytes_so_far| bytes_so_far.checked_add(bytes))
                .filter(|bytes| *bytes <= MAX_COMPILATION_BASELINE_BYTES)
                .ok_or_else(|| CollectionStop::ByteLimit.error())?;
            baseline.points.push(point);
        }
        if !baseline.is_valid() {
            return Err(CollectionStop::InvalidPage.error());
        }
        Ok(baseline)
    }

    pub fn from_snapshot(snapshot: &crate::Snapshot) -> Option<Self> {
        let captured = snapshot
            .operations
            .query_compilation
            .as_ref()
            .filter(|value| value.is_valid() && value.fits_database(&snapshot.database_identity()))?;
        let source = snapshot.sources.iter().find(|source| source.source == captured.source);
        let rows: Vec<_> = captured.observations.iter().collect();
        Self::from_observations(
            &captured.source,
            captured.context.clone(),
            source.map(|source| source.state),
            &rows,
            Some(crate::ScanReference::of(snapshot)),
        )
        .ok()
    }

    pub fn is_valid(&self) -> bool {
        let mut keys = BTreeSet::new();
        valid_key(&self.source)
            && self.context.as_ref().is_none_or(|context| {
                context.is_valid()
                    && context.target.database().is_some_and(|database| database.engine == crate::Engine::Mssql)
                    && context.partition == SourcePartition::Target
            })
            && self.reference.as_ref().is_none_or(|reference| canonical_time(&reference.taken_at).is_ok())
            && self
                .reference
                .as_ref()
                .and_then(|reference| reference.database.as_ref())
                .zip(self.context.as_ref())
                .is_none_or(|(database, context)| native_context_fits_database(context, database))
            && self.points.len() <= MAX_QUERY_COMPILATION_OBSERVATIONS
            && self.points.iter().all(|point| point.is_valid() && keys.insert(&point.observation_key))
            && match self.source_state {
                Some(
                    crate::SourceState::Empty | crate::SourceState::Unavailable | crate::SourceState::NotApplicable,
                ) => self.points.is_empty(),
                _ => true,
            }
            && crate::serialization::json_size(self, MAX_COMPILATION_BASELINE_BYTES).is_ok()
    }
}

fn bounded_size(value: &impl Serialize, limit: usize) -> Result<usize> {
    crate::serialization::json_size(value, limit).map_err(|error| match error {
        crate::serialization::JsonSizeError::LimitExceeded => CollectionStop::ByteLimit.error(),
        crate::serialization::JsonSizeError::Serialization => CollectionStop::InvalidPage.error(),
    })
}
