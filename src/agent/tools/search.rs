//! Bounded, shell-free file discovery and text search.
use super::{Tool, ToolContext, ToolResult};
use crate::i18n::Lang;
use glob::{MatchOptions, Pattern};
use regex::RegexBuilder;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{future::Future, io::Read, num::NonZeroUsize, pin::Pin};
use walkdir::WalkDir;

const MAX_FILE_BYTES: usize = 2 * 1024 * 1024;
const MAX_SCAN_BYTES: usize = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;

pub struct Glob;
pub struct Grep;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GlobArgs {
    pattern: String,
    path: Option<String>,
    limit: Option<NonZeroUsize>,
    #[serde(default)]
    hidden: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrepArgs {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    limit: Option<NonZeroUsize>,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    literal: bool,
    #[serde(default)]
    ignore_case: bool,
}

#[derive(Default, Serialize)]
struct SearchResult {
    results: Vec<Value>,
    truncated: bool,
    skipped_files: usize,
    warnings: Vec<String>,
    #[serde(skip)]
    bytes: usize,
}

impl SearchResult {
    fn push(&mut self, value: Value, limit: usize) -> bool {
        let bytes = value.to_string().len() + 1;
        if self.results.len() == limit || self.bytes + bytes > MAX_OUTPUT_BYTES {
            self.truncated = true;
            return false;
        }
        self.bytes += bytes;
        self.results.push(value);
        true
    }

    fn warn(&mut self, message: String) {
        self.skipped_files += 1;
        if self.warnings.len() < 5 {
            self.warnings.push(message);
        }
    }
}

fn pattern(text: &str) -> Result<Pattern, String> {
    if text.is_empty() {
        return Err("pattern must not be empty".into());
    }
    Pattern::new(text).map_err(|e| format!("invalid glob: {e}"))
}

fn search(args: GlobArgs, grep: Option<GrepArgs>) -> Result<String, String> {
    let filter = pattern(&args.pattern)?;
    let matcher = grep
        .as_ref()
        .map(|args| {
            if args.pattern.is_empty() {
                return Err("pattern must not be empty".to_owned());
            }
            let text = if args.literal {
                regex::escape(&args.pattern)
            } else {
                args.pattern.clone()
            };
            RegexBuilder::new(&text)
                .case_insensitive(args.ignore_case)
                .size_limit(2 * 1024 * 1024)
                .build()
                .map_err(|e| format!("invalid regex: {e}"))
        })
        .transpose()?;
    let root = super::files::resolve_path(args.path.as_deref().unwrap_or("."))?;
    let metadata =
        std::fs::symlink_metadata(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    if metadata.file_type().is_symlink() || (!metadata.is_file() && !metadata.is_dir()) {
        return Err("search path must be a regular file or directory, not a symlink".into());
    }
    let limit = args.limit.map_or(200, NonZeroUsize::get).min(1000);
    let options = MatchOptions {
        case_sensitive: true,
        require_literal_separator: true,
        require_literal_leading_dot: false,
    };
    let mut result = SearchResult::default();
    let mut scanned_bytes = 0;
    // Stream raw entries so excluded files consume the traversal budget too.
    // Keep enough directory handles for the depth limit; WalkDir otherwise
    // buffers remaining siblings when it closes a handle to descend further.
    let mut walker = WalkDir::new(&root)
        .follow_links(false)
        .max_depth(64)
        .max_open(65)
        .into_iter();
    let mut visited = 0;
    'entries: while let Some(entry) = walker.next() {
        if visited == MAX_ENTRIES || scanned_bytes >= MAX_SCAN_BYTES {
            result.truncated = true;
            break;
        }
        visited += 1;
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                result.warn(e.to_string());
                continue;
            }
        };
        if entry.depth() > 0 {
            let name = entry.file_name().to_string_lossy();
            if name == ".git"
                || (!args.hidden && name.starts_with('.'))
                || (entry.file_type().is_dir()
                    && matches!(name.as_ref(), "node_modules" | "target"))
            {
                if entry.file_type().is_dir() {
                    walker.skip_current_dir();
                }
                continue;
            }
        }
        if entry.file_type().is_dir() && entry.depth() == 64 {
            result.truncated = true;
            result.warn(format!("{}: depth limit reached", entry.path().display()));
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = if metadata.is_file() {
            std::path::Path::new(entry.file_name())
        } else {
            entry
                .path()
                .strip_prefix(&root)
                .map_err(|e| e.to_string())?
        };
        if !filter.matches_path_with(relative, options) {
            continue;
        }
        let Some(matcher) = &matcher else {
            if !result.push(json!(entry.path()), limit) {
                break;
            }
            continue;
        };
        let mut bytes = Vec::new();
        let read = std::fs::File::open(entry.path()).and_then(|file| {
            file.take((MAX_FILE_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
        });
        if let Err(e) = read {
            result.warn(format!("{}: {e}", entry.path().display()));
            continue;
        }
        scanned_bytes += bytes.len();
        if bytes.len() > MAX_FILE_BYTES {
            result.warn(format!("{}: exceeds 2 MiB", entry.path().display()));
            continue;
        }
        if bytes.contains(&0) {
            result.skipped_files += 1;
            continue;
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            result.skipped_files += 1;
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            if matcher.is_match(line)
                && !result.push(
                    json!({"path":entry.path(), "line":index + 1, "text":line}),
                    limit,
                )
            {
                break 'entries;
            }
        }
    }
    if result.truncated {
        result.warnings.push("Results incomplete: narrow path/pattern/glob or raise limit (maximum 1000). Scan and output budgets also apply.".into());
    }
    serde_json::to_string(&result).map_err(|e| e.to_string())
}

async fn run(args: Value, grep: bool) -> ToolResult {
    let outcome = tokio::task::spawn_blocking(move || {
        if grep {
            let args: GrepArgs =
                serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))?;
            search(
                GlobArgs {
                    pattern: args.glob.clone().unwrap_or_else(|| "**/*".into()),
                    path: args.path.clone(),
                    limit: args.limit,
                    hidden: args.hidden,
                },
                Some(args),
            )
        } else {
            search(
                serde_json::from_value(args).map_err(|e| format!("invalid arguments: {e}"))?,
                None,
            )
        }
    })
    .await;
    match outcome {
        Ok(Ok(content)) => ToolResult::ok(content),
        Ok(Err(error)) => ToolResult::err(error),
        Err(error) => ToolResult::err(error.to_string()),
    }
}

