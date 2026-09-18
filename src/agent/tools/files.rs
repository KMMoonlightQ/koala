//! Text file tools. Mutations validate first and replace files atomically.
use super::{Tool, ToolContext, ToolResult};
use crate::agent::file_io::atomic_write;
use crate::i18n::{self, Key, Lang};
use serde::Deserialize;
use serde_json::{Value, json};
use std::future::Future;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Mutex;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

const MAX_READ_BYTES: usize = 32 * 1024;
const MAX_READ_LINES: usize = 2000;
// Shared by foreground and background agents; an edit's read/validate/write is one operation.
static MUTATIONS: Mutex<()> = Mutex::new(());

pub struct Read;
pub struct Edit;
pub struct Write;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    path: String,
    offset: Option<NonZeroUsize>,
    limit: Option<NonZeroUsize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditArgs {
    path: String,
    edits: Vec<Replacement>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Replacement {
    #[serde(rename = "oldText")]
    old_text: String,
    #[serde(rename = "newText")]
    new_text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    path: String,
    content: String,
}

pub(super) fn resolve_path(path: &str) -> Result<PathBuf, String> {
    if path.trim().is_empty() {
        return Err("path must not be empty".into());
    }
    let path = if path == "~" || path.starts_with("~/") {
        dirs::home_dir()
            .ok_or("home directory is unavailable")?
            .join(path.strip_prefix("~/").unwrap_or(""))
    } else {
        PathBuf::from(path)
    };
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()
            .map_err(|e| e.to_string())?
            .join(path))
    }
}

async fn read_file(args: ReadArgs) -> Result<String, String> {
    let path = resolve_path(&args.path)?;
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|e| e.to_string())?;
    if !metadata.is_file() {
        return Err("read supports regular text files only".into());
    }
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file);
    let start = args.offset.map_or(1, NonZeroUsize::get);
    let limit = args
        .limit
        .map_or(MAX_READ_LINES, NonZeroUsize::get)
        .min(MAX_READ_LINES);
    let mut line = Vec::new();
    let mut output = String::new();
    let mut number = 0;
    let mut count = 0;
    loop {
        line.clear();
        let n = (&mut reader)
            .take((MAX_READ_BYTES + 1) as u64)
            .read_until(b'\n', &mut line)
            .await
            .map_err(|e| e.to_string())?;
        if n == 0 {
            if number == 0 && start == 1 {
                return Ok(format!("{}: empty file", path.display()));
            }
            if start > number {
                return Err(format!(
                    "offset {start} is beyond end of file ({number} lines)"
                ));
            }
            return Ok(format!(
                "{}\n[Lines {start}-{number}; end of file]\n{output}",
                path.display()
            ));
        }
        number += 1;
        if n > MAX_READ_BYTES {
            if count > 0 {
                break;
            }
            return Err(format!(
                "line {number} exceeds {MAX_READ_BYTES} bytes; use bash for a bounded extraction"
            ));
        }
        if number < start {
            continue;
        }
        if output.len() + n > MAX_READ_BYTES || count == limit {
            break;
        }
        let text = std::str::from_utf8(&line)
            .map_err(|_| "read supports UTF-8 text only; this file contains invalid UTF-8")?;
        output.push_str(text);
        count += 1;
    }
    Ok(format!(
        "{}\n[Lines {start}-{}; more content: call read with offset={number}]\n{output}",
        path.display(),
        number - 1
    ))
}

fn apply_edits(original: &str, edits: &[Replacement]) -> Result<String, String> {
    if edits.is_empty() {
        return Err("edits must contain at least one replacement".into());
    }
    let mut ranges = Vec::new();
    for (index, edit) in edits.iter().enumerate() {
        if edit.old_text.is_empty() {
            return Err(format!("edits[{index}].oldText must not be empty"));
        }
        let Some(start) = original.find(&edit.old_text) else {
            return Err(format!(
                "edits[{index}].oldText was not found; read the file again"
            ));
        };
        if original.rfind(&edit.old_text) != Some(start) {
            return Err(format!(
                "edits[{index}].oldText is not unique; include more surrounding text"
            ));
        }
        ranges.push((start, start + edit.old_text.len(), edit.new_text.as_str()));
    }
    ranges.sort_unstable_by_key(|(start, _, _)| *start);
    if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return Err("edits overlap; merge overlapping changes into one replacement".into());
    }
    let mut result = original.to_owned();
    for (start, end, text) in ranges.into_iter().rev() {
        result.replace_range(start..end, text);
    }
    Ok(result)
}

fn mutation_target(path: &str) -> Result<PathBuf, String> {
    let path = resolve_path(path)?;
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            let target = std::fs::canonicalize(&path).map_err(|e| e.to_string())?;
            if !target.is_file() {
                return Err("target must be a regular file".into());
            }
            Ok(target)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(path),
        Err(e) => Err(e.to_string()),
    }
}

