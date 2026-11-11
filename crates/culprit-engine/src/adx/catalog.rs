//! The complete analytical contract. Recovery-only incident checkpoints deliberately have no ADX table.
use super::{
    envelope::COMMON,
    prepared,
    schema::{self, Schema},
};
use culprit_core::{
    Result,
    telemetry::{DeliveryProblem, TelemetryStream, digest},
};

pub struct SchemaCatalog {
    schemas: Vec<(TelemetryStream, Schema)>,
    digest: String,
    publication: Schema,
}
impl SchemaCatalog {
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
    pub fn digest(&self) -> &str {
        &self.digest
    }
    pub fn publication(&self) -> &Schema {
        &self.publication
    }
    pub fn schemas(&self) -> impl Iterator<Item = (TelemetryStream, &Schema)> {
        self.schemas.iter().map(|(stream, schema)| (*stream, schema))
    }
    pub fn get(&self, stream: TelemetryStream) -> Result<&Schema> {
        self.schemas
            .iter()
            .find(|(known, _)| *known == stream)
            .map(|(_, schema)| schema)
            .ok_or_else(|| DeliveryProblem::UnsupportedSchema.error())
    }
}
