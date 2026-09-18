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
pub mod transcripts;

use crate::config::{Config, HooksConfig};
use crate::i18n::{self, Key, Lang, LangCell};
use crate::llm::{LlmClient, LlmError, Message};
use agentmem::AgentMemory;
use background::BackgroundManager;
use event::{EventSender, UiEvent};
use permissions::Permissions;
use plan::TodoList;
use skills::Skills;
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
    #[error("invalid model settings: {0}")]
    InvalidModelSettings(String),
    #[error("tool round limit reached ({0}); completed operations were not rolled back")]
    ToolRoundLimit(usize),
}

/// Subsystems shared with tools, sub-agents and background tasks.
pub struct SharedState {
    pub llm: LlmClient,
    pub permissions: Permissions,
    pub hooks: HooksConfig,
    pub extensions: crate::extensions::Extensions,
    pub max_tool_rounds: Option<usize>,
    pub max_retries: usize,
    pub compact_threshold: usize,
    pub subagent_max_rounds: Option<usize>,
    pub memory_file: PathBuf,
    /// Interface language, shared with sub-agents and background work.
    pub lang: LangCell,
}

pub struct Agent {
    models: Vec<crate::config::ModelConfig>,
    reasoning_efforts: Vec<String>,
    context_window: Option<u64>,
    shared: Arc<SharedState>,
    todos: TodoList,
    agent_memory: AgentMemory,
    skills: Arc<Skills>,
    background: BackgroundManager,
    transcript: transcripts::TranscriptStore,
    history: Vec<Message>,
    plan_mode: bool,
    pending_input: Option<String>,
}

impl Agent {
    pub fn new(cfg: &Config) -> Result<Self, AgentError> {
        if cfg.llm.model.is_empty() {
            return Err(AgentError::MissingModel);
        }
        let models = cfg
            .llm
            .selectable_models()
            .map_err(AgentError::InvalidModelSettings)?;
        let selected = models.iter().find(|m| m.model == cfg.llm.model).unwrap();
        let efforts = &selected.reasoning_efforts;
        let llm = LlmClient::new(
            &cfg.llm.base_url,
            &cfg.llm.api_key,
            &cfg.llm.model,
            &cfg.llm.headers,
        );
        llm.set_reasoning_effort(
            selected
                .reasoning_effort
                .clone()
                .or_else(|| efforts.first().cloned()),
        );
        Ok(Self {
            reasoning_efforts: efforts.clone(),
            context_window: selected.context_window.map(|v| v.get()),
            models,
            shared: Arc::new(SharedState {
                llm,
                permissions: Permissions::new(&cfg.permissions),
                hooks: cfg.hooks.clone(),
                extensions: crate::extensions::load(cfg).map_err(AgentError::Extension)?,
                max_tool_rounds: cfg.agent.max_tool_rounds,
                max_retries: cfg.agent.max_retries,
                compact_threshold: cfg.agent.compact_threshold,
                lang: LangCell::new(cfg.lang),
                subagent_max_rounds: cfg.agent.subagent_max_rounds,
                memory_file: cfg.agent.memory_file.clone(),
            }),
            agent_memory: AgentMemory::load(cfg.agent.memory_file.clone()),
            skills: Arc::new(Skills::load()),
            todos: TodoList::default(),
            background: BackgroundManager::default(),
            transcript: transcripts::TranscriptStore::new(cfg.agent.session_dir.clone()),
            history: Vec::new(),
            plan_mode: false,
            pending_input: None,
        })
    }

    pub fn model_settings(&self) -> UiEvent {
        UiEvent::ModelSettings {
            model: self.shared.llm.model(),
            models: self.models.iter().map(|m| m.model.clone()).collect(),
            reasoning_efforts: self.reasoning_efforts.clone(),
            reasoning_effort: self.shared.llm.reasoning_effort(),
            context_window: self.context_window,
        }
    }

    pub fn select_model(&mut self, name: &str) -> Result<(), String> {
        let selected = self
            .models
            .iter()
            .find(|m| m.model == name)
            .ok_or_else(|| {
                i18n::fill(self.lang(), Key::ErrModelNotConfigured, &[("name", name)])
            })?;
        if self.shared.llm.model() == name {
            return Ok(());
        }
        let effort = selected
            .reasoning_effort
            .clone()
            .or_else(|| selected.reasoning_efforts.first().cloned());
        self.shared.llm.select_model(selected.model.clone(), effort);
        self.reasoning_efforts = selected.reasoning_efforts.clone();
        self.context_window = selected.context_window.map(|v| v.get());
        Ok(())
    }

