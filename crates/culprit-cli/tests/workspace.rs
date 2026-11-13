use std::path::PathBuf;

pub(super) struct Home(pub PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(super) fn home(tag: &str) -> Home {
    let directory = std::env::temp_dir().join(format!("culprit-cli-said-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    Home(directory)
}
