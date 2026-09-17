pub mod bm25;
pub mod chunk;
pub mod distill;
pub mod dream;
pub mod store;
pub mod tools;

pub use distill::{DistillError, distill_session};
pub use dream::{DreamError, DreamReport, dream};

pub use store::{Catalog, FileStore, LinkNeighbor, MemoryError, SearchHit};

pub const BUCKET_NAMES: [&str; 3] = ["personal", "procedure", "wiki"];

pub(crate) fn ensure_layout(workspace: &std::path::Path) -> Result<(), MemoryError> {
    for dir in [
        "session",
        "daily",
        "digest/personal",
        "digest/procedure",
        "digest/wiki",
        "metadata",
    ] {
        let path = workspace.join(dir);
        std::fs::create_dir_all(&path).map_err(|source| MemoryError::Io {
            path: path.display().to_string(),
            source,
        })?;
    }
    Ok(())
}
