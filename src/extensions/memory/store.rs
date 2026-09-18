use super::bm25::{Bm25Index, tokenize};
use super::chunk::{Chunk, DEFAULT_MAX_CHUNK_LINES, chunk_markdown};
use crate::markdown::{self, WikiLink, extract_wikilinks};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("io error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid workspace-relative path: {0}")]
    InvalidPath(String),
    #[error("json error on {path}: {source}")]
    Json {
        path: String,
        #[source]
        source: serde_json::Error,
    },
}

fn io_err(path: &Path) -> impl FnOnce(std::io::Error) -> MemoryError + '_ {
    move |source| MemoryError::Io {
        path: path.display().to_string(),
        source,
    }
}

#[derive(Debug, Clone, Default)]
pub struct FileMeta {
    pub name: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub score: f64,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct LinkNeighbor {
    pub path: String,
    pub name: Option<String>,
    pub description: Option<String>,
}

struct FileEntry {
    meta: FileMeta,
    chunks: Vec<Chunk>,
    outlinks: Vec<WikiLink>,
}

/// In-memory index over `daily/` and `digest/`. Files are the source of truth;
/// everything here is rebuilt on startup and refreshed on writes.
pub struct FileStore {
    workspace: PathBuf,
    max_chunk_lines: usize,
    files: BTreeMap<String, FileEntry>,
    inlinks: HashMap<String, Vec<String>>,
    bm25: Bm25Index,
    doc_index: Vec<(String, usize)>,
}

impl FileStore {
    pub fn open(workspace: impl AsRef<Path>) -> Result<Self, MemoryError> {
        Self::with_chunk_lines(workspace, DEFAULT_MAX_CHUNK_LINES)
    }

    pub fn with_chunk_lines(
        workspace: impl AsRef<Path>,
        max_chunk_lines: usize,
    ) -> Result<Self, MemoryError> {
        let workspace = workspace.as_ref().to_path_buf();
        super::ensure_layout(&workspace)?;
        let mut store = Self {
            workspace,
            max_chunk_lines,
            files: BTreeMap::new(),
            inlinks: HashMap::new(),
            bm25: Bm25Index::default(),
            doc_index: Vec::new(),
        };
        store.rebuild()?;
        Ok(store)
    }

    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    pub fn rebuild(&mut self) -> Result<(), MemoryError> {
        let mut files = BTreeMap::new();
        for sub in ["daily", "digest"] {
            let dir = checked_path(&self.workspace, sub)?;
            let mut paths = Vec::new();
            collect_markdown(&dir, &mut paths)?;
            paths.sort();
            for abs in paths {
                let rel = abs
                    .strip_prefix(&self.workspace)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .map_err(|_| MemoryError::InvalidPath(abs.display().to_string()))?;
                files.insert(rel.clone(), self.read_entry(&rel)?);
            }
        }
        self.files = files;
        self.rebuild_index();
        Ok(())
    }

    /// Re-read a file from disk and refresh its index entries.
    pub fn upsert_file(&mut self, rel: &str) -> Result<(), MemoryError> {
        validate_rel_path(rel)?;
        self.index_file(rel)
    }

    /// Write a file and immediately refresh its index entries.
    pub fn write_file(&mut self, rel: &str, content: &str) -> Result<(), MemoryError> {
        let abs = checked_path(&self.workspace, rel)?;
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        fs::write(&abs, content).map_err(io_err(&abs))?;
        self.index_file(rel)
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        let tokens = tokenize(query);
        self.bm25
            .search(&tokens, limit)
            .into_iter()
            .map(|(doc, score)| {
                let (path, idx) = &self.doc_index[doc];
                let chunk = &self.files[path].chunks[*idx];
                SearchHit {
                    path: path.clone(),
                    start_line: chunk.start_line,
                    end_line: chunk.end_line,
                    score,
                    text: chunk.text.clone(),
                }
            })
            .collect()
    }

    /// One-hop outlink targets and inlink sources, with name/description when indexed.
    pub fn expand_links(&self, rel: &str) -> Vec<LinkNeighbor> {
        let mut paths: Vec<String> = Vec::new();
        if let Some(entry) = self.files.get(rel) {
            paths.extend(entry.outlinks.iter().map(|l| l.target.clone()));
        }
        if let Some(sources) = self.inlinks.get(rel) {
            paths.extend(sources.iter().cloned());
        }
        paths.sort();
        paths.dedup();
        paths
            .into_iter()
            .map(|path| {
                let meta = self
                    .files
                    .get(&path)
                    .map(|e| e.meta.clone())
                    .unwrap_or_default();
                LinkNeighbor {
                    path,
                    name: meta.name,
                    description: meta.description,
                }
            })
            .collect()
    }

