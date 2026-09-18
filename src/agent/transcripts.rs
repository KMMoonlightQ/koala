//! Persistent conversation records and strict restoration.
//! Picker/restore errors use the frontend language; write errors retain I/O context.
use crate::i18n::{self, Key, Lang};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Serialize)]
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

/// Owns the active transcript identity and its on-disk record format.
/// Restoration validates every record before changing the write target.
pub struct TranscriptStore {
    directory: PathBuf,
    id: String,
}

impl TranscriptStore {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            id: new_id(),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn path(&self) -> PathBuf {
        self.directory.join(format!("{}.jsonl", self.id))
    }

    pub fn reset(&mut self) {
        self.id = new_id();
    }

    pub fn list(&self, lang: Lang) -> Result<Vec<SessionView>, String> {
        list(&self.directory, &self.id, lang)
    }

    pub fn restore(&mut self, id: &str, lang: Lang) -> Result<Vec<Record>, String> {
        let records = read(&self.directory, id, lang)?;
        self.id = id.to_owned();
        Ok(records)
    }

    /// Encode the whole turn before touching disk. A normal write failure rolls
    /// back to the original length; this is not a crash-durability guarantee.
    pub fn append_turn(&self, input: &str, reply: &str) -> Result<(), super::AgentError> {
        self.append_turn_with(input, reply, |file, bytes| file.write_all(bytes))
    }

    // Internal I/O seam for exercising partial-write failures against real files.
    fn append_turn_with(
        &self,
        input: &str,
        reply: &str,
        write: impl FnOnce(&mut fs::File, &[u8]) -> std::io::Result<()>,
    ) -> Result<(), super::AgentError> {
        let path = self.path();
        let append = || -> std::io::Result<()> {
            fs::create_dir_all(&self.directory)?;
            let mut bytes = Vec::new();
            let ts = chrono::Local::now().to_rfc3339();
            for (role, content) in [("user", input), ("assistant", reply)] {
                serde_json::to_writer(
                    &mut bytes,
                    &Record {
                        ts: ts.clone(),
                        role: role.into(),
                        content: content.into(),
                    },
                )?;
                bytes.push(b'\n');
            }
            let mut file = fs::OpenOptions::new()
                .create(true)
                .read(true)
                .append(true)
                .open(&path)?;
            // Serialize appends so rollback cannot truncate a concurrent turn.
            file.lock()?;
            let original_len = file.metadata()?.len();
            if original_len > 0 {
                file.seek(SeekFrom::End(-1))?;
                let mut last = [0];
                file.read_exact(&mut last)?;
                if last[0] != b'\n' {
                    bytes.insert(0, b'\n');
                }
            }
            if let Err(error) = write(&mut file, &bytes) {
                if let Err(rollback) = file.set_len(original_len) {
                    return Err(std::io::Error::new(
                        error.kind(),
                        format!("{error}; transcript rollback failed: {rollback}"),
                    ));
                }
                return Err(error);
            }
            Ok(())
        };
        append().map_err(|source| super::AgentError::Io {
            path: path.display().to_string(),
            source,
        })
    }
}

