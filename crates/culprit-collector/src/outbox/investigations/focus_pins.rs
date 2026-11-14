//! Queued investigation parents share the existing capture pin owner; they never authorize native work.
use super::*;

const OWNER_PREFIX: &str = "investigation:";

fn owner(key: &str) -> Result<String> {
    if !valid_digest(key) {
        return Err(integrity());
    }
    Ok(format!("{OWNER_PREFIX}{key}"))
}

impl Files {
    pub(in crate::outbox) fn pin_investigation_parent(&mut self, key: &str, focus: &ActivityWaitFocus) -> Result<()> {
        let owner = owner(key)?;
        self.verify_focus_parent(focus)?;
        let transaction = self.journal.conn.transaction().map_err(storage)?;
        let parents = {
            let mut query = transaction
                .prepare("SELECT observation_id FROM diagnostic_pins WHERE owner=?1 LIMIT 2")
                .map_err(storage)?;
            query
                .query_map([&owner], |row| row.get::<_, String>(0))
                .map_err(storage)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(storage)?
        };
        match parents.as_slice() {
            [parent] if *parent == focus.parent.capture.observation_id => return Ok(()),
            [] => {}
            _ => return Err(integrity()),
        }
        let pins: u32 = transaction
            .query_row("SELECT count(*) FROM diagnostic_pins WHERE owner LIKE 'investigation:%'", [], |row| row.get(0))
            .map_err(storage)?;
        if pins >= MAX_DIAGNOSTIC_EPISODES as u32 {
            return Err(problem(OutboxProblem::Capacity));
        }
        transaction
            .execute("INSERT INTO diagnostic_pins VALUES(?1,?2)", params![owner, focus.parent.capture.observation_id])
            .map_err(storage)?;
        transaction.commit().map_err(storage)
    }

    pub(in crate::outbox) fn investigation_parent_keys(&self) -> Result<Vec<String>> {
        let mut query = self
            .journal
            .conn
            .prepare("SELECT owner FROM diagnostic_pins WHERE owner LIKE 'investigation:%' ORDER BY owner LIMIT ?1")
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
