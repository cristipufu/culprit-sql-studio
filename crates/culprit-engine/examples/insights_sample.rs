//! Regenerate the browser's plan insights. An optional output path allows verification without replacing fixtures.
use std::{collections::BTreeMap, path::PathBuf};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    culprit_core::i18n::scope("en", generate)
}

fn generate() -> Result<(), Box<dyn std::error::Error>> {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    let output = std::env::args_os().nth(1).map(PathBuf::from).unwrap_or_else(|| fixtures.join("insights.sample.json"));
    let mut paths =
        std::fs::read_dir(&fixtures)?.map(|entry| entry.map(|entry| entry.path())).collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| path.file_name().is_some_and(|name| name.to_string_lossy().ends_with(".report.json")));
    paths.sort();
    let mut insights = BTreeMap::new();
    for path in paths {
        let report = culprit_engine::reading::open(&path)?;
        let mut queries = BTreeMap::new();
        for query in report.snapshot.workload.iter().flat_map(|workload| &workload.queries) {
            if let Some(plan) = &query.plan {
                let set = culprit_checks::query_plan_insights(&report.snapshot, query, plan);
                queries.insert(query.id.clone(), serde_json::to_value(set)?);
            }
        }
        insights.insert(report.snapshot.connection.clone(), queries);
    }
    let text = serde_json::to_string_pretty(&insights)?;
    std::fs::write(output, text + "\n")?;
    Ok(())
}
