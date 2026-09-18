//! Durable work journal, separate from the legacy conversation/distillation log.
use super::{event::*, plan::TodoItem};
use crate::llm::Message;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    sync::{Arc, Mutex},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Trace {
    User(String),
    Text(String),
    ToolStart {
        id: String,
        name: String,
        summary: String,
        arguments: String,
    },
    ToolEnd {
        id: String,
        output: String,
        is_error: bool,
        duration_ms: u64,
    },
    Todos(Vec<TodoItem>),
    Note(String),
}

#[derive(Serialize, Deserialize)]
enum Entry {
    Trace(Trace),
    Context {
        messages: Vec<Message>,
        todos: Vec<TodoItem>,
    },
    Tasks(Vec<TaskView>),
}

#[derive(Debug, Default)]
pub struct State {
    pub messages: Vec<Message>,
    pub todos: Vec<TodoItem>,
    pub trace: Vec<Trace>,
    pub tasks: Vec<TaskView>,
}

#[derive(Debug, Clone)]
pub struct Journal {
    pub path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl Journal {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Arc::new(Mutex::new(())),
        }
    }

    pub fn load(&self) -> Result<Option<State>, String> {
        let metadata = match fs::symlink_metadata(&self.path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        if !metadata.is_file() {
            return Err("work journal is not a regular file".into());
        }
        let mut file = fs::File::open(&self.path).map_err(|e| e.to_string())?;
        file.lock_shared().map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        // A newline commits an entry. A crash can leave an incomplete tail,
        // including half a UTF-8 character; only the committed prefix is read.
        let committed = bytes.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let text = std::str::from_utf8(&bytes[..committed]).map_err(|e| e.to_string())?;
        let mut state = State::default();
        let mut partial_text = String::new();
        for (line, value) in text.lines().enumerate() {
            let entry: Entry = serde_json::from_str(value)
                .map_err(|e| format!("work journal line {}: {e}", line + 1))?;
            match entry {
                Entry::Trace(trace) => {
                    if let Trace::User(text) = &trace {
                        if !partial_text.is_empty() {
                            state
                                .messages
                                .push(Message::assistant(std::mem::take(&mut partial_text)));
                        }
                        state.messages.push(Message::user(text));
                    }
                    if let Trace::Text(text) = &trace {
                        partial_text.push_str(text);
                    }
                    if let Trace::Todos(todos) = &trace {
                        state.todos = todos.clone();
                    }
                    state.trace.push(trace);
                }
                Entry::Context { messages, todos } => {
                    state.messages = messages;
                    state.todos = todos;
                    partial_text.clear();
                }
                Entry::Tasks(tasks) => state.tasks = tasks,
            }
        }
        // Never replay an unfinished tool invocation. Complete its protocol with
        // an explicit unknown-outcome result so the next request remains valid.
        let mut pending = Vec::new();
        for message in &state.messages {
            if let Some(calls) = &message.tool_calls {
                pending.extend(calls.iter().map(|c| c.id.clone()));
            }
            if let Some(id) = &message.tool_call_id {
                pending.retain(|p| p != id);
            }
        }
        for id in pending {
            state.messages.push(Message::tool(
                id,
                "Execution interrupted; outcome unknown. Inspect state before retrying.",
            ));
        }
        if !partial_text.is_empty() {
            state.messages.push(Message::assistant(partial_text));
        }
        Ok(Some(state))
    }

    fn append(&self, entry: Entry) -> std::io::Result<()> {
        let _guard = self.lock.lock().unwrap();
        fs::create_dir_all(self.path.parent().unwrap())?;
        let mut bytes = serde_json::to_vec(&entry)?;
        bytes.push(b'\n');
        let mut file = fs::OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&self.path)?;
        file.lock()?;
        let mut len = file.metadata()?.len();
        if len > 0 {
            file.seek(SeekFrom::End(-1))?;
            let mut last = [0];
            file.read_exact(&mut last)?;
            if last[0] != b'\n' {
                // Repair only an uncommitted tail before the next append.
                file.seek(SeekFrom::Start(0))?;
                let mut previous = Vec::new();
                file.read_to_end(&mut previous)?;
                len = previous
                    .iter()
                    .rposition(|b| *b == b'\n')
                    .map_or(0, |i| i + 1) as u64;
                file.set_len(len)?;
            }
        }
        if let Err(error) = file.write_all(&bytes).and_then(|_| file.sync_data()) {
            file.set_len(len)?;
            return Err(error);
        }
        Ok(())
    }
    pub fn trace(&self, trace: Trace) -> std::io::Result<()> {
        self.append(Entry::Trace(trace))
    }
    pub fn context(&self, messages: &[Message], todos: &[TodoItem]) -> std::io::Result<()> {
        self.append(Entry::Context {
            messages: messages
                .iter()
                .filter(|m| m.role != "system")
                .cloned()
                .collect(),
            todos: todos.to_vec(),
        })
    }
    pub fn tasks(&self, tasks: Vec<TaskView>) -> std::io::Result<()> {
        self.append(Entry::Tasks(tasks))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_tail_is_ignored_and_repaired_without_losing_committed_state() {
        let root = std::env::temp_dir().join(format!("koala-journal-{}", uuid::Uuid::new_v4()));
        let journal = Journal::new(root.join("session.work"));
        journal
            .trace(Trace::User("unfinished first turn".into()))
            .unwrap();
        let mut file = fs::OpenOptions::new()
            .append(true)
            .open(&journal.path)
            .unwrap();
        file.write_all(b"{\"Trace\":\"\xf0\x9f").unwrap();
        assert_eq!(
            journal.load().unwrap().unwrap().messages[0]
                .content
                .as_deref(),
            Some("unfinished first turn")
        );
        journal
            .trace(Trace::Text("partial response".into()))
            .unwrap();
        let state = journal.load().unwrap().unwrap();
        assert_eq!(state.trace.len(), 2);
        assert_eq!(
            state.messages[1].content.as_deref(),
            Some("partial response")
        );
        file.write_all(b"invalid committed entry\n").unwrap();
        assert!(journal.load().unwrap_err().contains("line 3"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unfinished_tool_protocol_is_closed_and_latest_todo_survives() {
        let root =
            std::env::temp_dir().join(format!("koala-journal-tools-{}", uuid::Uuid::new_v4()));
        let journal = Journal::new(root.join("session.work"));
        let assistant: Message = serde_json::from_value(serde_json::json!({"role":"assistant", "tool_calls":[{"id":"one","type":"function","function":{"name":"bash","arguments":"{}"}},{"id":"two","type":"function","function":{"name":"bash","arguments":"{}"}}]})).unwrap();
        journal
            .context(
                &[
                    Message::user("work"),
                    assistant,
                    Message::tool("one", "done"),
                ],
                &[],
            )
            .unwrap();
        journal
            .trace(Trace::Todos(vec![TodoItem {
                content: "still pending".into(),
                status: super::super::plan::TodoStatus::InProgress,
            }]))
            .unwrap();
        let state = journal.load().unwrap().unwrap();
        assert_eq!(state.messages.len(), 4);
        assert_eq!(state.messages[3].tool_call_id.as_deref(), Some("two"));
        assert!(
            state.messages[3]
                .content
                .as_ref()
                .unwrap()
                .contains("outcome unknown")
        );
        assert_eq!(state.todos.len(), 1);
        fs::remove_dir_all(root).unwrap();
    }
}
