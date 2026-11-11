//! Offline deployment output uses exactly the same schemas, mappings and projection contract as the sink.
use super::{catalog::SchemaCatalog, projection::ScalarType, schema::Schema};
use culprit_core::{Result, telemetry::TelemetryStream};
use serde::Serialize;

#[derive(Serialize)]
pub struct DeploymentAssets {
    pub schema_digest: String,
    pub commands: Vec<String>,
    pub routes: Vec<IngestionRoute>,
}
#[derive(Serialize)]
pub struct IngestionRoute {
    pub table: String,
    pub mapping: String,
    pub format: &'static str,
    pub blob_prefix: String,
    pub blob_suffix: &'static str,
}
impl DeploymentAssets {
    pub fn new() -> Result<Self> {
        let catalog = SchemaCatalog::new()?;
        let mut commands = Vec::new();
        let mut routes = Vec::new();
        for schema in catalog.schemas().map(|(_, schema)| schema).chain(std::iter::once(catalog.publication())) {
            commands.push(schema.create_table());
            commands.push(schema.create_mapping()?);
            routes.push(IngestionRoute {
                table: schema.table().into(),
                mapping: schema.mapping_name(),
                format: "json",
                blob_prefix: format!("ready/{}/", schema.table()),
                blob_suffix: ".ndjson",
            });
        }
        commands.push(published(&catalog));
        commands.push(heads("CulpritActivityHeadsV1","tostring(SourceDescription.observation.kind) == \"activity\" and tobool(SourceDescription.observation.data.database_wide)",false));
        commands.push(heads(
            "CulpritSnapshotHeadsV1",
            "tostring(SourceDescription.observation.kind) == \"snapshot\"",
            true,
        ));
        for (stream, schema) in catalog.schemas() {
            commands.push(eligible(schema));
            let historical = matches!(
                stream,
                TelemetryStream::OperationalSamples
                    | TelemetryStream::WaitCounterSamples
                    | TelemetryStream::ReplicationSamples
                    | TelemetryStream::SettingObservations
                    | TelemetryStream::TuningObservations
                    | TelemetryStream::ObjectDefinitions
                    | TelemetryStream::ActivityObservations
                    | TelemetryStream::BlockingEdges
            );
            commands.push(if historical {
                format!(".create function {}Rows(contract:string,store:string,database:string,captured_from:datetime,asof:datetime) {{ {}Eligible(contract,store,database,captured_from,asof) }}",schema.table(),schema.table())
            } else {revisions(schema)});
            let current_heads = match stream {
                TelemetryStream::ActivityObservations | TelemetryStream::BlockingEdges => {
                    Some("CulpritActivityHeadsV1")
                }
                TelemetryStream::OperationalSamples
                | TelemetryStream::WaitCounterSamples
                | TelemetryStream::ReplicationSamples
                | TelemetryStream::SettingObservations
                | TelemetryStream::TuningObservations
                | TelemetryStream::ObjectDefinitions => Some("CulpritSnapshotHeadsV1"),
                _ => None,
            };
            if let Some(heads) = current_heads {
                let keys = if matches!(stream, TelemetryStream::ActivityObservations | TelemetryStream::BlockingEdges) {
                    "RangeId"
                } else {
                    "RangeId, NativeSource"
                };
                commands.push(format!(".create function {}Current(contract:string,store:string,database:string,asof:datetime,max_age:timespan) {{\nlet Heads={} (contract,store,database,asof-max_age,asof) | where max_age > 0s and CollectedUntil >= asof-max_age | project {keys};\n{}Rows(contract,store,database,asof-max_age,asof) | join kind=leftsemi Heads on {keys}\n}}",schema.table(),heads,schema.table()));
            }
        }
        Ok(Self { schema_digest: catalog.digest().into(), commands, routes })
    }
    pub fn script(&self) -> String {
        format!(".execute database script with (ContinueOnErrors=false) <|\n{}\n", self.commands.join("\n\n"))
    }
}
fn columns(schema: &Schema) -> String {
    schema.columns().iter().map(|column| column.name.as_str()).collect::<Vec<_>>().join(", ")
}
fn restore(schema: &Schema) -> String {
    let conversions = schema
        .columns()
        .iter()
        .filter_map(|column| match column.kind {
            ScalarType::JsonContainer => Some(format!("{}=todynamic({})", column.name, column.name)),
            ScalarType::ExactCounter => Some(format!("{}=todecimal({})", column.name, column.name)),
            _ => None,
        })
        .collect::<Vec<_>>();
    if conversions.is_empty() { String::new() } else { format!("| extend {}", conversions.join(", ")) }
}
fn published(catalog: &SchemaCatalog) -> String {
    let schema = catalog.publication();
    include_str!("kql/published.kql")
        .replace("$PROJECTION", &schema.verification_projection())
        .replace("$COLUMNS", &columns(schema))
        .replace("$RESTORE", &restore(schema))
        .replace("$SCHEMA", catalog.digest())
}
fn eligible(schema: &Schema) -> String {
    include_str!("kql/eligible.kql")
        .replace("$TABLE", schema.table())
        .replace("$PROJECTION", &schema.verification_projection())
        .replace("$COLUMNS", &columns(schema))
        .replace("$RESTORE", &restore(schema))
}
fn revisions(schema: &Schema) -> String {
    include_str!("kql/revisions.kql").replace("$TABLE", schema.table()).replace("$COLUMNS", &columns(schema))
}
fn heads(name: &str, selection: &str, check_sources: bool) -> String {
    let complete = if check_sources {
        "| mv-expand Coverage=SourceDescription.observation.data.sources\n| where tobool(Coverage.complete_scope) and tostring(Coverage.state) in (\"collected\", \"empty\")\n| extend NativeSource=tostring(Coverage.source)"
    } else {
        ""
    };
    include_str!("kql/heads.kql")
        .replace("$FUNCTION", name)
        .replace("$SELECTION", selection)
        .replace("$COMPLETE", complete)
        .replace(
            "$GROUP_KEYS",
            if check_sources { "LogicalSourceId, Dataset, NativeSource" } else { "LogicalSourceId, Dataset" },
        )
}

#[cfg(test)]
mod tests;
