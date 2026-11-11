//! Native file identity shared by scans and background captures, retained across SQLite reads.
use crate::native;
use culprit_core::{DatabaseIdentity, Error, Result, telemetry::CollectionStop};
use rusqlite::Connection;
use std::{
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
};
fn changed() -> Error {
    CollectionStop::SourceChanged.error()
}
fn io_error(_: std::io::Error) -> Error {
    CollectionStop::SourceUnavailable.error()
}
pub(crate) struct FileIdentity {
    path: PathBuf,
    handle: same_file::Handle,
    pub(crate) database: DatabaseIdentity,
}
impl FileIdentity {
    pub(crate) fn read(path: &Path) -> Result<Self> {
        let path = path.canonicalize().map_err(io_error)?;
        let handle = same_file::Handle::from_path(&path).map_err(io_error)?;
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        handle.hash(&mut hash);
        let mut database = DatabaseIdentity::file(
            path.file_name().and_then(|s| s.to_str()).ok_or_else(native::invalid)?,
            path.to_string_lossy().into_owned(),
        );
        database.incarnation = Some(format!("file-id-v1-{:016x}", hash.finish()));
        Ok(Self { path, handle, database })
    }
    pub(crate) fn unchanged(&self, path: &Path) -> bool {
        path.canonicalize().ok().as_ref() == Some(&self.path)
            && same_file::Handle::from_path(path).is_ok_and(|handle| handle == self.handle)
    }
    pub(crate) fn bound_to(&self, conn: &Connection, path: &Path) -> Result<()> {
        #[cfg(unix)]
        {
            let mut moved = 1;
            conn.file_control(Some("main"), rusqlite::FileControl::HasMoved(&mut moved))
                .map_err(|error| crate::map_error(&error))?;
            if moved != 0 {
                return Err(changed());
            }
        }
        // On Windows SQLite's native open handle denies deletion/replacement until it closes. On Unix, HAS_MOVED
        // compares SQLite's actual open inode with the path, closing the open-vs-path replacement race.
        if !self.unchanged(path) {
            return Err(changed());
        }
        Ok(())
    }
}
