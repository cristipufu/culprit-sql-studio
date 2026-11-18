//! Queued investigation parents share the existing capture pin owner; they never authorize native work.
use super::*;

const OWNER_PREFIX: &str = "investigation:";

fn owner(key: &str) -> Result<String> {
    if !valid_digest(key) {
        return Err(integrity());
    }
    Ok(format!("{OWNER_PREFIX}{key}"))
}

/// Keep the one completed control input until Fleet durably acknowledges this exact intent. The existing
/// investigation owner releases both prerequisites and the result, including after a lost completion ack.
pub(super) fn pin_source_read_result(
    tx: &Transaction<'_>,
    intent: &DiagnosticInvestigationIntent,
    result: &CaptureIdentity,
) -> Result<()> {
    if !intent.is_valid() || !matches!(intent.focus, DiagnosticFocus::SourceRead { .. }) {
        return Err(integrity());
    }
    tx.execute(
        "INSERT OR IGNORE INTO diagnostic_pins VALUES(?1,?2)",
        params![owner(&intent.key()?)?, result.observation_id],
    )
    .map_err(storage)?;
    Ok(())
}

impl Files {
    pub(in crate::outbox) fn pin_investigation_parent(&mut self, intent: &DiagnosticInvestigationIntent) -> Result<()> {
        let owner = owner(&intent.key()?)?;
        self.verify_investigation_intent(intent)?;
        let mut expected = intent
            .focus
            .references()
            .map(|reference| reference.capture.observation_id.clone())
            .collect::<BTreeSet<_>>();
        if let DiagnosticExecutionBinding::Shared { owner } = &intent.execution {
            expected.insert(owner.capture.observation_id.clone());
        }
        // Revalidation still needs the original native-cost artifact until this exact queued owner retires.
        expected.insert(intent.qualification.evidence.capture.observation_id.clone());
        let query_limit =
            expected.len().checked_add(1).and_then(|limit| i64::try_from(limit).ok()).ok_or_else(integrity)?;
        let transaction = self.journal.conn.transaction().map_err(storage)?;
        let parents = {
            let mut query = transaction
                .prepare("SELECT observation_id FROM diagnostic_pins WHERE owner=?1 LIMIT ?2")
                .map_err(storage)?;
            query
                .query_map(params![owner, query_limit], |row| row.get::<_, String>(0))
                .map_err(storage)?
                .collect::<rusqlite::Result<BTreeSet<_>>>()
                .map_err(storage)?
        };
        if !parents.is_empty() {
            if parents == expected {
                return Ok(());
            }
            return Err(integrity());
        }
        let pins: u32 = transaction
            .query_row(
                "SELECT count(DISTINCT owner) FROM diagnostic_pins WHERE owner LIKE 'investigation:%'",
                [],
                |row| row.get(0),
            )
            .map_err(storage)?;
        if pins >= MAX_DIAGNOSTIC_EPISODES as u32 {
            return Err(problem(OutboxProblem::Capacity));
        }
        for parent in expected {
            transaction
                .execute("INSERT INTO diagnostic_pins VALUES(?1,?2)", params![owner, parent])
                .map_err(storage)?;
        }
        transaction.commit().map_err(storage)
    }

    pub(in crate::outbox) fn investigation_parent_keys(&self) -> Result<Vec<String>> {
        let mut query = self
            .journal
            .conn
            .prepare(
                "SELECT DISTINCT owner FROM diagnostic_pins WHERE owner LIKE 'investigation:%' ORDER BY owner LIMIT ?1",
            )
            .map_err(storage)?;
        let owners = query
            .query_map([(MAX_DIAGNOSTIC_EPISODES + 1) as u32], |row| row.get::<_, String>(0))
            .map_err(storage)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(storage)?;
        if owners.len() > MAX_DIAGNOSTIC_EPISODES {
            return Err(problem(OutboxProblem::Capacity));
        }
        let mut keys = BTreeSet::new();
        for owner in owners {
            let key = owner.strip_prefix(OWNER_PREFIX).filter(|key| valid_digest(key)).ok_or_else(integrity)?;
            if !keys.insert(key.to_owned()) {
                return Err(integrity());
            }
        }
        Ok(keys.into_iter().collect())
    }

    pub(in crate::outbox) fn release_investigation_parent(&mut self, key: &str) -> Result<()> {
        self.journal.conn.execute("DELETE FROM diagnostic_pins WHERE owner=?1", [owner(key)?]).map_err(storage)?;
        Ok(())
    }
}