    pub fn file_meta(&self, rel: &str) -> Option<FileMeta> {
        self.files.get(rel).map(|e| e.meta.clone())
    }

    /// 1-based inclusive line range, clamped to the file.
    pub fn read_lines(&self, rel: &str, start: usize, end: usize) -> Result<String, MemoryError> {
        let abs = checked_path(&self.workspace, rel)?;
        let text = fs::read_to_string(&abs).map_err(io_err(&abs))?;
        let start = start.max(1);
        Ok(text
            .lines()
            .enumerate()
            .skip(start - 1)
            .take_while(|(index, _)| *index < end)
            .map(|(_, line)| line)
            .collect::<Vec<_>>()
            .join("\n"))
    }

    fn index_file(&mut self, rel: &str) -> Result<(), MemoryError> {
        let entry = self.read_entry(rel)?;
        self.files.insert(rel.to_string(), entry);
        self.rebuild_index();
        Ok(())
    }

    fn read_entry(&self, rel: &str) -> Result<FileEntry, MemoryError> {
        let abs = checked_path(&self.workspace, rel)?;
        let text = fs::read_to_string(&abs).map_err(io_err(&abs))?;
        let parsed =
            markdown::parse(&text).unwrap_or_else(|_| markdown::ParsedMarkdown::plain(&text));
        let (name, description) = parsed
            .frontmatter
            .map(|f| (f.name, f.description))
            .unwrap_or_default();
        let chunks = chunk_markdown(
            rel,
            &text,
            self.max_chunk_lines,
            name.as_deref(),
            description.as_deref(),
        );
        let outlinks = extract_wikilinks(&text);
        Ok(FileEntry {
            meta: FileMeta { name, description },
            chunks,
            outlinks,
        })
    }

    fn rebuild_index(&mut self) {
        self.inlinks.clear();
        self.doc_index.clear();
        let mut docs = Vec::new();
        for (path, entry) in &self.files {
            for link in &entry.outlinks {
                self.inlinks
                    .entry(link.target.clone())
                    .or_default()
                    .push(path.clone());
            }
            for (i, chunk) in entry.chunks.iter().enumerate() {
                self.doc_index.push((path.clone(), i));
                docs.push(tokenize(&chunk.search_text));
            }
        }
        for sources in self.inlinks.values_mut() {
            sources.sort();
            sources.dedup();
        }
        self.bm25 = Bm25Index::build(&docs);
    }
}

fn validate_rel_path(rel: &str) -> Result<(), MemoryError> {
    let path = Path::new(rel);
    if rel.is_empty() || path.is_absolute() {
        return Err(MemoryError::InvalidPath(rel.to_string()));
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_) | Component::CurDir) {
            return Err(MemoryError::InvalidPath(rel.to_string()));
        }
    }
    Ok(())
}

/// Resolve a workspace-relative path without following links inside the workspace.
/// Missing suffixes are allowed so callers can create new files and directories.
pub(super) fn checked_path(workspace: &Path, rel: &str) -> Result<PathBuf, MemoryError> {
    validate_rel_path(rel)?;
    let mut path = workspace.to_path_buf();
    for component in Path::new(rel).components() {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(MemoryError::InvalidPath(rel.into()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_err(&path)(error)),
        }
    }
    Ok(path)
}

pub(super) fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), MemoryError> {
    let metadata = match fs::symlink_metadata(dir) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_err(dir)(error)),
    };
    if !metadata.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).map_err(io_err(dir))? {
        let entry = entry.map_err(io_err(dir))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(io_err(&path))?;
        if kind.is_dir() {
            collect_markdown(&path, out)?;
        } else if kind.is_file() && path.extension().is_some_and(|ext| ext == "md") {
            out.push(path);
        }
    }
    Ok(())
}

/// dream checkpoint: for each processed file, the mtime (unix secs) it was processed at.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Catalog {
    #[serde(default)]
    pub checkpoints: HashMap<String, i64>,
}

impl Catalog {
    pub fn path(workspace: &Path) -> PathBuf {
        workspace.join("metadata").join("catalog.json")
    }

    pub fn load(workspace: &Path) -> Result<Self, MemoryError> {
        let path = checked_path(workspace, "metadata/catalog.json")?;
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| MemoryError::Json {
                path: path.display().to_string(),
                source: e,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(io_err(&path)(e)),
        }
    }