fn schema(grep: bool) -> Value {
    let mut value = json!({"type":"object","properties":{
        "pattern":{"type":"string","minLength":1},
        "path":{"type":"string","description":"Root directory or regular file; default cwd. Supports ~/"},
        "limit":{"type":"integer","minimum":1,"maximum":1000,"description":"Maximum results; default 200"},
        "hidden":{"type":"boolean","description":"Include dotfiles/directories; default false"}
    },"required":["pattern"],"additionalProperties":false});
    if grep {
        value["properties"]["glob"] =
            json!({"type":"string","description":"Root-relative file glob, e.g. **/*.rs"});
        value["properties"]["literal"] = json!({"type":"boolean","description":"Treat pattern as literal text; default false (Rust regex)"});
        value["properties"]["ignore_case"] =
            json!({"type":"boolean","description":"Case-insensitive matching; default false"});
    }
    value
}

impl Tool for Glob {
    fn name(&self) -> &'static str {
        "glob"
    }
    fn description(&self) -> &str {
        "Find regular files by root-relative glob (*, **, ?, []). Use **/* to list recursively. No shell. Skips symlinks, .git, node_modules, target and (unless hidden=true) dotfiles. Does not apply .gitignore. JSON results with truncation/skipped diagnostics. Bounded to 20,000 entries, depth 64, 32 KiB results."
    }
    fn prompt_snippet(&self, lang: Lang) -> &str {
        match lang {
            Lang::Zh => "按路径模式查找文件",
            _ => "Find files by path pattern",
        }
    }
    fn prompt_guidelines(&self, lang: Lang) -> &str {
        match lang {
            Lang::Zh => {
                "先用 glob 发现路径，再用 grep 搜索内容、read 读取文件；结果截断时缩小目录或模式。"
            }
            _ => {
                "Use glob to discover paths, grep to search content and read to inspect files. Narrow the directory or pattern when results are truncated."
            }
        }
    }
    fn schema(&self) -> Value {
        schema(false)
    }
    fn execute<'a>(
        &'a self,
        _: &'a mut ToolContext,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(run(args, false))
    }
}

