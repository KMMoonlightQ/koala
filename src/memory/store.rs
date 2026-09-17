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
        self.files.clear();
        for sub in ["daily", "digest"] {
            let dir = self.workspace.join(sub);
            let mut paths = Vec::new();
            collect_markdown(&dir, &mut paths)?;
            paths.sort();
            for abs in paths {
                let rel = abs
                    .strip_prefix(&self.workspace)
                    .map(|p| p.to_string_lossy().replace('\\', "/"))
                    .map_err(|_| MemoryError::InvalidPath(abs.display().to_string()))?;
                self.index_file(&rel)?;
            }
        }
        Ok(())
    }

    /// Re-read a file from disk and refresh its index entries.
    pub fn upsert_file(&mut self, rel: &str) -> Result<(), MemoryError> {
        validate_rel_path(rel)?;
        self.files.remove(rel);
        self.index_file(rel)
    }

    /// Write a file and immediately refresh its index entries.
    pub fn write_file(&mut self, rel: &str, content: &str) -> Result<(), MemoryError> {
        validate_rel_path(rel)?;
        let abs = self.workspace.join(rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        fs::write(&abs, content).map_err(io_err(&abs))?;
        self.upsert_file(rel)
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchHit> {
        let tokens = tokenize(query);
        self.bm25
            .search(&tokens, limit)
            .into_iter()
            .filter_map(|(doc, score)| {
                let (path, idx) = &self.doc_index[doc];
                let chunk = self.files.get(path)?.chunks.get(*idx)?;
                Some(SearchHit {
                    path: path.clone(),
                    start_line: chunk.start_line,
                    end_line: chunk.end_line,
                    score,
                    text: chunk.text.clone(),
                })
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
        validate_rel_path(rel)?;
        let abs = self.workspace.join(rel);
        let text = fs::read_to_string(&abs).map_err(io_err(&abs))?;
        let lines: Vec<&str> = text.lines().collect();
        if lines.is_empty() || start > lines.len() {
            return Ok(String::new());
        }
        let start = start.max(1);
        let end = end.min(lines.len());
        Ok(lines[start - 1..end].join("\n"))
    }

    fn index_file(&mut self, rel: &str) -> Result<(), MemoryError> {
        let abs = self.workspace.join(rel);
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
        self.files.insert(
            rel.to_string(),
            FileEntry {
                meta: FileMeta { name, description },
                chunks,
                outlinks,
            },
        );
        self.rebuild_index();
        Ok(())
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

fn collect_markdown(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), MemoryError> {
    if !dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(dir).map_err(io_err(dir))? {
        let path = entry.map_err(io_err(dir))?.path();
        if path.is_dir() {
            collect_markdown(&path, out)?;
        } else if path.extension().is_some_and(|ext| ext == "md") {
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
        let path = Self::path(workspace);
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
        let path = Self::path(workspace);
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
}