    pub fn save(&self, workspace: &Path) -> Result<(), MemoryError> {
        let path = checked_path(workspace, "metadata/catalog.json")?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| MemoryError::Json {
            path: path.display().to_string(),
            source: e,
        })?;
        fs::write(&path, text).map_err(io_err(&path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempWorkspace(PathBuf);

    impl TempWorkspace {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("kb-agent-test-{}", uuid::Uuid::new_v4()));
            Self(dir)
        }
    }

    impl Drop for TempWorkspace {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const RUST_CARD: &str = "---\nname: rust-notes\ndescription: Rust 学习笔记\n---\n\n# Rust\n\n讨论了 [[digest/wiki/borrow.md|借用检查]] 的所有权规则。\n";
    const BORROW_CARD: &str = "---\nname: borrow-checker\ndescription: 借用检查器要点\n---\n\n# Borrow\n\n借用检查保证引用有效。\n";

    fn open_seeded() -> (TempWorkspace, FileStore) {
        let ws = TempWorkspace::new();
        fs::create_dir_all(ws.0.join("daily/2026-09-17")).unwrap();
        fs::create_dir_all(ws.0.join("digest/wiki")).unwrap();
        fs::write(ws.0.join("daily/2026-09-17/rust.md"), RUST_CARD).unwrap();
        fs::write(ws.0.join("digest/wiki/borrow.md"), BORROW_CARD).unwrap();
        let store = FileStore::open(&ws.0).unwrap();
        (ws, store)
    }

    #[cfg(unix)]
    #[test]
    fn links_cannot_escape_reads_writes_or_indexing() {
        use std::os::unix::fs::symlink;
        let (ws, mut store) = open_seeded();
        let outside = TempWorkspace::new();
        fs::create_dir_all(&outside.0).unwrap();
        let target = outside.0.join("secret.md");
        fs::write(&target, "outsideonlymarker").unwrap();
        symlink(&outside.0, ws.0.join("digest/link")).unwrap();
        symlink(&target, ws.0.join("daily/linked.md")).unwrap();
        symlink(outside.0.join("missing.md"), ws.0.join("daily/dangling.md")).unwrap();
        symlink(ws.0.join("daily"), ws.0.join("daily/cycle")).unwrap();
        for path in [
            "digest/link/secret.md",
            "daily/linked.md",
            "daily/dangling.md",
        ] {
            assert!(store.read_lines(path, 1, 10).is_err(), "{path}");
            assert!(store.write_file(path, "overwrite").is_err(), "{path}");
            assert!(store.upsert_file(path).is_err(), "{path}");
        }
        assert!(store.write_file("digest/link/new/note.md", "new").is_err());
        assert!(!outside.0.join("new").exists());
        assert_eq!(fs::read_to_string(&target).unwrap(), "outsideonlymarker");
        store.rebuild().unwrap();
        assert!(store.search("outsideonlymarker", 5).is_empty());
        assert!(
            FileStore::open(&ws.0)
                .unwrap()
                .search("outsideonlymarker", 5)
                .is_empty()
        );
        let mut paths = Vec::new();
        collect_markdown(&ws.0.join("daily"), &mut paths).unwrap();
        assert_eq!(paths.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn layout_and_checkpoint_reject_linked_directories_and_files() {
        use std::os::unix::fs::symlink;
        let ws = TempWorkspace::new();
        let outside = TempWorkspace::new();
        fs::create_dir_all(&ws.0).unwrap();
        fs::create_dir_all(&outside.0).unwrap();
        symlink(&outside.0, ws.0.join("digest")).unwrap();
        assert!(FileStore::open(&ws.0).is_err());
        assert!(!outside.0.join("wiki").exists());
        fs::remove_file(ws.0.join("digest")).unwrap();
        FileStore::open(&ws.0).unwrap();
        let target = outside.0.join("checkpoint.json");
        fs::write(&target, "{}").unwrap();
        symlink(&target, Catalog::path(&ws.0)).unwrap();
        assert!(Catalog::load(&ws.0).is_err());
        assert!(Catalog::default().save(&ws.0).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "{}");
    }

    #[test]
    fn open_creates_layout_and_indexes_files() {
        let (ws, store) = open_seeded();
        for dir in [
            "session",
            "daily",
            "digest/personal",
            "digest/procedure",
            "digest/wiki",
            "metadata",
        ] {
            assert!(ws.0.join(dir).is_dir(), "missing {dir}");
        }
        let meta = store.file_meta("digest/wiki/borrow.md").unwrap();
        assert_eq!(meta.name.as_deref(), Some("borrow-checker"));
    }

    #[test]
    fn search_finds_chinese_and_english_terms() {
        let (_ws, store) = open_seeded();
        let hits = store.search("借用检查", 5);
        assert!(!hits.is_empty());
        assert_eq!(hits[0].path, "digest/wiki/borrow.md");
        let hits = store.search("rust", 5);
        assert!(hits.iter().any(|h| h.path == "daily/2026-09-17/rust.md"));
        assert!(hits.iter().all(|h| h.score > 0.0));
    }

    #[test]
    fn expand_links_returns_out_and_in_neighbors() {
        let (_ws, store) = open_seeded();
        let out = store.expand_links("daily/2026-09-17/rust.md");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "digest/wiki/borrow.md");
        assert_eq!(out[0].name.as_deref(), Some("borrow-checker"));
        assert_eq!(out[0].description.as_deref(), Some("借用检查器要点"));

        let back = store.expand_links("digest/wiki/borrow.md");
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].path, "daily/2026-09-17/rust.md");
        assert_eq!(back[0].name.as_deref(), Some("rust-notes"));
    }

