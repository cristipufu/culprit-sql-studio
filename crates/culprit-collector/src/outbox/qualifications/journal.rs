//! Issuance association and its independent artifact pin commit atomically in the existing private journal.
use super::*;
use crate::outbox::{Files, RevisionAuthority, investigations};
use rusqlite::{Connection, OptionalExtension, params};

const MAX_QUALIFICATION_BINDINGS: u32 = 256;
const MAX_BINDING_BYTES: usize = 128 * 1024;

impl Files {
    pub(in crate::outbox) fn install_qualification(
        &mut self,
        mut binding: Binding,
        captured: CompletedQualificationOriginal,
    ) -> Result<IssuedQualification> {
        if !matches!(self.authority, RevisionAuthority::Local) {
            return Err(DeliveryProblem::Fenced.error());
        }
        if binding.receipt.evidence.capture_ref() != *captured.capture() {
            return Err(integrity());
        }
        let index = captured
            .row_ordinals()
            .iter()
            .position(|ordinal| *ordinal == binding.receipt.evidence.row_ordinal)
            .ok_or_else(integrity)?;
        if captured.reference(index)? != binding.receipt.evidence {
            return Err(integrity());
        }
        let capture = self.journal.capture(&binding.receipt.evidence.capture.observation_id)?;
        if capture.connection != binding.connection
            || capture.state != OutboxCaptureState::Ready
            || capture.source_work != SourceWorkState::Quiescent
        {
            return Err(integrity());
        }
        let manifest_sha: Option<String> = self
            .journal
            .conn
            .query_row(
                "SELECT manifest_sha FROM captures WHERE observation_id=?1",
                [&capture.identity.observation_id],
                |row| row.get(0),
            )
            .map_err(storage)?;
        if capture.identity != captured.capture().capture
            || manifest_sha.as_deref() != Some(captured.capture().manifest_sha256.as_str())
        {
            return Err(integrity());
        }
        let max_bytes = self.journal.limits.max_bytes;
        let tx = self.journal.conn.transaction().map_err(storage)?;
        let previous: Option<String> = tx
            .query_row(
                "SELECT binding_json FROM qualifications WHERE connection=?1 AND recipe_id=?2",
                params![binding.connection, binding.receipt.recipe_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage)?;
        if previous.is_none() {
            let count: u32 =
                tx.query_row("SELECT count(*) FROM qualifications", [], |row| row.get(0)).map_err(storage)?;
            if count >= MAX_QUALIFICATION_BINDINGS {
                return Err(capacity());
            }
        }
        if let Some(previous) = previous {
            let previous = decode(&previous)?;
            binding.revision = previous.revision.checked_add(1).ok_or_else(capacity)?;
            self.qualification_leases.retire(&previous.id()?);
            tx.execute("DELETE FROM diagnostic_pins WHERE owner=?1", [owner(&previous.id()?)]).map_err(storage)?;
        }
        let id = binding.id()?;
        save_binding(&tx, &binding)?;
        investigations::journal::pin(&tx, &owner(&id), &binding.receipt.evidence.capture.observation_id)?;
        enforce_capacity(&tx, max_bytes)?;
        tx.commit().map_err(storage)?;
        self.qualification_leases.issue(binding)
    }

    pub(in crate::outbox) fn revoke_qualification(&mut self, connection: &str, recipe_id: &str) -> Result<bool> {
        let max_bytes = self.journal.limits.max_bytes;
        let tx = self.journal.conn.transaction().map_err(storage)?;
        let previous: Option<String> = tx
            .query_row(
                "SELECT binding_json FROM qualifications WHERE connection=?1 AND recipe_id=?2",
                params![connection, recipe_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(storage)?;
        let Some(previous) = previous else { return Ok(false) };
        let mut binding = decode(&previous)?;
        if !binding.active {
            return Ok(false);
        }
        let id = binding.id()?;
        self.qualification_leases.retire(&id);
        binding.active = false;
        binding.revision = binding.revision.checked_add(1).ok_or_else(capacity)?;
        save_binding(&tx, &binding)?;
        tx.execute("DELETE FROM diagnostic_pins WHERE owner=?1", [owner(&id)]).map_err(storage)?;
        enforce_capacity(&tx, max_bytes)?;
        tx.commit().map_err(storage)?;
        Ok(true)
    }

    pub(in crate::outbox) fn issued_qualifications(
        &mut self,
        control: &ReadControl,
        budget: CompletedCaptureBudget,
    ) -> Result<QualificationBindingsRead> {
        if !matches!(self.authority, RevisionAuthority::Local) {
            return Ok(QualificationBindingsRead { bindings: vec![], bytes_read: 0 });
        }
        let (json, bytes_read) = {
            let mut select = self
                .journal
                .conn
                .prepare("SELECT length(CAST(binding_json AS BLOB)),binding_json FROM qualifications ORDER BY connection,recipe_id LIMIT ?1")
                .map_err(storage)?;
            let mut rows = select.query([MAX_QUALIFICATION_BINDINGS + 1]).map_err(storage)?;
            let mut json = Vec::new();
            let mut bytes = 0u64;
            while let Some(row) = rows.next().map_err(storage)? {
                control.check()?;
                let signed_length: i64 = row.get(0).map_err(storage)?;
                let length = u64::try_from(signed_length).map_err(|_| integrity())?;
                if length > MAX_BINDING_BYTES as u64 || json.len() >= MAX_QUALIFICATION_BINDINGS as usize {
                    return Err(integrity());
                }
                bytes = bytes.checked_add(length).ok_or_else(capacity)?;
                if bytes > budget.max_bytes || json.len() >= budget.max_rows as usize {
                    return Err(DeliveryProblem::Capacity.error());
                }
                json.push(row.get::<_, String>(1).map_err(storage)?);
            }
            (json, bytes)
        };
        if json.len() > MAX_QUALIFICATION_BINDINGS as usize {
            return Err(integrity());
        }
        let mut installed = Vec::new();
        for json in json {
            control.check()?;
            let binding = decode(&json)?;
            if !binding.active {
                installed.push(self.qualification_leases.issue(binding)?);
                continue;
            }
            let id = binding.id()?;
            let pinned: bool = self
                .journal
                .conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM diagnostic_pins WHERE owner=?1 AND observation_id=?2)",
                    params![owner(&id), binding.receipt.evidence.capture.observation_id],
                    |row| row.get(0),
                )
                .map_err(storage)?;
            let capture = self.journal.capture(&binding.receipt.evidence.capture.observation_id)?;
            if !pinned
                || capture.connection != binding.connection
                || capture.state != OutboxCaptureState::Ready
                || capture.source_work != SourceWorkState::Quiescent
            {
                return Err(integrity());
            }
            installed.push(self.qualification_leases.issue(binding)?);
        }
        Ok(QualificationBindingsRead { bindings: installed, bytes_read })
    }
}

fn save_binding(conn: &Connection, binding: &Binding) -> Result<()> {
    let json = encode(binding)?;
    let charge = (json.len() as u64).checked_add(crate::outbox::CAPTURE_ACCOUNTING).ok_or_else(capacity)?;
    conn.execute(
        "INSERT INTO qualifications VALUES(?1,?2,?3,?4) ON CONFLICT(connection,recipe_id) DO UPDATE SET binding_json=excluded.binding_json,charged_bytes=excluded.charged_bytes",
        params![binding.connection,binding.receipt.recipe_id,json,crate::outbox::journal::bytes_to_sql(charge)?],
    ).map_err(storage)?;
    Ok(())
}
fn enforce_capacity(conn: &Connection, max_bytes: u64) -> Result<()> {
    let captures: i64 =
        conn.query_row("SELECT coalesce(sum(charged_bytes),0) FROM captures", [], |row| row.get(0)).map_err(storage)?;
    let total = crate::outbox::journal::bytes_from_sql(captures)?
        .checked_add(crate::outbox::journal::metadata_bytes(conn)?)
        .ok_or_else(capacity)?;
    if total > max_bytes {
        return Err(capacity());
    }
    Ok(())
}

pub(in crate::outbox) fn charged_bytes(conn: &Connection) -> Result<u64> {
    let bytes: i64 = conn
        .query_row("SELECT coalesce(sum(charged_bytes),0) FROM qualifications", [], |row| row.get(0))
        .map_err(storage)?;
    crate::outbox::journal::bytes_from_sql(bytes)
}
fn owner(id: &str) -> String {
    format!("qualification:{id}")
}
fn encode(binding: &Binding) -> Result<String> {
    let encoded = serde_json::to_string(binding).map_err(|_| integrity())?;
    if encoded.len() > MAX_BINDING_BYTES {
        return Err(capacity());
    }
    Ok(encoded)
}
fn decode(json: &str) -> Result<Binding> {
    if json.len() > MAX_BINDING_BYTES {
        return Err(integrity());
    }
    let binding: Binding = serde_json::from_str(json).map_err(|_| integrity())?;
    binding.id()?;
    Ok(binding)
}
fn capacity() -> culprit_core::Error {
    problem(OutboxProblem::Capacity)
}