fn edit_file(args: EditArgs) -> Result<String, String> {
    let _guard = MUTATIONS.lock().map_err(|e| e.to_string())?;
    let path = mutation_target(&args.path)?;
    let original = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let updated = apply_edits(&original, &args.edits)?;
    atomic_write(&path, updated.as_bytes()).map_err(|e| e.to_string())?;
    Ok(format!(
        "Updated {} ({} replacements)",
        path.display(),
        args.edits.len()
    ))
}

fn write_file(args: WriteArgs) -> Result<String, String> {
    let _guard = MUTATIONS.lock().map_err(|e| e.to_string())?;
    let path = mutation_target(&args.path)?;
    let parent = path.parent().ok_or("file has no parent directory")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    atomic_write(&path, args.content.as_bytes()).map_err(|e| e.to_string())?;
    Ok(format!(
        "Wrote {} bytes to {}",
        args.content.len(),
        path.display()
    ))
}

fn result(outcome: Result<String, String>) -> ToolResult {
    match outcome {
        Ok(content) => ToolResult::ok(content),
        Err(error) => ToolResult::err(error),
    }
}

impl Tool for Read {
    fn name(&self) -> &'static str {
        "read"
    }
    fn description(&self) -> &str {
        "Read a UTF-8 text file. Paths are absolute or relative to cwd; ~/ is supported. Returns at most 2000 lines / 32 KiB. Use the returned next offset to continue."
    }
    fn prompt_snippet(&self, lang: Lang) -> &str {
        i18n::text(lang, Key::ToolReadSnippet)
    }
    fn prompt_guidelines(&self, lang: Lang) -> &str {
        i18n::text(lang, Key::ToolReadRules)
    }
    fn schema(&self) -> Value {
        json!({"type":"object", "properties": {
            "path":{"type":"string"},
            "offset":{"type":"integer", "minimum":1, "description":"First line, 1-based; default 1"},
            "limit":{"type":"integer", "minimum":1, "maximum":2000, "description":"Maximum number of lines"}
        }, "required":["path"], "additionalProperties":false})
    }
    fn execute<'a>(
        &'a self,
        _ctx: &'a mut ToolContext,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            match serde_json::from_value::<ReadArgs>(args) {
                Ok(args) => result(read_file(args).await),
                Err(e) => ToolResult::err(format!("invalid arguments: {e}")),
            }
        })
    }
}

impl Tool for Edit {
    fn name(&self) -> &'static str {
        "edit"
    }
    fn description(&self) -> &str {
        "Edit an existing UTF-8 file with exact text replacements. Each oldText must be nonempty, unique and non-overlapping in the original file. All replacements are validated before any write."
    }
    fn prompt_snippet(&self, lang: Lang) -> &str {
        i18n::text(lang, Key::ToolEditSnippet)
    }
    fn prompt_guidelines(&self, lang: Lang) -> &str {
        i18n::text(lang, Key::ToolEditRules)
    }
    fn schema(&self) -> Value {
        json!({"type":"object", "properties": {
            "path":{"type":"string"},
            "edits":{"type":"array", "minItems":1, "items": {
                "type":"object", "properties": {
                    "oldText":{"type":"string", "minLength":1},
                    "newText":{"type":"string"}
                }, "required":["oldText","newText"], "additionalProperties":false
            }}
        }, "required":["path","edits"], "additionalProperties":false})
    }
    fn execute<'a>(
        &'a self,
        _ctx: &'a mut ToolContext,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            match serde_json::from_value::<EditArgs>(args) {
                Ok(args) => result(
                    tokio::task::spawn_blocking(move || edit_file(args))
                        .await
                        .unwrap_or_else(|e| Err(e.to_string())),
                ),
                Err(e) => ToolResult::err(format!("invalid arguments: {e}")),
            }
        })
    }
}

