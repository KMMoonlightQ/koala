pub mod agentmem;
pub mod background;
pub mod compact;
pub mod event;
pub mod hooks;
pub mod permissions;
pub mod plan;
pub(crate) mod process;
pub mod prompt;
pub mod react;
pub mod retry;
pub mod session;
pub mod skills;
pub mod subagent;
pub mod tools;

use crate::config::Config;
use crate::llm::{LlmClient, LlmError, Message};
use agentmem::AgentMemory;
use background::BackgroundManager;
use event::{EventSender, UiEvent};
use hooks::Hooks;
use permissions::Permissions;
use plan::TodoList;
use serde::Serialize;
use skills::Skills;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tools::ToolContext;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("extension: {0}")]
    Extension(String),
    #[error(transparent)]
    Llm(#[from] LlmError),
    #[error("io error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("llm.model is not configured")]
    MissingModel,
}

/// Subsystems shared with tools, sub-agents and background tasks.
pub struct SharedState {
    pub llm: LlmClient,
    pub permissions: Permissions,
    pub hooks: Hooks,
    pub extensions: crate::extensions::Extensions,
    pub max_tool_rounds: usize,
    pub max_retries: usize,
    pub compact_threshold: usize,
    pub subagent_max_rounds: usize,
    pub memory_file: PathBuf,
}

pub struct Agent {
    shared: Arc<SharedState>,
    todos: TodoList,
    agent_memory: AgentMemory,
    skills: Arc<Skills>,
    background: BackgroundManager,
    session_id: String,
    session_dir: PathBuf,
    history: Vec<Message>,
    plan_mode: bool,
    pending_input: Option<String>,
}

#[derive(Serialize)]
struct SessionRecord<'a> {
    ts: String,
    role: &'a str,
    content: &'a str,
}