    pub fn set_reasoning_effort(&self, requested: &str) -> Result<String, String> {
        if self.reasoning_efforts.is_empty() {
            return Err(i18n::text(self.lang(), Key::NoteNoReasoningEfforts).into());
        }
        let selected = self
            .reasoning_efforts
            .iter()
            .find(|v| v.as_str() == requested)
            .ok_or_else(|| {
                i18n::fill(
                    self.lang(),
                    Key::ErrUnsupportedEffort,
                    &[
                        ("requested", requested),
                        ("available", &self.reasoning_efforts.join(", ")),
                    ],
                )
            })?;
        self.shared.llm.set_reasoning_effort(Some(selected.clone()));
        Ok(selected.clone())
    }

    pub fn session_id(&self) -> &str {
        self.transcript.id()
    }

    /// Interface language used for messages and the system prompt.
    pub fn lang(&self) -> Lang {
        self.shared.lang.get()
    }

    pub fn set_lang(&self, lang: Lang) {
        self.shared.lang.set(lang);
    }

    pub fn list_sessions(&self) -> Result<Vec<transcripts::SessionView>, String> {
        self.transcript.list(self.lang())
    }

    pub fn restore_session(&mut self, id: &str) -> Result<Vec<transcripts::Record>, String> {
        // Validate the entire file before changing any live session state.
        let records = self.transcript.restore(id, self.lang())?;
        self.history = records
            .iter()
            .map(|r| {
                if r.role == "user" {
                    Message::user(&r.content)
                } else {
                    Message::assistant(&r.content)
                }
            })
            .collect();
        self.background = self.background.new_scope();
        self.todos = TodoList::default();
        self.pending_input = None;
        self.plan_mode = false;
        self.shared
            .permissions
            .set_mode(crate::config::PermissionMode::Normal);
        Ok(records)
    }

