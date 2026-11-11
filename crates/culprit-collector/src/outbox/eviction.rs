//! A durable deletion intent is written only after an exact remote commit. Source debt is never discarded.
use super::*;
use crate::delivery::recovery::CommittedRange;
use rusqlite::params;

impl Files {
    pub(super) fn eviction_candidate(&self, id: &str) -> Result<Option<bool>> {
        let capture = match self.journal.capture(id) {
            Ok(capture) => capture,
            Err(error) if error.detail == Some(culprit_core::ErrorDetail::Outbox(OutboxProblem::MissingCapture)) => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        Ok(Some(
            capture.request.collection() != TelemetryCollection::Activity
                || self
                    .journal
                    .latest(&capture.connection, TelemetryCollection::Activity)?
                    .is_none_or(|latest| latest.identity != capture.identity),
        ))
    }
    pub(super) fn evict(&mut self, committed: &CommittedRange) -> Result<bool> {
        let RevisionAuthority::Recovery { store_id } = &self.authority else {
            return Err(DeliveryProblem::InvalidContract.error());
        };
        if store_id != committed.store_id() {
            return Err(DeliveryProblem::DestinationChanged.error());
        }
        let manifest = committed.manifest();
        let capture = match self.journal.capture(&manifest.capture.observation_id) {
            Ok(capture) => capture,
            Err(error) if error.detail == Some(culprit_core::ErrorDetail::Outbox(OutboxProblem::MissingCapture)) => {
                return Ok(true);
            }
            Err(error) => return Err(error),
        };
        if capture.identity != manifest.capture {
            return Err(problem(OutboxProblem::Integrity));
        }
        if capture.source_work != SourceWorkState::Quiescent {
            return Err(problem(OutboxProblem::SourceUnconfirmed));
        }
        if capture.state == OutboxCaptureState::Evicting {
            self.resume_eviction()?;
            return Ok(true);
        }
        if self.eviction_candidate(&capture.identity.observation_id)? == Some(false) {
            return Ok(false);
        }
        let expected = self.prepare_delivery(
            &capture.identity.observation_id,
            manifest.contract.clone(),
            manifest.advances.clone(),
        )?;
        if expected.manifest() != manifest || manifest.digest()? != committed.manifest_digest() {
            return Err(problem(OutboxProblem::Integrity));
        }
        let changed = self
            .journal
            .conn
            .execute(
                "UPDATE captures SET state=?1 WHERE observation_id=?2 AND state=?3",
                params![
                    journal::encode(&OutboxCaptureState::Evicting)?,
                    capture.identity.observation_id,
                    journal::encode(&OutboxCaptureState::Ready)?
                ],
            )
            .map_err(storage)?;
        if changed != 1 {
            return Err(problem(OutboxProblem::Integrity));
        }
        self.remove_capture(&capture.identity.observation_id)?;
        Ok(true)
    }
    pub(super) fn resume_eviction(&mut self) -> Result<()> {
        let ids = {
            let mut query = self
                .journal
                .conn
                .prepare("SELECT observation_id FROM captures WHERE state=?1 LIMIT 2")
                .map_err(storage)?;
            query
                .query_map([journal::encode(&OutboxCaptureState::Evicting)?], |row| row.get::<_, String>(0))
                .map_err(storage)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(storage)?
        };
        if ids.len() > 1 {
            return Err(problem(OutboxProblem::Integrity));
        }
        for id in ids {
            let capture = self.journal.capture(&id)?;
            if !matches!(self.authority, RevisionAuthority::Recovery { .. })
                || capture.source_work != SourceWorkState::Quiescent
            {
                return Err(problem(OutboxProblem::Integrity));
            }
            self.remove_capture(&id)?;
        }
        Ok(())
    }
    fn remove_capture(&mut self, id: &str) -> Result<()> {
        let directory = self.directory(id);
        if directory.try_exists().map_err(storage)? {
            // The same bounded inventory validation used for recovery rejects unexpected files and links.
            measure(&directory)?;
            for entry in fs::read_dir(&directory).map_err(storage)? {
                fs::remove_file(entry.map_err(storage)?.path()).map_err(storage)?;
            }
            fs::remove_dir(&directory).map_err(storage)?;
            durable_fs::sync_directory(&self.root.join("captures")).map_err(storage)?;
        }
        let tx = self.journal.conn.transaction().map_err(storage)?;
        tx.execute("DELETE FROM range_heads WHERE observation_id=?1", [id]).map_err(storage)?;
        tx.execute("DELETE FROM batches WHERE observation_id=?1", [id]).map_err(storage)?;
        let changed = tx
            .execute(
                "DELETE FROM captures WHERE observation_id=?1 AND state=?2",
                params![id, journal::encode(&OutboxCaptureState::Evicting)?],
            )
            .map_err(storage)?;
        if changed != 1 {
            return Err(problem(OutboxProblem::Integrity));
        }
        tx.commit().map_err(storage)
    }
}