fn new_id() -> String {
    format!(
        "{}-{}",
        chrono::Local::now().format("%Y%m%d-%H%M%S"),
        &uuid::Uuid::new_v4().simple().to_string()[..6]
    )
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
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(saved) = super::work::Journal::new(path.with_extension("work")).load()? {
                return Ok(saved
                    .trace
                    .into_iter()
                    .filter_map(|trace| {
                        let (role, content) = match trace {
                            super::work::Trace::User(text) => ("user", text),
                            super::work::Trace::Text(text) => ("assistant", text),
                            _ => return None,
                        };
                        Some(Record {
                            ts: String::new(),
                            role: role.into(),
                            content,
                        })
                    })
                    .collect());
            }
            return Err(read_error(&error));
        }
        Err(error) => return Err(read_error(&error)),
    };
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
    let mut seen = std::collections::HashSet::new();
    for entry in entries {
        let entry = entry
            .map_err(|e| i18n::fill(lang, Key::SessionDirEntryFailed, &[("e", &e.to_string())]))?;
        let path = entry.path();
        if path
            .extension()
            .is_none_or(|ext| ext != "jsonl" && ext != "work")
            || !entry.file_type().map_err(|e| e.to_string())?.is_file()
        {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|v| v.to_str()) else {
            continue;
        };
        if !seen.insert(id.to_owned()) {
            continue;
        }
        let modified = ["jsonl", "work"]
            .into_iter()
            .filter_map(|extension| {
                fs::symlink_metadata(path.with_extension(extension))
                    .ok()
                    .filter(|metadata| metadata.is_file())
                    .and_then(|metadata| metadata.modified().ok())
            })
            .max()
            .ok_or_else(|| "session metadata unavailable".to_owned())?;
        let title_records = match super::work::Journal::new(path.with_extension("work")).load() {
            Ok(Some(saved)) if !saved.trace.is_empty() => Ok(saved
                .trace
                .into_iter()
                .filter_map(|trace| {
                    if let super::work::Trace::User(content) = trace {
                        Some(Record {
                            ts: String::new(),
                            role: "user".into(),
                            content,
                        })
                    } else {
                        None
                    }
                })
                .collect()),
            Ok(_) => read(directory, id, lang),
            Err(error) => Err(error),
        };
        let title = match title_records {
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
    fn restored_turn_appends_without_losing_prefix_and_failed_restore_keeps_target() {
        let root = std::env::temp_dir().join(format!("koala-turn-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let prefix = r#"{"ts":"then","role":"user","content":"old"}"#;
        fs::write(root.join("saved.jsonl"), prefix).unwrap();
        let mut store = TranscriptStore::new(root.clone());
        store.restore("saved", Lang::En).unwrap();
        fs::write(root.join("broken.jsonl"), "{").unwrap();
        assert!(store.restore("broken", Lang::En).is_err());
        assert_eq!(store.id(), "saved");
        store.append_turn("你好", "回答").unwrap();
        assert!(
            fs::read_to_string(store.path())
                .unwrap()
                .starts_with(prefix)
        );
        let records = read(&root, "saved", Lang::En).unwrap();
        assert_eq!(records.len(), 3);
        assert_eq!(records[1].content, "你好");
        assert_eq!(records[2].role, "assistant");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn partial_write_rolls_back_exact_bytes_then_a_retry_writes_once() {
        let root =
            std::env::temp_dir().join(format!("koala-write-failure-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let prefix = r#"{"ts":"then","role":"user","content":"original"}"#;
        fs::write(root.join("saved.jsonl"), prefix).unwrap();
        let mut store = TranscriptStore::new(root.clone());
        store.restore("saved", Lang::En).unwrap();
        let error = store
            .append_turn_with("new", "answer", |file, bytes| {
                file.write_all(&bytes[..bytes.len() / 2])?;
                Err(std::io::Error::other("injected write failure"))
            })
            .unwrap_err();
        assert!(error.to_string().contains("injected write failure"));
        assert_eq!(fs::read_to_string(store.path()).unwrap(), prefix);
        store.append_turn("new", "answer").unwrap();
        assert_eq!(read(&root, "saved", Lang::En).unwrap().len(), 3);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn strict_restore_rejects_malformed_records() {
        let root = std::env::temp_dir().join(format!("koala-read-modes-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("mixed.jsonl");
        fs::write(
            &path,
            concat!(
                "bad json\n",
                "{\"role\":\"user\",\"content\":\"你好世界\"}\n",
                "{\"role\":\"assistant\",\"content\":\"\"}\n",
                "{\"role\":\"custom\",\"content\":\"kept\"}\n"
            ),
        )
        .unwrap();
        assert!(read(&root, "mixed", Lang::En).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn lists_newest_first_and_reports_invalid_files_without_losing_valid_sessions() {
        let root = std::env::temp_dir().join(format!("koala-sessions-{}", uuid::Uuid::new_v4()));
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