    pub fn new_session(&mut self) {
        self.transcript.reset();
        self.background = self.background.new_scope();
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

    /// Manual /compact. Returns true when history was compacted.
    pub async fn compact_now(&mut self) -> Result<bool, AgentError> {
        let payload =
            serde_json::json!({"session": self.transcript.id(), "messages": self.history});
        self.shared
            .extensions
            .hook(crate::extensions::Stage::BeforeCompact, payload)
            .await
            .map_err(AgentError::Extension)?;
        let changed = compact::compact(&self.shared.llm, &mut self.history).await?;
        self.shared.extensions.hook(crate::extensions::Stage::AfterCompact, serde_json::json!({"session": self.transcript.id(), "messages": self.history, "changed": changed})).await.map_err(AgentError::Extension)?;
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
            &serde_json::json!({"hook": "turn_start", "session": self.transcript.id()}),
        )
        .await
        {
            let _ = events.send(UiEvent::Note(format!("hook: {reason}")));
        }

        let extension = self.shared.extensions.hook(crate::extensions::Stage::TurnStart,
            serde_json::json!({"session": self.transcript.id(), "input": input, "plan_mode": self.plan_mode, "depth": 0})).await.map_err(AgentError::Extension)?;
        let mut system = prompt::build_system(
            &self
                .agent_memory
                .content()
                .map_err(|source| AgentError::Io {
                    path: self.shared.memory_file.display().to_string(),
                    source,
                })?,
            &self.skills,
            &self.todos,
            self.plan_mode,
            self.lang(),
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
                        serde_json::json!({"session": self.transcript.id(), "input": input, "error": error.to_string(), "depth": 0, "plan_mode": self.plan_mode})).await {
                        let _ = events.send(UiEvent::Note(format!("extension: {reason}")));
                    }
                    return Err(error);
                }
            }
        };

        self.record_turn(input, &reply)?;

        if compact::estimate_chars(&self.history) > self.shared.compact_threshold {
            let _ = events.send(UiEvent::Status(
                i18n::text(self.lang(), Key::StatusCompacting).into(),
            ));
            match self.compact_now().await {
                Ok(true) => {
                    let _ = events.send(UiEvent::ContextUsage(None));
                }
                Ok(false) => {}
                Err(e) => {
                    let _ = events.send(UiEvent::Note(format!("compact failed: {e}")));
                }
            }
        }

        if let hooks::HookOutcome::Failed(reason) = hooks::run_all(
            &self.shared.hooks.turn_end,
            &serde_json::json!({"hook": "turn_end", "session": self.transcript.id()}),
        )
        .await
        {
            let _ = events.send(UiEvent::Note(format!("hook: {reason}")));
        }
        if let Err(reason) = self.shared.extensions.hook(crate::extensions::Stage::TurnEnd,
            serde_json::json!({"session": self.transcript.id(), "input": input, "reply": reply, "session_path": self.transcript.path(), "plan_mode": self.plan_mode, "depth": 0})).await {
            let _ = events.send(UiEvent::Note(format!("extension: {reason}")));
        }
        Ok(reply)
    }

    /// Preserve visible progress when a foreground future is dropped. This is
    /// plain conversation text, not an unfinished tool-call protocol exchange.
    pub(super) fn record_interruption(&mut self, progress: &str) -> Result<(), AgentError> {
        if let Some(input) = self.pending_input.take() {
            let reply = format!(
                "{}\n{}",
                progress,
                i18n::text(self.lang(), Key::InterruptNotice)
            );
            self.record_turn(&input, &reply)?;
        }
        Ok(())
    }

    fn record_turn(&mut self, input: &str, reply: &str) -> Result<(), AgentError> {
        // A completed reply stays in memory even if persistence fails. Clear
        // pending first so session completion cannot save it again as interrupted.
        self.history.push(Message::user(input));
        self.history.push(Message::assistant(reply));
        self.pending_input = None;
        self.transcript.append_turn(input, reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mock_agent(url: &str) -> (Agent, PathBuf) {
        let root = std::env::temp_dir().join(format!("kb-agent-review-{}", uuid::Uuid::new_v4()));
        let mut cfg = Config::default();
        cfg.permissions.mode = crate::config::PermissionMode::AskWhenNeed;
        cfg.llm.model = "test".into();
        cfg.llm.base_url = url.into();
        cfg.extensions.memory = false;
        cfg.agent.memory_file = root.join("memory.md");
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.max_tool_rounds = Some(1);
        (Agent::new(&cfg).unwrap(), root)
    }

    #[tokio::test]
    async fn background_results_reach_model_and_stay_in_their_session() {
        use crate::test_support::{MockLlm, stream};
        let mut mock = MockLlm::start(vec![stream(serde_json::json!({"content": "ok"})); 3]).await;
        let (mut agent, root) = mock_agent(&mock.url);
        let id = agent.background.register("task", "research");
        agent
            .background
            .finish(id, true, "UNIQUE_BACKGROUND_RESULT".into());
        let old_background = agent.background.clone();
        let late = old_background.register("bash", "late");
        agent
            .run_turn("summarize", event::null_events())
            .await
            .unwrap();
        assert!(
            mock.request().await["messages"]
                .to_string()
                .contains("UNIQUE_BACKGROUND_RESULT")
        );
        agent.new_session();
        old_background.finish(late, true, "STALE_BACKGROUND_RESULT".into());
        agent
            .run_turn("new conversation", event::null_events())
            .await
            .unwrap();
        let request = mock.request().await["messages"].to_string();
        assert!(!request.contains("UNIQUE_BACKGROUND_RESULT"));
        assert!(!request.contains("STALE_BACKGROUND_RESULT"));
        let id = agent.background.register("bash", "new task");
        agent
            .background
            .finish(id, false, "NEW_BACKGROUND_FAILURE".into());
        agent.run_turn("check", event::null_events()).await.unwrap();
        assert!(
            mock.request().await["messages"]
                .to_string()
                .contains("NEW_BACKGROUND_FAILURE")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn tool_round_limit_reports_failure_without_executing_extra_round() {
        use crate::test_support::{MockLlm, stream};
        let call = |text: &str| serde_json::json!({"tool_calls": [{"index": 0, "id": "c", "function": {"name": "remember", "arguments": serde_json::json!({"text": text}).to_string()}}]});
        let mock = MockLlm::start(vec![
            stream(call("first allowed write")),
            stream(call("EXTRA_WRITE")),
        ])
        .await;
        let (mut agent, root) = mock_agent(&mock.url);
        let outcome = agent.run_turn("work", event::null_events()).await;
        assert!(outcome.is_err(), "round exhaustion must not be successful");
        let memory = agent.agent_memory.content().unwrap();
        assert!(memory.contains("first allowed write"));
        assert!(!memory.contains("EXTRA_WRITE"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn final_answer_is_allowed_after_last_tool_round() {
        use crate::test_support::{MockLlm, stream};
        let mock = MockLlm::start(vec![
            stream(serde_json::json!({"tool_calls": [{"index": 0, "id": "c", "function": {"name": "remember", "arguments": "{\"text\":\"fact\"}"}}]})),
            stream(serde_json::json!({"content": "finished"})),
        ]).await;
        let (mut agent, root) = mock_agent(&mock.url);
        assert_eq!(
            agent.run_turn("work", event::null_events()).await.unwrap(),
            "finished"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn unlimited_tool_budget_continues_past_old_default() {
        use crate::test_support::{MockLlm, stream};
        let mut replies = Vec::new();
        for round in 0..12 {
            replies.push(stream(serde_json::json!({"tool_calls": [{"index": 0, "id": format!("c{round}"), "function": {
                "name": "remember",
                "arguments": serde_json::json!({"text": format!("fact {round}")}).to_string()
            }}]})));
        }
        replies.push(stream(serde_json::json!({"content": "finished"})));
        let mock = MockLlm::start(replies).await;
        let (mut agent, root) = mock_agent(&mock.url);
        Arc::get_mut(&mut agent.shared).unwrap().max_tool_rounds =
            Config::default().agent.max_tool_rounds;
        assert_eq!(
            agent.run_turn("work", event::null_events()).await.unwrap(),
            "finished"
        );
        let memory = agent.agent_memory.content().unwrap();
        for round in 0..12 {
            assert!(memory.contains(&format!("fact {round}")));
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn zero_tool_budget_still_allows_text_but_no_side_effects() {
        use crate::test_support::{MockLlm, stream};
        let mock = MockLlm::start(vec![
            stream(serde_json::json!({"content": "hello"})),
            stream(serde_json::json!({"tool_calls": [{"index": 0, "id": "c", "function": {"name": "remember", "arguments": "{\"text\":\"must not write\"}"}}]})),
        ]).await;
        let (mut agent, root) = mock_agent(&mock.url);
        Arc::get_mut(&mut agent.shared).unwrap().max_tool_rounds = Some(0);
        assert_eq!(
            agent.run_turn("hello", event::null_events()).await.unwrap(),
            "hello"
        );
        assert!(matches!(
            agent.run_turn("work", event::null_events()).await,
            Err(AgentError::ToolRoundLimit(0))
        ));
        assert!(!root.join("memory.md").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn incomplete_or_errored_stream_never_executes_pending_tools() {
        use crate::test_support::MockLlm;
        let delta = serde_json::json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "c", "function": {"name": "remember", "arguments": "{\"text\":\"must not write\"}"}}]}}]});
        for tail in [
            "",
            "data: {\"error\":{\"message\":\"failed\"}}\n\ndata: [DONE]\n\n",
        ] {
            let mock = MockLlm::start(vec![(200, format!("data: {delta}\n\n{tail}"))]).await;
            let (mut agent, root) = mock_agent(&mock.url);
            assert!(agent.run_turn("work", event::null_events()).await.is_err());
            assert!(!root.join("memory.md").exists());
            assert!(agent.history.is_empty());
        }
    }

    #[test]
    fn completed_and_interrupted_turns_are_not_replayed_after_write_failure() {
        let root = std::env::temp_dir().join(format!("kb-failed-save-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let blocked = root.join("not-a-directory");
        std::fs::write(&blocked, "keep").unwrap();
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.extensions.memory = false;
        cfg.agent.memory_file = root.join("memory.md");
        cfg.agent.session_dir = blocked;
        let mut agent = Agent::new(&cfg).unwrap();
        agent.pending_input = Some("question".into());
        assert!(agent.record_turn("question", "completed answer").is_err());
        agent.record_interruption("completed answer").unwrap();
        assert_eq!(agent.history.len(), 2);
        assert_eq!(
            agent.history[1].content.as_deref(),
            Some("completed answer")
        );
        assert!(agent.pending_input.is_none());
        agent.pending_input = Some("next question".into());
        assert!(agent.record_interruption("partial answer").is_err());
        agent.record_interruption("partial answer").unwrap();
        assert_eq!(agent.history.len(), 4);
        assert!(agent.pending_input.is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn session_id_is_filesystem_safe() {
        let store = transcripts::TranscriptStore::new(PathBuf::new());
        let id = store.id();
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
        assert_eq!(id.len(), 15 + 1 + 6);
    }
}
