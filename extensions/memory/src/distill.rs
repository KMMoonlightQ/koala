use crate::llm::{LlmClient, LlmError, Message};
use crate::markdown;
use crate::{FileStore, MemoryError};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DistillError {
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error(transparent)]
    Memory(#[from] MemoryError),
    #[error("session file is empty or missing: {0}")]
    EmptySession(String),
    #[error("llm returned no usable markdown")]
    NoMarkdown,
}

const MAX_TRANSCRIPT_CHARS: usize = 12_000;

/// Distill a session jsonl file into `daily/<date>/<slug>.md`. Returns the written path.
pub async fn distill_session(
    llm: &LlmClient,
    store: &mut FileStore,
    session_path: &Path,
) -> Result<String, DistillError> {
    let transcript = read_transcript(session_path)?;
    let source = session_path
        .strip_prefix(store.workspace())
        .unwrap_or(session_path)
        .to_string_lossy()
        .replace('\\', "/");
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    let prompt = format!(
        "以下是一段对话的记录。请把它蒸馏成一张 daily 记忆卡片，用对话的主要语言书写。\n\
         要求：\n\
         1. 输出且只输出一个 Markdown 文档，以 frontmatter 开头，包含 name（小写短横线 slug）和 description（一句话摘要）。\n\
         2. 正文提炼：发生的事实、得出的结论、遗留的待办。不要逐条复述对话。\n\
         3. 末尾必须有 ## Sources 章节，用完整句子引用来源 [[{source}]]。\n\
         4. 不要用代码围栏包裹整个文档，不要输出任何解释。\n\n\
         对话记录：\n{transcript}"
    );
    let reply = llm
        .chat(&[Message::user(prompt)], None)
        .await?
        .content
        .unwrap_or_default();
    let markdown_text = strip_code_fence(&reply);
    let parsed = markdown::parse(markdown_text).map_err(|_| DistillError::NoMarkdown)?;
    let fm = parsed.frontmatter.ok_or(DistillError::NoMarkdown)?;
    let name = fm.name.ok_or(DistillError::NoMarkdown)?;
    let slug = slugify(&name);
    if slug.is_empty() {
        return Err(DistillError::NoMarkdown);
    }
    for suffix in 1u64.. {
        let name = if suffix == 1 {
            slug.clone()
        } else {
            format!("{slug}-{suffix}")
        };
        let path = format!("daily/{today}/{name}.md");
        match store.create_file(&path, markdown_text) {
            Ok(()) => return Ok(path),
            Err(MemoryError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::AlreadyExists =>
            {
                continue;
            }
            Err(error) => return Err(error.into()),
        }
    }
    unreachable!("exhausted all card suffixes")
}

fn read_transcript(session_path: &Path) -> Result<String, DistillError> {
    let out = distillation_text(session_path, MAX_TRANSCRIPT_CHARS)
        .map_err(|_| DistillError::EmptySession(session_path.display().to_string()))?;
    if out.trim().is_empty() {
        return Err(DistillError::EmptySession(
            session_path.display().to_string(),
        ));
    }
    Ok(out)
}

fn strip_code_fence(text: &str) -> &str {
    let trimmed = text.trim();
    let body = trimmed
        .strip_prefix("```markdown")
        .or_else(|| trimmed.strip_prefix("```md"))
        .or_else(|| trimmed.strip_prefix("```"))
        .map(str::trim_start)
        .unwrap_or(trimmed);
    body.strip_suffix("```").map(str::trim_end).unwrap_or(body)
}

pub fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(&c) {
            slug.push(c);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_end_matches('-').to_string()
}

/// A best-effort text projection for distillation. Unlike restoration, this
/// accepts incomplete records and skips malformed lines. The limit is bytes,
/// rounded down to a UTF-8 boundary, matching the model-input budget.
pub fn distillation_text(path: &Path, max_bytes: usize) -> std::io::Result<String> {
    let raw = std::fs::read_to_string(path)?;
    let mut out = String::new();
    for line in raw.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let role = value.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let content = value.get("content").and_then(|v| v.as_str()).unwrap_or("");
        if content.is_empty() {
            continue;
        }
        out.push_str(role);
        out.push_str(": ");
        out.push_str(content);
        out.push_str("\n\n");
        if out.len() > max_bytes {
            let mut end = max_bytes;
            while !out.is_char_boundary(end) {
                end -= 1;
            }
            out.truncate(end);
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn same_topic_distillations_preserve_both_sessions() {
        use crate::test_support::{MockLlm, reply};
        let root =
            std::env::temp_dir().join(format!("koala-distill-collision-{}", uuid::Uuid::new_v4()));
        let mut store = FileStore::open(&root).unwrap();
        let mock = MockLlm::start(
            (1..=2)
                .map(|n| {
                    reply(Message::assistant(format!(
                        "---\nname: same-topic\ndescription: test\n---\nunique-fact-{n}\n"
                    )))
                })
                .collect(),
        )
        .await;
        let mut paths = Vec::new();
        for n in 1..=2 {
            let session = root.join(format!("session-{n}.jsonl"));
            std::fs::write(
                &session,
                serde_json::json!({"role":"user", "content":format!("fact-{n}")}).to_string(),
            )
            .unwrap();
            paths.push(
                distill_session(&mock.client, &mut store, &session)
                    .await
                    .unwrap(),
            );
        }
        assert_ne!(paths[0], paths[1]);
        for (n, path) in paths.iter().enumerate() {
            assert!(
                store
                    .read_lines(path, 1, usize::MAX)
                    .unwrap()
                    .contains(&format!("unique-fact-{}", n + 1))
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn transcript_projection_tolerates_bad_lines_and_truncates_utf8() {
        let path = std::env::temp_dir().join(format!("koala-projection-{}", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            concat!(
                "bad json\n",
                "{\"role\":\"user\",\"content\":\"你好世界\"}\n",
                "{\"role\":\"assistant\",\"content\":\"\"}\n",
                "{\"role\":\"custom\",\"content\":\"kept\"}\n"
            ),
        )
        .unwrap();
        assert_eq!(
            distillation_text(&path, 12000).unwrap(),
            "user: 你好世界\n\ncustom: kept\n\n"
        );
        assert_eq!(distillation_text(&path, 10).unwrap(), "user: 你");
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn long_unicode_transcript_preserves_valid_text() {
        let path = std::env::temp_dir().join(format!("koala-distill-{}", uuid::Uuid::new_v4()));
        std::fs::write(
            &path,
            serde_json::json!({"role": "user", "content": "a中".repeat(5000)}).to_string(),
        )
        .unwrap();
        let text = read_transcript(&path).unwrap();
        assert!(text.starts_with("user: a中"));
        assert!(text.len() <= MAX_TRANSCRIPT_CHARS);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn strip_fence_variants() {
        assert_eq!(strip_code_fence("```markdown\n# A\n```"), "# A");
        assert_eq!(strip_code_fence("# A"), "# A");
        assert_eq!(strip_code_fence("```\n# A\n```\n"), "# A");
    }

    #[test]
    fn slugify_mixed_scripts() {
        assert_eq!(slugify("Rust 所有权笔记!"), "rust-所有权笔记");
        assert_eq!(slugify("  Hello World  "), "hello-world");
        assert_eq!(slugify("---"), "");
    }
}