    #[test]
    fn upsert_refreshes_index_immediately() {
        let (ws, store) = open_seeded();
        let mut store = store;
        assert!(store.search("生命周期", 5).is_empty());
        let updated = BORROW_CARD.replace("引用有效。", "引用有效，并涉及生命周期标注。");
        store.write_file("digest/wiki/borrow.md", &updated).unwrap();
        let hits = store.search("生命周期", 5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, "digest/wiki/borrow.md");
        assert!(ws.0.join("digest/wiki/borrow.md").is_file());
    }

    #[test]
    fn read_lines_returns_inclusive_clamped_range() {
        let (_ws, store) = open_seeded();
        let text = store.read_lines("digest/wiki/borrow.md", 6, 7).unwrap();
        assert_eq!(text, "# Borrow\n");
        let clamped = store.read_lines("digest/wiki/borrow.md", 7, 999).unwrap();
        assert!(clamped.contains("借用检查保证引用有效。"));
        let empty = store.read_lines("digest/wiki/borrow.md", 500, 600).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn rejects_paths_escaping_workspace() {
        let (_ws, store) = open_seeded();
        assert!(store.read_lines("../outside.md", 1, 10).is_err());
        assert!(store.read_lines("/etc/passwd", 1, 10).is_err());
    }

    #[test]
    fn catalog_roundtrips_and_defaults_when_missing() {
        let ws = TempWorkspace::new();
        fs::create_dir_all(&ws.0).unwrap();
        let missing = Catalog::load(&ws.0).unwrap();
        assert!(missing.checkpoints.is_empty());

        let mut catalog = Catalog::default();
        catalog
            .checkpoints
            .insert("daily/2026-09-17/rust.md".to_string(), 1_758_000_000);
        catalog.save(&ws.0).unwrap();
        assert!(Catalog::path(&ws.0).is_file());
        let loaded = Catalog::load(&ws.0).unwrap();
        assert_eq!(loaded, catalog);
    }

    #[test]
    fn reversed_and_extreme_line_ranges_are_empty() {
        let (_ws, store) = open_seeded();
        for (start, end) in [(7, 2), (1, 0), (usize::MAX, usize::MAX)] {
            assert!(
                store
                    .read_lines("digest/wiki/borrow.md", start, end)
                    .unwrap()
                    .is_empty()
            );
        }
        assert_eq!(
            store.read_lines("digest/wiki/borrow.md", 0, 1).unwrap(),
            "---"
        );
    }

    #[test]
    fn rebuild_clears_search_and_links_when_all_files_are_removed() {
        let (ws, mut store) = open_seeded();
        fs::remove_dir_all(ws.0.join("daily")).unwrap();
        fs::remove_dir_all(ws.0.join("digest")).unwrap();
        store.rebuild().unwrap();
        assert!(store.search("rust", 5).is_empty());
        assert!(store.expand_links("digest/wiki/borrow.md").is_empty());
    }

    #[test]
    fn failed_refresh_preserves_the_previous_index() {
        let (ws, mut store) = open_seeded();
        fs::write(ws.0.join("digest/wiki/borrow.md"), [0xff]).unwrap();
        assert!(store.upsert_file("digest/wiki/borrow.md").is_err());
        assert_eq!(store.search("借用检查", 5)[0].path, "digest/wiki/borrow.md");
        assert!(store.rebuild().is_err());
        assert_eq!(store.search("借用检查", 5)[0].path, "digest/wiki/borrow.md");
    }
}
