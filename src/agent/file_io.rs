use std::io::Write as _;
use std::path::{Path, PathBuf};

/// A sibling temporary file avoids partially truncating the destination on failure.
pub(super) fn atomic_write(path: &Path, content: &[u8]) -> std::io::Result<()> {
    struct Temporary(PathBuf);
    impl Drop for Temporary {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let parent = path
        .parent()
        .ok_or_else(|| std::io::Error::other("file has no parent directory"))?;
    let temporary = Temporary(parent.join(format!(".koala-write-{}", uuid::Uuid::new_v4())));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary.0)?;
    if let Ok(metadata) = std::fs::metadata(path) {
        file.set_permissions(metadata.permissions())?;
    }
    file.write_all(content)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&temporary.0, path)
}
