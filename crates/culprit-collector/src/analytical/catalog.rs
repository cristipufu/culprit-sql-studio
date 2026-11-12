//! The complete analytical contract. Recovery-only incident checkpoints have no analytical table.
use super::{
    envelope::COMMON,
    prepared,
    schema::{self, LogicalSchema},
};
use culprit_core::{
    Result,
    telemetry::{DeliveryProblem, TelemetryStream, digest},
};

pub struct LogicalCatalog {
    schemas: Vec<(TelemetryStream, LogicalSchema)>,
    digest: String,
    publication: LogicalSchema,
}
impl LogicalCatalog {
    pub fn new() -> Result<Self> {
        use TelemetryStream::*;
        let mut schemas = Vec::new();
        for stream in TelemetryStream::ALL {
            let schema = match stream {
                QueryRuntime => schema::queries::runtime(COMMON)?,
                QueryWaits => schema::queries::waits(COMMON)?,
                QueryDefinitions => schema::definitions::queries(COMMON)?,
                PlanDefinitions => schema::definitions::plans(COMMON)?,
                ResourceSamples => schema::samples::resources(COMMON)?,
                ActivityObservations => schema::activity::observations(COMMON)?,
                BlockingEdges => schema::activity::blocking(COMMON)?,
                ActivityIncidents => schema::activity::incidents(COMMON)?,
                DeadlockEvents => schema::facts::deadlocks(COMMON)?,
                OperationalSamples => schema::facts::operational(COMMON)?,
                WaitCounterSamples => schema::samples::wait_counters(COMMON)?,
                ReplicationSamples => schema::facts::replication(COMMON)?,
                SettingObservations => schema::samples::settings(COMMON)?,
                TuningObservations => schema::facts::tuning(COMMON)?,
                ObjectDefinitions => schema::facts::objects(COMMON)?,
                ActivityCheckpoints => continue,
            };
            schemas.push((stream, schema));
        }
        let identities = schemas.iter().map(|(stream, schema)| (stream, schema.digest())).collect::<Vec<_>>();
        let publication = schema::control::published_ranges()?;
        let bytes = serde_json::to_vec(&(prepared::PREPARATION_VERSION, identities, publication.digest()))
            .map_err(|_| DeliveryProblem::Integrity.error())?;
        Ok(Self { schemas, digest: digest(&bytes), publication })
    }
    pub fn into_parts(self) -> (Vec<(TelemetryStream, LogicalSchema)>, LogicalSchema) {
        (self.schemas, self.publication)
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn publication(&self) -> &LogicalSchema {
        &self.publication
    }
    pub fn schemas(&self) -> impl Iterator<Item = (TelemetryStream, &LogicalSchema)> {
        self.schemas.iter().map(|(stream, schema)| (*stream, schema))
    }
    pub fn get(&self, stream: TelemetryStream) -> Result<&LogicalSchema> {
        self.schemas
            .iter()
            .find(|(known, _)| *known == stream)
            .map(|(_, schema)| schema)
            .ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())
    }
}

/// A physical schema factory is the only destination choice. Stream coverage and contract fingerprinting are shared.
pub struct PhysicalCatalog<S> {
    schemas: Vec<(TelemetryStream, S)>,
    publication: S,
    digest: String,
}
impl<S: super::RowProjection> PhysicalCatalog<S> {
    pub fn new() -> Result<Self> {
        let (logical, publication) = LogicalCatalog::new()?.into_parts();
        let schemas = logical
            .into_iter()
            .map(|(stream, schema)| Ok((stream, S::from_logical(schema)?)))
            .collect::<Result<Vec<_>>>()?;
        let publication = S::from_logical(publication)?;
        let identities = schemas.iter().map(|(stream, schema)| (stream, schema.digest())).collect::<Vec<_>>();
        let bytes =
            serde_json::to_vec(&(identities, publication.digest())).map_err(|_| DeliveryProblem::Integrity.error())?;
        Ok(Self { schemas, publication, digest: digest(&bytes) })
    }
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn schemas(&self) -> impl Iterator<Item = (TelemetryStream, &S)> {
        self.schemas.iter().map(|(stream, schema)| (*stream, schema))
    }
    pub fn publication(&self) -> &S {
        &self.publication
    }
    pub fn get(&self, stream: TelemetryStream) -> Result<&S> {
        self.schemas
            .iter()
            .find(|(known, _)| *known == stream)
            .map(|(_, schema)| schema)
            .ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())
    }
}
impl<S: super::RowProjection> super::ProjectionCatalog for PhysicalCatalog<S> {
    fn digest(&self) -> &str {
        self.digest()
    }
    fn schema(&self, stream: TelemetryStream) -> Result<&dyn super::RowProjection> {
        Ok(self.get(stream)?)
    }
    fn streams(&self) -> Vec<(TelemetryStream, &dyn super::RowProjection)> {
        self.schemas().map(|(stream, schema)| (stream, schema as &dyn super::RowProjection)).collect()
    }
    fn publication(&self) -> &dyn super::RowProjection {
        self.publication()
    }
}
