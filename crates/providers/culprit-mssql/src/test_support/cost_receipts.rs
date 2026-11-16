//! Private experimental receipts do not constitute source cost approval.
use std::path::{Path, PathBuf};

const MAX_RECEIPT_BYTES: usize = 4 * 1024 * 1024;

pub(crate) fn receipt_directory() -> PathBuf {
    let path =
        PathBuf::from(std::env::var("CULPRIT_NATIVE_COST_RECEIPT_DIR").expect("explicit private receipt directory"));
    assert!(path.is_absolute() && path.is_dir() && !path.symlink_metadata().unwrap().file_type().is_symlink());
    path
}

pub(crate) fn save_cost_receipt(directory: &Path, name: &str, value: &serde_json::Value) -> culprit_core::Result<()> {
    use std::io::Write;
    let path = directory.join(format!("{name}.json"));
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut output = options.open(path).map_err(|_| crate::read::invalid_data())?;
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| crate::read::invalid_data())?;
    if bytes.len() > MAX_RECEIPT_BYTES {
        return Err(crate::read::invalid_data());
    }
    output.write_all(&bytes).and_then(|_| output.sync_all()).map_err(|_| crate::read::invalid_data())
}