impl Tool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }
    fn description(&self) -> &str {
        "Search UTF-8 files line-by-line using Rust regex (or literal=true). JSON results contain absolute path, 1-based line and text. Same traversal exclusions as glob; no .gitignore support. Skips binary/non-UTF-8 and files above 2 MiB; scans at most 64 MiB, 20,000 entries, depth 64; returns at most 32 KiB results with truncation/skipped diagnostics. No shell."
    }
    fn prompt_snippet(&self, lang: Lang) -> &str {
        match lang {
            Lang::Zh => "搜索文本内容，返回路径与行号",
            _ => "Search text with file paths and line numbers",
        }
    }
    fn prompt_guidelines(&self, lang: Lang) -> &str {
        match lang {
            Lang::Zh => {
                "内容搜索优先用 grep；普通文本可设 literal=true，用 path/glob 缩小范围。没有匹配不代表被跳过或未扫描的文件中也没有。"
            }
            _ => {
                "Prefer grep for content search; use literal=true for plain text and path/glob to narrow scope. No matches does not rule out matches in skipped or unscanned files."
            }
        }
    }
    fn schema(&self) -> Value {
        schema(true)
    }
    fn execute<'a>(
        &'a self,
        _: &'a mut ToolContext,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(run(args, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn search_reports_skipped_files_and_oversized_results() {
        let root =
            std::env::temp_dir().join(format!("koala-search-bounds-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("large.txt"), vec![b'x'; MAX_FILE_BYTES + 1]).unwrap();
        std::fs::write(root.join("invalid.txt"), [0xff]).unwrap();
        std::fs::write(root.join("binary.txt"), b"needle\0").unwrap();
        let outcome = run(json!({"path":root,"pattern":"needle"}), true).await;
        assert!(!outcome.is_error, "{}", outcome.content);
        let result: Value = serde_json::from_str(&outcome.content).unwrap();
        assert_eq!(result["results"], json!([]));
        assert_eq!(result["skipped_files"], 3);
        assert!(
            result["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v.as_str().unwrap().contains("large.txt"))
        );
        std::fs::write(root.join("long.txt"), "中".repeat(MAX_OUTPUT_BYTES)).unwrap();
        let outcome = run(json!({"path":root.join("long.txt"),"pattern":"中"}), true).await;
        let result: Value = serde_json::from_str(&outcome.content).unwrap();
        assert_eq!(result["truncated"], true);
        assert!(!result["warnings"].as_array().unwrap().is_empty());
        assert!(outcome.content.len() < MAX_OUTPUT_BYTES);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn hidden_entries_still_consume_the_traversal_budget() {
        let root =
            std::env::temp_dir().join(format!("koala-search-entries-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        for index in 0..MAX_ENTRIES {
            std::fs::write(root.join(format!(".hidden-{index}")), "").unwrap();
        }
        let outcome = run(json!({"path":root,"pattern":"**/*"}), false).await;
        let result: Value = serde_json::from_str(&outcome.content).unwrap();
        std::fs::remove_dir_all(root).unwrap();
        assert_eq!(result["results"], json!([]));
        assert_eq!(result["truncated"], true);
    }

    #[tokio::test]
    async fn depth_limit_marks_results_incomplete() {
        let root =
            std::env::temp_dir().join(format!("koala-search-depth-{}", uuid::Uuid::new_v4()));
        let mut nested = root.clone();
        for _ in 0..65 {
            nested.push("d");
        }
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("found.txt"), "needle").unwrap();
        let outcome = run(json!({"path":root,"pattern":"**/*.txt"}), false).await;
        let result: Value = serde_json::from_str(&outcome.content).unwrap();
        assert_eq!(result["results"], json!([]));
        assert_eq!(result["truncated"], true);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn searches_skip_symlinks_and_do_not_recurse_into_generated_directories() {
        let root =
            std::env::temp_dir().join(format!("koala-search-links-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("keep.txt"), "needle").unwrap();
        std::os::unix::fs::symlink(&root, root.join("loop")).unwrap();
        std::os::unix::fs::symlink(root.join("keep.txt"), root.join("link.txt")).unwrap();
        for name in [".git", "target", "node_modules"] {
            std::fs::create_dir_all(root.join(name)).unwrap();
            std::fs::write(root.join(name).join("skip.txt"), "needle").unwrap();
        }
        let outcome = run(json!({"path":root,"pattern":"needle","hidden":true}), true).await;
        assert!(!outcome.is_error, "{}", outcome.content);
        let result: Value = serde_json::from_str(&outcome.content).unwrap();
        assert_eq!(
            result["results"],
            json!([{"path":root.join("keep.txt"),"line":1,"text":"needle"}])
        );
        assert!(
            run(
                json!({"path":root.join("link.txt"),"pattern":"needle"}),
                true
            )
            .await
            .is_error
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