impl Tool for Write {
    fn name(&self) -> &'static str {
        "write"
    }
    fn description(&self) -> &str {
        "Create or overwrite a UTF-8 text file. Creates missing parent directories. Use edit for partial changes."
    }
    fn prompt_snippet(&self, lang: Lang) -> &str {
        i18n::text(lang, Key::ToolWriteSnippet)
    }
    fn prompt_guidelines(&self, lang: Lang) -> &str {
        i18n::text(lang, Key::ToolWriteRules)
    }
    fn schema(&self) -> Value {
        json!({"type":"object", "properties": {"path":{"type":"string"}, "content":{"type":"string"}}, "required":["path","content"], "additionalProperties":false})
    }
    fn execute<'a>(
        &'a self,
        _ctx: &'a mut ToolContext,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            match serde_json::from_value::<WriteArgs>(args) {
                Ok(args) => result(
                    tokio::task::spawn_blocking(move || write_file(args))
                        .await
                        .unwrap_or_else(|e| Err(e.to_string())),
                ),
                Err(e) => ToolResult::err(format!("invalid arguments: {e}")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        let path = std::env::temp_dir().join(format!("koala-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn replacement(old: &str, new: &str) -> Replacement {
        Replacement {
            old_text: old.into(),
            new_text: new.into(),
        }
    }

    #[test]
    fn replacements_use_original_text_and_reject_ambiguous_or_overlapping_matches() {
        assert_eq!(
            apply_edits(
                "alpha beta",
                &[replacement("alpha", "beta"), replacement("beta", "gamma")]
            )
            .unwrap(),
            "beta gamma"
        );
        assert_eq!(
            apply_edits("\u{feff}甲\r\n乙\r\n", &[replacement("乙", "丙")]).unwrap(),
            "\u{feff}甲\r\n丙\r\n"
        );
        for (original, edits) in [
            ("aaa", vec![replacement("aa", "x")]),
            (
                "abcdef",
                vec![replacement("abc", "x"), replacement("cde", "y")],
            ),
            (
                "abcdef",
                vec![replacement("abc", "x"), replacement("abc", "y")],
            ),
            ("abc", vec![replacement("", "x")]),
            ("abc", vec![]),
            ("abc", vec![replacement("absent", "x")]),
        ] {
            assert!(apply_edits(original, &edits).is_err());
        }
    }

    #[test]
    fn failed_batch_leaves_file_intact_and_write_creates_or_overwrites() {
        let root = root();
        let path = root.join("nested/file.txt");
        let filename = path.to_string_lossy().to_string();
        write_file(WriteArgs {
            path: filename.clone(),
            content: "alpha beta".into(),
        })
        .unwrap();
        let error = edit_file(EditArgs {
            path: filename.clone(),
            edits: vec![
                replacement("alpha", "changed"),
                replacement("missing", "fail"),
            ],
        })
        .unwrap_err();
        assert!(error.contains("not found"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "alpha beta");
        edit_file(EditArgs {
            path: filename.clone(),
            edits: vec![replacement("alpha", "甲"), replacement("beta", "乙")],
        })
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "甲 乙");
        write_file(WriteArgs {
            path: filename,
            content: String::new(),
        })
        .unwrap();
        assert!(std::fs::read(&path).unwrap().is_empty());
        assert_eq!(
            std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
            1
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn read_pages_by_lines_and_bytes_without_splitting_unicode() {
        let root = root();
        let path = root.join("input.txt");
        std::fs::write(&path, "甲\n乙\n丙").unwrap();
        let filename = path.to_string_lossy().to_string();
        let first = read_file(ReadArgs {
            path: filename.clone(),
            offset: None,
            limit: NonZeroUsize::new(2),
        })
        .await
        .unwrap();
        assert!(first.contains("offset=3"));
        assert!(first.ends_with("甲\n乙\n"));
        let last = read_file(ReadArgs {
            path: filename.clone(),
            offset: NonZeroUsize::new(3),
            limit: None,
        })
        .await
        .unwrap();
        assert!(last.contains("end of file"));
        assert!(last.ends_with('丙'));
        assert!(
            read_file(ReadArgs {
                path: filename.clone(),
                offset: NonZeroUsize::new(4),
                limit: None
            })
            .await
            .is_err()
        );
        std::fs::write(
            &path,
            format!("{}\n{}\n", "中".repeat(7000), "文".repeat(7000)),
        )
        .unwrap();
        let page = read_file(ReadArgs {
            path: filename.clone(),
            offset: None,
            limit: None,
        })
        .await
        .unwrap();
        assert!(page.contains("offset=2"));
        assert!(!page.contains('文'));
        assert!(page.len() < MAX_READ_BYTES + 200);
        std::fs::write(&path, "x".repeat(MAX_READ_BYTES + 1)).unwrap();
        assert!(
            read_file(ReadArgs {
                path: filename.clone(),
                offset: None,
                limit: None
            })
            .await
            .unwrap_err()
            .contains("bounded extraction")
        );
        std::fs::write(&path, [0xff]).unwrap();
        assert!(
            read_file(ReadArgs {
                path: filename.clone(),
                offset: None,
                limit: None
            })
            .await
            .is_err()
        );
        std::fs::write(&path, "").unwrap();
        assert!(
            read_file(ReadArgs {
                path: filename,
                offset: None,
                limit: None
            })
            .await
            .unwrap()
            .contains("empty file")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn mutations_follow_symlinks_and_preserve_executable_permissions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let root = root();
        let path = root.join("script");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o751)).unwrap();
        let link = root.join("link");
        symlink(&path, &link).unwrap();
        edit_file(EditArgs {
            path: link.to_string_lossy().to_string(),
            edits: vec![replacement("old", "new")],
        })
        .unwrap();
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o751
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
