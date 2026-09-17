//! Tool definitions and dispatch for KB-internal LLM processes (dream).
//! These are not exposed to the interactive agent.

use crate::llm::ToolCall;
use crate::markdown::{self, FrontMatter};
use crate::memory::FileStore;

const WRITEABLE_PREFIXES: [&str; 2] = ["daily/", "digest/"];

/// Tool definitions for the memory tools, used by dream.
pub fn definitions() -> Vec<crate::llm::Tool> {
    vec![
        crate::llm::Tool::function(
            "memory_search",
            "Search the knowledge base (daily cards and digest nodes) with BM25. \
             Returns matching chunks as `path:start_line-end_line` plus their text.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "search query"},
                    "limit": {"type": "integer", "description": "max results, default 5"}
                },
                "required": ["query"]
            }),
        ),
        crate::llm::Tool::function(
            "memory_read",
            "Read an inclusive, 1-based line range of a memory file by workspace-relative path.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "workspace-relative path"},
                    "start_line": {"type": "integer", "description": "1-based first line, default 1"},
                    "end_line": {"type": "integer", "description": "1-based last line, inclusive"}
                },
                "required": ["path"]
            }),
        ),
        crate::llm::Tool::function(
            "memory_write",
            "Write a Markdown memory file under daily/ or digest/ of the knowledge base. \
             The `.md` suffix is added when missing. `name` and `description` become the \
             frontmatter; `content` is the Markdown body. Use [[workspace/relative/path.md]] \
             wikilinks to reference other memories.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "workspace-relative path under daily/ or digest/"},
                    "name": {"type": "string", "description": "short node name (frontmatter)"},
                    "description": {"type": "string", "description": "one-line summary (frontmatter)"},
                    "content": {"type": "string", "description": "Markdown body"}
                },
                "required": ["path", "name", "description", "content"]
            }),
        ),
    ]
}

/// Execute a memory tool call directly against the store.
pub fn dispatch(kb: &mut FileStore, call: &ToolCall) -> String {
    let args: serde_json::Value = match serde_json::from_str(&call.function.arguments) {
        Ok(v) => v,
        Err(e) => return format!("invalid arguments: {e}"),
    };
    match call.function.name.as_str() {
        "memory_search" => exec_search(kb, &args),
        "memory_read" => exec_read(kb, &args),
        "memory_write" => exec_write(kb, &args),
        other => format!("unknown tool: {other}"),
    }
}

fn exec_search(kb: &FileStore, args: &serde_json::Value) -> String {
    let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(5);
    let hits = kb.search(query, limit);
    if hits.is_empty() {
        return "no matches".to_string();
    }
    hits.iter()
        .map(|h| format!("{}:{}-{}\n{}", h.path, h.start_line, h.end_line, h.text))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn exec_read(kb: &FileStore, args: &serde_json::Value) -> String {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let start = args
        .get("start_line")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(1);
    let end = args
        .get("end_line")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(usize::MAX);
    match kb.read_lines(path, start, end) {
        Ok(text) if text.is_empty() => "empty range".to_string(),
        Ok(text) => text,
        Err(e) => format!("read failed: {e}"),
    }
}

fn exec_write(kb: &mut FileStore, args: &serde_json::Value) -> String {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
    let name = args.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let description = args
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
    if name.is_empty() || content.is_empty() {
        return "name and content must not be empty".to_string();
    }
    let path = if path.ends_with(".md") {
        path.to_string()
    } else {
        format!("{path}.md")
    };
    if !WRITEABLE_PREFIXES.iter().any(|p| path.starts_with(p)) {
        return format!(
            "path must be under one of: {}",
            WRITEABLE_PREFIXES.join(", ")
        );
    }
    let fm = FrontMatter {
        name: Some(name.to_string()),
        description: Some(description.to_string()),
        extra: serde_yml::Mapping::default(),
    };
    let rendered = match markdown::render(&fm, content) {
        Ok(r) => r,
        Err(e) => return format!("render failed: {e}"),
    };
    match kb.write_file(&path, &rendered) {
        Ok(()) => format!("written: {path}"),
        Err(e) => format!("write failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{FunctionCall, ToolCall};

    fn call(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: arguments.to_string(),
            },
        }
    }

    fn temp_store() -> (std::path::PathBuf, FileStore) {
        let dir = std::env::temp_dir().join(format!("kb-agent-tools-{}", uuid::Uuid::new_v4()));
        let store = FileStore::open(&dir).unwrap();
        (dir, store)
    }

    #[test]
    fn write_then_search_roundtrip() {
        let (dir, mut store) = temp_store();
        let out = dispatch(
            &mut store,
            &call(
                "memory_write",
                serde_json::json!({
                    "path": "digest/wiki/rust-ownership",
                    "name": "rust-ownership",
                    "description": "所有权要点",
                    "content": "所有权保证内存安全。相关 [[digest/wiki/borrow.md]]"
                }),
            ),
        );
        assert_eq!(out, "written: digest/wiki/rust-ownership.md");
        assert!(dir.join("digest/wiki/rust-ownership.md").is_file());
        let hits = store.search("所有权", 5);
        assert_eq!(hits.len(), 1);
        let written = std::fs::read_to_string(dir.join("digest/wiki/rust-ownership.md")).unwrap();
        assert!(written.starts_with("---\nname: rust-ownership"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_rejects_paths_outside_memory_dirs() {
        let (dir, mut store) = temp_store();
        let out = dispatch(
            &mut store,
            &call(
                "memory_write",
                serde_json::json!({
                    "path": "session/hack",
                    "name": "x",
                    "description": "x",
                    "content": "x"
                }),
            ),
        );
        assert!(out.contains("path must be under"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn invalid_arguments_report_error() {
        let (dir, mut store) = temp_store();
        let mut bad = call("memory_search", serde_json::json!({}));
        bad.function.arguments = "{oops".into();
        assert!(dispatch(&mut store, &bad).starts_with("invalid arguments"));
        assert_eq!(
            dispatch(&mut store, &call("nope", serde_json::json!({}))),
            "unknown tool: nope"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn read_line_range() {
        let (dir, mut store) = temp_store();
        store
            .write_file("digest/wiki/a.md", "---\nname: a\n---\n\nhello\n")
            .unwrap();
        let out = dispatch(
            &mut store,
            &call(
                "memory_read",
                serde_json::json!({"path": "digest/wiki/a.md", "start_line": 2, "end_line": 2}),
            ),
        );
        assert_eq!(out, "name: a");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
