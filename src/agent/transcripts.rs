//! On-disk chat transcripts used by the session picker and restoration.
//! Every error string here is shown verbatim by the session picker, so `lang`
//! is threaded in and callers pass the frontend's language.
use crate::i18n::{self, Key, Lang};
use serde::Deserialize;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Deserialize)]
pub struct Record {
    pub ts: String,
    pub role: String,
    pub content: String,
}

#[derive(Debug, Clone)]
pub struct SessionView {
    pub id: String,
    pub title: String,
    pub updated: String,
    pub current: bool,
}

pub fn read(directory: &Path, id: &str, lang: Lang) -> Result<Vec<Record>, String> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(i18n::text(lang, Key::SessionInvalidId).into());
    }
    let path = directory.join(format!("{id}.jsonl"));
    let read_error = |e: &dyn std::fmt::Display| {
        i18n::fill(
            lang,
            Key::SessionReadFailed,
            &[("id", id), ("e", &e.to_string())],
        )
    };
    // Only regular transcript files inside the configured directory are accepted.
    let metadata = fs::symlink_metadata(&path).map_err(|e| read_error(&e))?;
    if !metadata.file_type().is_file() {
        return Err(i18n::fill(lang, Key::SessionNotAFile, &[("id", id)]));
    }
    let text = fs::read_to_string(&path).map_err(|e| read_error(&e))?;
    let mut records = Vec::new();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: Record = serde_json::from_str(line).map_err(|e| {
            i18n::fill(
                lang,
                Key::SessionLineCorrupt,
                &[
                    ("id", id),
                    ("line", &(index + 1).to_string()),
                    ("e", &e.to_string()),
                ],
            )
        })?;
        if !matches!(record.role.as_str(), "user" | "assistant") {
            return Err(i18n::fill(
                lang,
                Key::SessionLineInvalidRole,
                &[("id", id), ("line", &(index + 1).to_string())],
            ));
        }
        records.push(record);
    }
    if records.is_empty() {
        return Err(i18n::fill(lang, Key::SessionNoRecords, &[("id", id)]));
    }
    Ok(records)
}

pub fn list(directory: &Path, current: &str, lang: Lang) -> Result<Vec<SessionView>, String> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(i18n::fill(
                lang,
                Key::SessionDirReadFailed,
                &[("e", &e.to_string())],
            ));
        }
    };
    let mut sessions = Vec::new();
    for entry in entries {
        let entry = entry
            .map_err(|e| i18n::fill(lang, Key::SessionDirEntryFailed, &[("e", &e.to_string())]))?;
        let path = entry.path();
        if path.extension().is_none_or(|ext| ext != "jsonl")
            || !entry.file_type().map_err(|e| e.to_string())?.is_file()
        {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|v| v.to_str()) else {
            continue;
        };
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .map_err(|e| e.to_string())?;
        let title = match read(directory, id, lang) {
            Ok(records) => records
                .iter()
                .find(|r| r.role == "user")
                .map(|r| {
                    r.content
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" ")
                        .chars()
                        .take(64)
                        .collect()
                })
                .unwrap_or_else(|| i18n::text(lang, Key::UntitledSession).into()),
            Err(_) => i18n::text(lang, Key::UnreadableSession).into(),
        };
        sessions.push((
            modified,
            SessionView {
                id: id.into(),
                title,
                updated: chrono::DateTime::<chrono::Local>::from(modified)
                    .format("%Y-%m-%d %H:%M")
                    .to_string(),
                current: id == current,
            },
        ));
    }
    sessions.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.id.cmp(&a.1.id)));
    Ok(sessions.into_iter().map(|(_, view)| view).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_newest_first_and_reports_invalid_files_without_losing_valid_sessions() {
        let root = std::env::temp_dir().join(format!("kb-sessions-{}", uuid::Uuid::new_v4()));
        assert!(list(&root, "", Lang::En).unwrap().is_empty());
        fs::create_dir_all(&root).unwrap();
        let record = r#"{"ts":"2026-09-18T10:00:00+08:00","role":"user","content":"查找\n旧会话"}"#;
        fs::write(root.join("old.jsonl"), record).unwrap();
        fs::File::options()
            .write(true)
            .open(root.join("old.jsonl"))
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(std::time::UNIX_EPOCH))
            .unwrap();
        fs::write(root.join("new.jsonl"), record).unwrap();
        fs::write(root.join(".input-history"), "ignored").unwrap();
        let sessions = list(&root, "new", Lang::En).unwrap();
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].id, "new");
        assert!(sessions[0].current);
        assert_eq!(sessions[0].title, "查找 旧会话");
        assert_eq!(read(&root, "old", Lang::En).unwrap()[0].role, "user");
        fs::write(root.join("bad.jsonl"), format!("{record}\n{{")).unwrap();
        assert!(read(&root, "bad", Lang::En).unwrap_err().contains("line 2"));
        assert!(
            read(&root, "bad", Lang::Zh)
                .unwrap_err()
                .contains("第 2 行损坏")
        );
        assert_eq!(list(&root, "", Lang::En).unwrap().len(), 3);
        fs::write(
            root.join("system.jsonl"),
            r#"{"ts":"now","role":"system","content":"not a conversation"}"#,
        )
        .unwrap();
        assert!(read(&root, "system", Lang::En).is_err());
        fs::write(root.join("empty.jsonl"), "").unwrap();
        assert!(read(&root, "empty", Lang::En).is_err());
        assert!(read(&root, "../old", Lang::En).is_err());
        assert!(read(&root, "missing", Lang::En).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("old.jsonl"), root.join("link.jsonl")).unwrap();
            assert!(read(&root, "link", Lang::En).is_err());
            assert!(
                !list(&root, "", Lang::En)
                    .unwrap()
                    .iter()
                    .any(|s| s.id == "link")
            );
        }
        fs::remove_dir_all(root).unwrap();
    }
}
