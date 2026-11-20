//! Historical native values are distinct from the original acquisition's GUID/epoch and receipt proofs.
use culprit_core::{BackupHistoryReport, telemetry::MssqlBackupRecord};
use culprit_engine::LocalSnapshotAnalysis;
use serde_json::{Value, json};
pub(super) fn value(analysis: &LocalSnapshotAnalysis, report: &BackupHistoryReport) -> Value {
    let entries = report
        .entries
        .iter()
        .take(super::ENTRY_LIMIT)
        .map(|entry| {
            json!({
                "reference":super::reference(analysis,&entry.observation_key),"observed_at":entry.observed_at,
                "title":entry.title,"explanation":entry.explanation,"native_record":native_record(&entry.native),
                "investigation":entry.investigation,"verify":entry.verify,
            })
        })
        .collect();
    let mut value = super::summary(
        &report.title,
        &report.summary,
        Some(report.source_state),
        &None,
        &report.limitations,
        report.entries.len(),
        entries,
    );
    value["profile"] = json!(report.scope.profile);
    value["native_source"] = json!({"state":report.source.state,"records":report.source.records,
        "complete_scope":report.source.complete_scope,"error_code":report.source.error_code,"stopped":report.source.stopped});
    value["read_complete"] = json!(report.read_complete);
    value
}
fn native_record(record: &MssqlBackupRecord) -> Value {
    match record {
        MssqlBackupRecord::AzureSqlDatabase(backup) => json!({
            "backup_type":backup.backup_type,"started_without_offset":backup.started_without_offset,
            "finished_without_offset":backup.finished_without_offset,"in_retention":backup.in_retention,
            "physical_database_name":backup.physical_database_name,"logical_server_name":backup.logical_server_name,
            "logical_database_name":backup.logical_database_name,
        }),
        MssqlBackupRecord::Msdb(backup) => json!({
            "backup_set_id":backup.backup_set_id,"backup_type":backup.backup_type,
            "started_without_offset":backup.started_without_offset,"finished_without_offset":backup.finished_without_offset,
            "time_zone_quarter_hours":backup.time_zone_quarter_hours,"size_bytes":backup.size_bytes,"compressed_size_bytes":backup.compressed_size_bytes,
            "historical_initiator_user_name":backup.initiator_user_name,"historical_server_name":backup.server_name,"historical_machine_name":backup.machine_name,
            "software":backup.software,"lsns":backup.lsns,"flags":backup.flags,
        }),
    }
}