impl Agent {
    pub fn new(cfg: &Config) -> Result<Self, AgentError> {
        if cfg.llm.model.is_empty() {
            return Err(AgentError::MissingModel);
        }
        Ok(Self {
            shared: Arc::new(SharedState {
                llm: LlmClient::new(
                    &cfg.llm.base_url,
                    &cfg.llm.api_key,
                    &cfg.llm.model,
                    &cfg.llm.headers,
                ),
                permissions: Permissions::new(&cfg.permissions),
                hooks: Hooks::new(&cfg.hooks),
                extensions: crate::extensions::load(cfg).map_err(AgentError::Extension)?,
                max_tool_rounds: cfg.agent.max_tool_rounds,
                max_retries: cfg.agent.max_retries,
                compact_threshold: cfg.agent.compact_threshold,
                subagent_max_rounds: cfg.agent.subagent_max_rounds,
                memory_file: cfg.agent.memory_file.clone(),
            }),
            agent_memory: AgentMemory::load(cfg.agent.memory_file.clone()),
            skills: Arc::new(Skills::load()),
            todos: TodoList::default(),
            background: BackgroundManager::default(),
            session_id: new_session_id(),
            session_dir: cfg.agent.session_dir.clone(),
            history: Vec::new(),
            plan_mode: false,
            pending_input: None,
        })
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn new_session(&mut self) {
        self.session_id = new_session_id();
        self.history.clear();
        self.todos = TodoList::default();
        self.pending_input = None;
    }

    pub fn toggle_plan_mode(&mut self) -> bool {
        self.plan_mode = !self.plan_mode;
        self.plan_mode
    }

    pub fn plan_mode(&self) -> bool {
        self.plan_mode
    }

    pub fn skills(&self) -> &Skills {
        &self.skills
    }

    pub fn tasks_render(&self) -> String {
        self.background.render()
    }

    /// Manual /compact. Returns true when history was compacted.
    pub async fn compact_now(&mut self) -> Result<bool, AgentError> {
        let payload = serde_json::json!({"session": self.session_id, "messages": self.history});
        self.shared
            .extensions
            .hook(crate::extensions::Stage::BeforeCompact, payload)
            .await
            .map_err(AgentError::Extension)?;
        let changed = compact::compact(&self.shared.llm, &mut self.history).await?;
        self.shared.extensions.hook(crate::extensions::Stage::AfterCompact, serde_json::json!({"session": self.session_id, "messages": self.history, "changed": changed})).await.map_err(AgentError::Extension)?;
        Ok(changed)
    }

    pub async fn run_turn(
        &mut self,
        input: &str,
        events: EventSender,
    ) -> Result<String, AgentError> {
        self.pending_input = Some(input.to_owned());
        if let hooks::HookOutcome::Failed(reason) = hooks::run_all(
            &self.shared.hooks.turn_start,
            &serde_json::json!({"hook": "turn_start", "session": self.session_id}),
        )
        .await
        {
            let _ = events.send(UiEvent::Note(format!("hook: {reason}")));
        }

        let extension = self.shared.extensions.hook(crate::extensions::Stage::TurnStart,
            serde_json::json!({"session": self.session_id, "input": input, "plan_mode": self.plan_mode, "depth": 0})).await.map_err(AgentError::Extension)?;
        let mut system = prompt::build_system(
            &self.agent_memory.content(),
            &self.skills,
            &self.todos,
            self.plan_mode,
        );

        if let Some(context) = extension.context {
            system.push_str(&context);
        }
        let mut messages = Vec::with_capacity(self.history.len() + 2);
        messages.push(Message::system(system));
        messages.extend(self.history.iter().cloned());
        messages.push(Message::user(input));

        let reply = {
            let mut ctx = ToolContext {
                todos: &mut self.todos,
                agent_memory: &self.agent_memory,
                background: self.background.clone(),
                skills: &self.skills,
                events: &events,
                shared: &self.shared,
                depth: 0,
                plan_mode: self.plan_mode,
            };
            match react::run(&mut ctx, &mut messages, self.shared.max_tool_rounds).await {
                Ok(reply) => reply,
                Err(error) => {
                    if let Err(reason) = self.shared.extensions.hook(crate::extensions::Stage::TurnEnd,
                        serde_json::json!({"session": self.session_id, "input": input, "error": error.to_string(), "depth": 0, "plan_mode": self.plan_mode})).await {
                        let _ = events.send(UiEvent::Note(format!("extension: {reason}")));
                    }
                    return Err(error);
                }
            }
        };

        self.history.push(Message::user(input));
        self.history.push(Message::assistant(&reply));
        self.pending_input = None;
        self.append_session("user", input)?;
        self.append_session("assistant", &reply)?;

        if compact::estimate_chars(&self.history) > self.shared.compact_threshold {
            let _ = events.send(UiEvent::Status("压缩上下文中".into()));
            if let Err(e) = self.compact_now().await {
                let _ = events.send(UiEvent::Note(format!("compact failed: {e}")));
            }
        }

        if let hooks::HookOutcome::Failed(reason) = hooks::run_all(
            &self.shared.hooks.turn_end,
            &serde_json::json!({"hook": "turn_end", "session": self.session_id}),
        )
        .await
        {
            let _ = events.send(UiEvent::Note(format!("hook: {reason}")));
        }
        if let Err(reason) = self.shared.extensions.hook(crate::extensions::Stage::TurnEnd,
            serde_json::json!({"session": self.session_id, "input": input, "reply": reply, "session_path": self.session_path(), "plan_mode": self.plan_mode, "depth": 0})).await {
            let _ = events.send(UiEvent::Note(format!("extension: {reason}")));
        }
        Ok(reply)
    }

    /// Preserve visible progress when a foreground future is dropped. This is
    /// plain conversation text, not an unfinished tool-call protocol exchange.
    pub(super) fn record_interruption(&mut self, progress: &str) -> Result<(), AgentError> {
        if let Some(input) = self.pending_input.take() {
            let reply = format!(
                "{progress}\n[本轮已中断。已完成的操作不会撤销；无结果的工具可能已产生副作用，请先核实再继续。]"
            );
            self.history.push(Message::user(&input));
            self.history.push(Message::assistant(&reply));
            self.append_session("user", &input)?;
            self.append_session("assistant", &reply)?;
        }
        Ok(())
    }

    fn append_session(&self, role: &str, content: &str) -> Result<(), AgentError> {
        let path = self.session_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| AgentError::Io {
                path: parent.display().to_string(),
                source,
            })?;
        }
        let record = SessionRecord {
            ts: chrono::Local::now().to_rfc3339(),
            role,
            content,
        };
        let mut line = serde_json::to_string(&record).unwrap_or_default();
        line.push('\n');
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| AgentError::Io {
                path: path.display().to_string(),
                source,
            })?;
        file.write_all(line.as_bytes())
            .map_err(|source| AgentError::Io {
                path: path.display().to_string(),
                source,
            })
    }

    fn session_path(&self) -> PathBuf {
        self.session_dir.join(format!("{}.jsonl", self.session_id))
    }
}

fn new_session_id() -> String {
    let date = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let short = &uuid::Uuid::new_v4().simple().to_string()[..6];
    format!("{date}-{short}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_is_filesystem_safe() {
        let id = new_session_id();
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        assert_eq!(id.len(), 15 + 1 + 6);
    }
}
