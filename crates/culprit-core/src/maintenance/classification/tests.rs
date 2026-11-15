use super::*;
use crate::{maintenance::MaintenanceKind, telemetry::*};

fn current(sql: &str) -> ActivityObservation {
    ActivityObservation {
        key: ActivityKey {
            session: NativeActivityIdentity { id: "57".into(), generation: "native-login".into() },
            request: Some(NativeActivityIdentity { id: "0".into(), generation: "native-request-start".into() }),
            transaction: None,
        },
        scope: ActivityScopeBasis::RequestDatabase,
        state: "running".into(),
        idle: false,
        database: Some("db".into()),
        command: None,
        elapsed_ms: Some("1000".into()),
        last_request_age_ms: None,
        open_transactions: None,
        transaction_age_ms: None,
        session_counters: Default::default(),
        request_counters: Some(Default::default()),
        wait: None,
        query_hash: None,
        statement_identity: Some(NativeIdentity::new("native-current-statement")),
        client: None,
        statement: DefinitionPayload::sql(sql, Engine::Mssql),
        blocking_status: ActivityBlockingStatus::Known,
        waiting_on_lock: None,
    }
}

#[test]
fn a_guarded_current_statement_can_identify_a_rebuild_without_a_progress_or_lock_owner_claim() {
    let row = current("ALTER INDEX ix ON dbo.orders REBUILD");
    assert_eq!(
        classify_maintenance(Engine::Mssql, &row),
        MaintenanceClassification::Maintenance { kind: MaintenanceKind::IndexRebuild }
    );
    assert!(row.wait.is_none());
}

#[test]
fn native_command_labels_cannot_replace_missing_current_statement_or_change_observed_sql() {
    let mut row = current("SELECT id FROM dbo.orders");
    row.command = Some("ALTER INDEX REORGANIZE".into());
    assert_eq!(classify_maintenance(Engine::Mssql, &row), MaintenanceClassification::OtherStatement);
    row.statement = DefinitionPayload::Unavailable { reason: PayloadUnavailable::NotCollected };
    assert_eq!(
        classify_maintenance(Engine::Mssql, &row),
        MaintenanceClassification::Unresolved { reason: MaintenanceClassificationIssue::StatementUnavailable }
    );
}

#[test]
fn idle_previous_or_unidentified_statements_do_not_become_current_maintenance() {
    let mut row = current("ALTER INDEX ix ON dbo.orders REBUILD");
    row.statement_identity = None;
    assert_eq!(
        classify_maintenance(Engine::Mssql, &row),
        MaintenanceClassification::Unresolved { reason: MaintenanceClassificationIssue::StatementIdentityUnavailable }
    );
    row.key.request = None;
    row.request_counters = None;
    row.idle = true;
    assert!(row.is_valid());
    assert_eq!(
        classify_maintenance(Engine::Mssql, &row),
        MaintenanceClassification::Unresolved { reason: MaintenanceClassificationIssue::NoCurrentRequest }
    );
}

#[test]
fn invalid_or_oversized_native_payloads_are_unresolved_instead_of_known_non_maintenance() {
    let mut row = current("ALTER INDEX ix ON dbo.orders REBUILD (");
    assert_eq!(
        classify_maintenance(Engine::Mssql, &row),
        MaintenanceClassification::Unresolved { reason: MaintenanceClassificationIssue::InvalidStatement }
    );
    row = current(&format!("{}ALTER INDEX ix ON dbo.orders REBUILD", " ".repeat(MAX_MAINTENANCE_STATEMENT_BYTES)));
    assert!(row.statement.is_valid());
    assert_eq!(
        classify_maintenance(Engine::Mssql, &row),
        MaintenanceClassification::Unresolved { reason: MaintenanceClassificationIssue::StatementTooLarge }
    );
    row.statement = DefinitionPayload::sql("ALTER INDEX ix ON dbo.orders REBUILD", Engine::Mssql);
    let DefinitionPayload::Available { sha256, .. } = &mut row.statement else { unreachable!() };
    *sha256 = digest(b"foreign native body");
    assert_eq!(
        classify_maintenance(Engine::Mssql, &row),
        MaintenanceClassification::Unresolved { reason: MaintenanceClassificationIssue::InvalidObservation }
    );
}
