//! Exclusive private artifacts; data reaches disk before a directory entry is published.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::Path,
};

pub(crate) fn create_directory(path: &Path) -> io::Result<()> {
    let builder = fs::DirBuilder::new();
    #[cfg(unix)]
    let builder = {
        use std::os::unix::fs::DirBuilderExt;
        let mut builder = builder;
        builder.mode(0o700);
        builder
    };
    builder.create(path)?;
    let parent = path.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or_else(|| Path::new("."));
    sync_directory(parent)?;
    sync_directory(path)?;
    Ok(())
}
pub(crate) fn create_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}
pub(crate) fn regular_file(path: &Path) -> io::Result<File> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        return Err(io::ErrorKind::InvalidData.into());
    }
    File::open(path)
}
pub(crate) fn sync_directory(path: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        File::open(path)?.sync_all()?;
        Ok(true)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(false)
    }
}
pub(crate) fn publish(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let name = path.file_name().ok_or(io::ErrorKind::InvalidInput)?.to_string_lossy();
    let temporary = path.with_file_name(format!(".{name}.new"));
    let mut file = create_file(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(temporary, path)?;
    sync_directory(path.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
    Ok(())
}
pub(crate) fn random_id() -> culprit_core::Result<String> {
    let mut random = [0u8; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random)
        .map_err(|_| culprit_core::Error::internal(culprit_core::t!("collection.random_failed")))?;
    Ok(random.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub(crate) struct LimitedBuffer {
    bytes: Vec<u8>,
    limit: usize,
    exceeded: bool,
}
impl LimitedBuffer {
    pub(crate) fn new(limit: usize) -> Self {
        Self { bytes: vec![], limit, exceeded: false }
    }
    pub(crate) fn exceeded(&self) -> bool {
        self.exceeded
    }
    pub(crate) fn len(&self) -> usize {
        self.bytes.len()
    }
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}
impl Write for LimitedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::ErrorKind::WriteZero.into());
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(crate) fn json_bytes(value: &impl serde::Serialize, limit: usize) -> io::Result<Vec<u8>> {
    let mut buffer = LimitedBuffer::new(limit);
    serde_json::to_writer_pretty(&mut buffer, value).map_err(|_| io::ErrorKind::InvalidData)?;
    buffer.write_all(b"\n")?;
    Ok(buffer.into_bytes())
}

pub(crate) enum OwnershipError {
    InvalidFile,
    Contended,
    Io(io::Error),
}
pub(crate) struct ExclusiveFile {
    file: File,
}
impl Drop for ExclusiveFile {
    fn drop(&mut self) {
        // A pre-exec descriptor copy can survive this handle. Unlock explicitly; if it fails, OS cleanup on final
        // closure remains the fallback and a replacement owner must still obtain the lock before doing any work.
        let _ = self.file.unlock();
    }
}
/// One ownership primitive, with enough structure for each journal to report its own policy failure.
pub(crate) fn own_file(path: &Path) -> Result<ExclusiveFile, OwnershipError> {
    let metadata = fs::symlink_metadata(path).map_err(OwnershipError::Io)?;
    if !metadata.file_type().is_file() {
        return Err(OwnershipError::InvalidFile);
    }
    let file = OpenOptions::new().read(true).write(true).open(path).map_err(OwnershipError::Io)?;
    match file.try_lock() {
        Ok(()) => Ok(ExclusiveFile { file }),
        Err(fs::TryLockError::WouldBlock) => Err(OwnershipError::Contended),
        Err(fs::TryLockError::Error(error)) => Err(OwnershipError::Io(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ending_ownership_releases_the_lock_even_when_a_descriptor_was_duplicated() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("owner.lock");
        create_file(&path).unwrap();
        let owner = own_file(&path).ok().expect("owner");
        let inherited = owner.file.try_clone().unwrap();
        drop(owner);
        let replacement = own_file(&path);
        assert!(replacement.is_ok(), "a duplicated pre-exec descriptor must not extend ownership");
        drop(inherited);
        assert!(matches!(own_file(&path), Err(OwnershipError::Contended)));
        drop(replacement);
        assert!(own_file(&path).is_ok());
    }
}
