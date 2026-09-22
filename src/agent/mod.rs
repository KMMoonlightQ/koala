pub mod agentmem;
pub mod background;
mod btw;
pub mod compact;
pub mod event;
pub mod extension_ui;
mod file_io;
pub mod graph;
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
pub mod work;

use crate::config::{Config, HooksConfig};
use crate::i18n::{self, Key, Lang, LangCell};
use crate::llm::{ImageAttachment, LlmClient, LlmError, Message};
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
    #[error(
        "request context exceeds configured budget ({used} >= {limit} estimated tokens including response reserve); reduce input, images, extension context, or tools, or check model context_window or agent.compact_threshold (percentage)"
    )]
    ContextBudget { used: usize, limit: usize },
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
    #[error("invalid input: {0}")]
    InvalidInput(String),
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
    pending_images: Vec<ImageAttachment>,
}

impl Agent {
    pub async fn new(cfg: &Config) -> Result<Self, AgentError> {
        if cfg.llm.model.is_empty() {
            return Err(AgentError::MissingModel);
        }
        let mut models = cfg
            .llm
            .selectable_models()
            .map_err(AgentError::InvalidModelSettings)?;
        if !(1..=100).contains(&cfg.agent.compact_threshold) {
            return Err(AgentError::InvalidModelSettings(
                "agent.compact_threshold must be a percentage from 1 to 100 (default 75); legacy byte budgets are no longer supported".into(),
            ));
        }
        let catalog = crate::model_catalog::catalog().await;
        for model in &mut models {
            model.context_window = catalog
                .context_window(&cfg.llm.base_url, &model.model)
                .and_then(std::num::NonZeroU64::new)
                .or(model.context_window);
        }
        let selected = models.iter().find(|m| m.model == cfg.llm.model).unwrap();
        let efforts = &selected.reasoning_efforts;
        let llm = LlmClient::new(
            &cfg.llm.base_url,
            &cfg.llm.api_key,
            &cfg.llm.model,
            &cfg.llm.headers,
        );
        llm.set_context_window(selected.context_window.map(|v| v.get()));
        llm.set_reasoning_effort(
            selected
                .reasoning_effort
                .clone()
                .or_else(|| efforts.first().cloned()),
        );
        let mut extensions = crate::extensions::load(
            &cfg.extensions,
            tools::catalog::builtin_names().map(str::to_owned),
        )
        .map_err(AgentError::Extension)?;
        crate::mcp::load(&cfg.mcp, &mut extensions)
            .await
            .map_err(AgentError::Extension)?;
        let transcript = transcripts::TranscriptStore::new(
            std::env::current_dir()?.join(&cfg.agent.session_dir),
        );
        let background = BackgroundManager::default().for_session(
            work::Journal::new(transcript.path().with_extension("work")),
            Vec::new(),
        );
        Ok(Self {
            reasoning_efforts: efforts.clone(),
            context_window: selected.context_window.map(|v| v.get()),
            models,
            shared: Arc::new(SharedState {
                llm,
                permissions: Permissions::new(&cfg.permissions),
                hooks: cfg.hooks.clone(),
                extensions,
                max_tool_rounds: cfg.agent.max_tool_rounds,
                max_retries: cfg.agent.max_retries,
                compact_threshold: cfg.agent.compact_threshold,
                lang: LangCell::new(cfg.lang),
                subagent_max_rounds: cfg.agent.subagent_max_rounds,
                memory_file: cfg.agent.memory_file.clone(),
            }),
            agent_memory: AgentMemory::new(
                cfg.agent.memory_file.clone(),
                &std::env::current_dir()?,
                transcript.path().display().to_string(),
                cfg.agent.memory_read,
                cfg.agent.memory_write,
                cfg.agent.memory_index_bytes,
            )?,
            skills: Arc::new(Skills::load()),
            todos: TodoList::default(),
            background,
            transcript,
            history: Vec::new(),
            plan_mode: false,
            pending_input: None,
            pending_images: Vec::new(),
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
        self.shared.llm.set_context_window(self.context_window);
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
        let records = transcripts::read(self.transcript.path().parent().unwrap(), id, self.lang())?;
        let journal =
            work::Journal::new(self.transcript.path().with_file_name(format!("{id}.work")));
        let saved = journal.load()?;
        let plan_mode = saved.as_ref().and_then(|s| s.plan_mode).unwrap_or(false);
        let permission_mode = saved
            .as_ref()
            .and_then(|s| s.permission_mode)
            .unwrap_or(crate::config::PermissionMode::Normal);
        self.transcript.restore(id, self.lang())?;
        self.agent_memory
            .set_source(self.transcript.path().display().to_string());
        self.history = records
            .iter()
            .map(|r| {
                if r.role == "user" {
                    Message::user_with_images(&r.content, r.images.clone())
                } else {
                    Message::assistant(&r.content)
                }
            })
            .collect();
        self.todos = TodoList::default();
        let tasks = if let Some(saved) = saved {
            // A settings-only journal must not replace a legacy transcript's context.
            if saved.has_conversation {
                self.history = saved.messages;
            }
            self.todos.replace(saved.todos);
            saved.tasks
        } else {
            Vec::new()
        };
        self.background = self.background.for_session(journal, tasks);
        self.pending_input = None;
        self.pending_images.clear();
        self.plan_mode = plan_mode;
        self.shared.permissions.set_mode(permission_mode);
        Ok(records)
    }

    /// Move within a session without deleting or copying its alternate paths.
    pub fn branch_before_turn(&mut self, id: &str) -> Result<String, String> {
        self.navigate_turn(id, false)
    }

    pub fn navigate_turn(&mut self, id: &str, after: bool) -> Result<String, String> {
        let journal = self
            .background
            .journal
            .as_ref()
            .ok_or("No session journal")?;
        let graph = journal.graph_snapshot();
        let node = graph
            .get(id)
            .filter(|n| n.kind == graph::Kind::Turn)
            .ok_or("Select a conversation turn")?;
        let draft = if after {
            String::new()
        } else {
            node.data["input"]
                .as_str()
                .ok_or("Turn has no saved input")?
                .to_owned()
        };
        let state = journal
            .load_at(Some(id), after)?
            .ok_or("No saved context")?;
        let current = if after {
            Some(id.to_owned())
        } else {
            graph.turn_parent(id)
        };
        journal
            .navigate(current, &state)
            .map_err(|e| e.to_string())?;
        self.history = state.messages;
        self.todos.replace(state.todos);
        self.pending_input = None;
        self.pending_images.clear();
        Ok(draft)
    }

    pub fn new_session(&mut self) {
        self.transcript.reset();
        self.agent_memory
            .set_source(self.transcript.path().display().to_string());
        self.background = self.background.for_session(
            work::Journal::new(self.transcript.path().with_extension("work")),
            Vec::new(),
        );
        self.history.clear();
        self.todos = TodoList::default();
        self.pending_input = None;
        self.pending_images.clear();
    }

    pub fn toggle_plan_mode(&mut self) -> std::io::Result<bool> {
        let next = !self.plan_mode;
        if let Some(journal) = &self.background.journal {
            journal.plan_mode(next)?;
        }
        self.plan_mode = next;
        Ok(next)
    }

    pub fn plan_mode(&self) -> bool {
        self.plan_mode
    }

    pub fn skills(&self) -> &Skills {
        &self.skills
    }

    fn initialize_work_trace(&self) -> std::io::Result<()> {
        let Some(journal) = &self.background.journal else {
            return Ok(());
        };
        if journal.has_trace()
            || journal
                .load()
                .map_err(std::io::Error::other)?
                .is_some_and(|saved| saved.navigated || !saved.trace.is_empty())
        {
            return Ok(());
        }
        // The display uses original conversation records, even if the saved model
        // context has already been compacted by an earlier version.
        let messages = if self.transcript.path().is_file() {
            transcripts::read(
                self.transcript.path().parent().unwrap(),
                self.transcript.id(),
                self.lang(),
            )
            .map_err(std::io::Error::other)?
            .into_iter()
            .map(|record| {
                if record.role == "user" {
                    Message::user_with_images(record.content, record.images)
                } else {
                    Message::assistant(record.content)
                }
            })
            .collect()
        } else {
            self.history.clone()
        };
        for message in messages {
            let content = message.display_content();
            journal.trace(if message.role == "user" {
                work::Trace::User(content)
            } else {
                work::Trace::Text(content)
            })?;
        }
        Ok(())
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
        self.initialize_work_trace()?;
        if let Some(journal) = &self.background.journal {
            journal.initialize_permission_mode(self.shared.permissions.mode())?;
            journal.plan_mode(self.plan_mode)?;
        }
        let mut span = self
            .background
            .journal
            .clone()
            .map(graph::Recorder::new)
            .map(|r| {
                r.start(
                    graph::Kind::Compaction,
                    "Manual compaction",
                    serde_json::json!({"before": self.history}),
                    vec![],
                )
            })
            .transpose()?;
        let outcome = compact::compact_keeping(
            &self.shared.llm,
            &mut self.history,
            compact::KEEP_RECENT,
            self.shared
                .llm
                .context_budget(self.shared.compact_threshold),
        )
        .await;
        if let Some(span) = &mut span {
            span.finish(if outcome.is_ok() { graph::Status::Succeeded } else { graph::Status::Failed },
                serde_json::json!({"after": self.history, "changed": outcome.as_ref().ok(), "error": outcome.as_ref().err().map(ToString::to_string)}))?;
        }
        let changed = outcome?;
        self.shared.extensions.hook(crate::extensions::Stage::AfterCompact, serde_json::json!({"session": self.transcript.id(), "messages": self.history, "changed": changed})).await.map_err(AgentError::Extension)?;
        if let Some(journal) = &self.background.journal {
            journal.context(&self.history, &self.todos.items)?;
        }
        Ok(changed)
    }

    pub async fn run_turn(
        &mut self,
        input: &str,
        events: EventSender,
    ) -> Result<String, AgentError> {
        self.run_turn_with_images(input, Vec::new(), events).await
    }

    pub async fn run_turn_with_images(
        &mut self,
        input: &str,
        images: Vec<ImageAttachment>,
        events: EventSender,
    ) -> Result<String, AgentError> {
        crate::images::validate(&images).map_err(AgentError::InvalidInput)?;
        self.initialize_work_trace()?;
        if let Some(journal) = &self.background.journal {
            journal.initialize_permission_mode(self.shared.permissions.mode())?;
            journal.plan_mode(self.plan_mode)?;
        }
        let mut span = self
            .background
            .journal
            .clone()
            .map(graph::Recorder::new)
            .map(|r| {
                r.start(
                    graph::Kind::Turn,
                    input.lines().next().unwrap_or("Turn"),
                    serde_json::json!({"input": input, "images": images, "conversation_parent": self.background.journal.as_ref().and_then(|j| j.graph_snapshot().current_turn)}),
                    vec![],
                )
            })
            .transpose()?;
        let recorder = span.as_ref().map(graph::Span::recorder);
        let result = self
            .run_turn_recorded(input, &images, events, recorder)
            .await;
        if let Some(span) = &mut span {
            span.finish(
                if result.is_ok() {
                    graph::Status::Succeeded
                } else {
                    graph::Status::Failed
                },
                match &result {
                    Ok(text) => serde_json::json!({"output": text}),
                    Err(e) => serde_json::json!({"error": e.to_string()}),
                },
            )?;
        }
        result
    }

    async fn run_turn_recorded(
        &mut self,
        input: &str,
        images: &[ImageAttachment],
        events: EventSender,
        graph: Option<graph::Recorder>,
    ) -> Result<String, AgentError> {
        self.initialize_work_trace()?;
        if let Some(journal) = &self.background.journal {
            journal.trace(work::Trace::User(
                Message::user_with_images(input, images.to_vec()).display_content(),
            ))?;
            let mut messages = self.history.clone();
            messages.push(Message::user_with_images(input, images.to_vec()));
            journal.context(&messages, &self.todos.items)?;
        }
        self.pending_input = Some(input.to_owned());
        self.pending_images = images.to_vec();
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
        let mut messages = Vec::with_capacity(self.history.len() + 2);
        messages.extend(self.history.iter().cloned());
        messages.push(Message::user_with_images(input, images.to_vec()));

        let reply = {
            let mut ctx = ToolContext {
                graph,
                todos: &mut self.todos,
                agent_memory: &self.agent_memory,
                background: self.background.clone(),
                skills: &self.skills,
                events: &events,
                shared: &self.shared,
                depth: 0,
                plan_mode: self.plan_mode,
            };
            match react::run(
                &mut ctx,
                &mut messages,
                self.shared.max_tool_rounds,
                extension.context.as_deref(),
            )
            .await
            {
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

        self.history = messages
            .into_iter()
            .filter(|m| m.role != "system")
            .collect();
        self.pending_input = None;
        self.pending_images.clear();
        self.transcript
            .append_turn_with_images(input, images, &reply)?;

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
            let images = std::mem::take(&mut self.pending_images);
            let reply = format!(
                "{}\n{}",
                progress,
                i18n::text(self.lang(), Key::InterruptNotice)
            );
            if let Some(journal) = &self.background.journal {
                let saved = journal.load();
                if let Ok(Some(saved)) = &saved {
                    self.history = saved.messages.clone();
                } else {
                    self.history
                        .push(Message::user_with_images(&input, images.clone()));
                }
                self.history.push(Message::assistant(&reply));
                saved.map_err(std::io::Error::other)?;
                journal.trace(work::Trace::Note(
                    i18n::text(self.lang(), Key::InterruptNotice).into(),
                ))?;
                journal.context(&self.history, &self.todos.items)?;
                self.transcript
                    .append_turn_with_images(&input, &images, &reply)?;
            } else {
                self.record_turn(&input, &images, &reply)?;
            }
        }
        Ok(())
    }

    fn record_turn(
        &mut self,
        input: &str,
        images: &[ImageAttachment],
        reply: &str,
    ) -> Result<(), AgentError> {
        // A completed reply stays in memory even if persistence fails. Clear
        // pending first so session completion cannot save it again as interrupted.
        self.history
            .push(Message::user_with_images(input, images.to_vec()));
        self.history.push(Message::assistant(reply));
        self.pending_input = None;
        self.pending_images.clear();
        self.transcript
            .append_turn_with_images(input, images, reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn image_turn_survives_tools_restart_and_tree_navigation() {
        use crate::test_support::{MockLlm, stream};
        use serde_json::json;
        let mut mock = MockLlm::start(vec![
            stream(json!({"tool_calls":[{"index":0,"id":"img-tool","function":{"name":"read","arguments":"{\"path\":\"missing-image-test-file\"}"}}]})),
            stream(json!({"content":"a red pixel"})),
            stream(json!({"content":"still red"})),
        ]).await;
        let (mut agent, root) = mock_agent(&mock.url).await;
        let image = crate::images::from_rgba(1, 1, &[255, 0, 0, 255]).unwrap();
        agent
            .run_turn_with_images("", vec![image.clone()], event::null_events())
            .await
            .unwrap();
        for _ in 0..2 {
            let request = mock.request().await;
            assert!(
                request["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|m| m["content"][0]["image_url"]["url"] == image.data_url)
            );
        }
        let id = agent.transcript.id().to_owned();
        let records =
            transcripts::read(agent.transcript.path().parent().unwrap(), &id, agent.lang())
                .unwrap();
        assert_eq!(records[0].images, vec![image.clone()]);
        let turn = agent
            .background
            .journal
            .as_ref()
            .unwrap()
            .graph_snapshot()
            .nodes
            .iter()
            .find(|n| n.kind == graph::Kind::Turn)
            .unwrap()
            .id
            .clone();
        agent.new_session();
        agent.restore_session(&id).unwrap();
        assert_eq!(agent.history[0].images, vec![image.clone()]);
        agent.navigate_turn(&turn, false).unwrap();
        assert!(agent.history.is_empty());
        agent.navigate_turn(&turn, true).unwrap();
        assert_eq!(agent.history[0].images, vec![image.clone()]);
        agent
            .run_turn("what color was it?", event::null_events())
            .await
            .unwrap();
        let request = mock.request().await;
        assert!(
            request["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["content"][0]["image_url"]["url"] == image.data_url)
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn image_failure_is_preserved_for_retry_and_invalid_input_is_not_recorded() {
        use crate::test_support::MockLlm;
        let mock = MockLlm::start(vec![(400, "model does not support images".into())]).await;
        let (mut agent, root) = mock_agent(&mock.url).await;
        let image = crate::images::from_rgba(1, 1, &[0, 0, 0, 255]).unwrap();
        let error = agent
            .run_turn_with_images("explain", vec![image.clone()], event::null_events())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("does not support images"));
        agent.record_interruption("").unwrap();
        let id = agent.transcript.id().to_owned();
        agent.restore_session(&id).unwrap();
        assert_eq!(agent.history[0].images, vec![image.clone()]);
        let history = serde_json::to_value(&agent.history).unwrap();
        assert!(
            agent
                .run_turn_with_images("too many", vec![image; 5], event::null_events())
                .await
                .is_err()
        );
        assert_eq!(serde_json::to_value(&agent.history).unwrap(), history);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn restored_image_transcript_reaches_provider() {
        use crate::test_support::{MockLlm, stream};
        let mut mock = MockLlm::start(vec![stream(serde_json::json!({"content":"yes"}))]).await;
        let (mut agent, root) = mock_agent(&mock.url).await;
        let dir = agent.transcript.path().parent().unwrap().to_owned();
        std::fs::create_dir_all(&dir).unwrap();
        let record = serde_json::json!({"ts":"now", "role":"user", "content":"look", "images":[{"data_url":"data:image/png;base64,AQID", "width":1, "height":1}]});
        std::fs::write(dir.join("image-test.jsonl"), format!("{record}\n")).unwrap();
        agent.restore_session("image-test").unwrap();
        agent
            .run_turn("describe", event::null_events())
            .await
            .unwrap();
        let request = mock.request().await;
        assert!(
            request["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|m| m["content"][1]["image_url"]["url"] == "data:image/png;base64,AQID")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn graph_records_nested_runs_and_consumed_tool_results_after_restart() {
        use crate::test_support::{MockLlm, stream};
        use serde_json::json;
        let mut mock = MockLlm::start(vec![
            stream(json!({"tool_calls":[{"index":0,"id":"reused","function":{"name":"task","arguments":json!({"description":"child","prompt":"inspect"}).to_string()}}]})),
            stream(json!({"tool_calls":[{"index":0,"id":"reused","function":{"name":"bash","arguments":json!({"command":"printf graph-child"}).to_string()}}]})),
            stream(json!({"content":"child finished"})),
            stream(json!({"content":"parent finished"})),
        ]).await;
        let (mut agent, root) = mock_agent(&mock.url).await;
        agent
            .shared
            .permissions
            .set_mode(crate::config::PermissionMode::NeverAsk);
        agent
            .run_turn("nested task", event::null_events())
            .await
            .unwrap();
        let journal = agent.background.journal.as_ref().unwrap();
        let graph = work::Journal::new(journal.path.clone())
            .load()
            .unwrap()
            .unwrap()
            .graph;
        assert_eq!(graph.nodes.len(), 8);
        assert!(
            graph
                .nodes
                .iter()
                .all(|n| n.status == graph::Status::Succeeded)
        );
        let task = graph
            .nodes
            .iter()
            .find(|n| n.kind == graph::Kind::Tool && n.label == "task")
            .unwrap();
        let child = graph
            .nodes
            .iter()
            .find(|n| n.kind == graph::Kind::Subagent)
            .unwrap();
        let bash = graph
            .nodes
            .iter()
            .find(|n| n.kind == graph::Kind::Tool && n.label == "bash")
            .unwrap();
        assert_eq!(child.parent_id.as_ref(), Some(&task.id));
        assert_eq!(bash.run_id, child.id);
        assert_ne!(
            bash.id, task.id,
            "provider call ids must not be graph identities"
        );
        assert_eq!(bash.data["result"]["model_output"], "graph-child");
        assert!(
            graph
                .nodes
                .iter()
                .any(|n| n.kind == graph::Kind::Model && n.inputs.contains(&bash.id))
        );
        assert!(
            graph
                .nodes
                .iter()
                .any(|n| n.kind == graph::Kind::Model && n.inputs.contains(&task.id))
        );
        let request = mock.request().await;
        let first_model = graph
            .nodes
            .iter()
            .find(|n| n.kind == graph::Kind::Model)
            .unwrap();
        assert_eq!(first_model.data["messages"], request["messages"]);
        assert_eq!(first_model.data["tools"], request["tools"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    async fn mock_agent(url: &str) -> (Agent, PathBuf) {
        let root = std::env::temp_dir().join(format!("koala-review-{}", uuid::Uuid::new_v4()));
        let mut cfg = Config::default();
        cfg.permissions.mode = crate::config::PermissionMode::AskWhenNeed;
        cfg.llm.model = "test".into();
        cfg.llm.base_url = url.into();

        cfg.agent.memory_file = root.join("memory.md");
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.max_tool_rounds = Some(1);
        (Agent::new(&cfg).await.unwrap(), root)
    }

    #[tokio::test]
    async fn extension_context_is_budgeted_for_root_and_child() {
        use crate::extensions::{Extension, ExtensionFuture, Response, Stage};
        struct LargeContext;
        impl Extension for LargeContext {
            fn name(&self) -> &str {
                "large-context"
            }
            fn hook<'a>(&'a self, stage: Stage, _: &'a serde_json::Value) -> ExtensionFuture<'a> {
                Box::pin(async move {
                    Ok(Response {
                        context: (stage == Stage::BeforeModel).then(|| "x".repeat(50000)),
                        ..Default::default()
                    })
                })
            }
        }
        let mock = crate::test_support::MockLlm::start(vec![]).await;
        let (mut agent, root) = mock_agent(&mock.url).await;
        Arc::get_mut(&mut agent.shared)
            .unwrap()
            .extensions
            .register(Arc::new(LargeContext))
            .unwrap();
        assert!(matches!(
            agent.run_turn("hello", event::null_events()).await,
            Err(AgentError::ContextBudget { .. })
        ));
        let events = event::null_events();
        let mut ctx = ToolContext {
            graph: None,
            todos: &mut agent.todos,
            agent_memory: &agent.agent_memory,
            background: agent.background.clone(),
            skills: &agent.skills,
            events: &events,
            shared: &agent.shared,
            depth: 1,
            plan_mode: false,
        };
        assert!(matches!(
            subagent::run(&mut ctx, "hello").await,
            Err(AgentError::ContextBudget { .. })
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn new_session_preserves_curated_memory_and_updates_provenance_and_controls() {
        use crate::test_support::{MockLlm, stream};
        use agentmem::{Kind, Note, Scope};
        let mut mock = MockLlm::start(vec![stream(serde_json::json!({"content":"你好"})); 3]).await;
        let (mut agent, root) = mock_agent(&mock.url).await;
        let entry = |summary: &str| Note {
            key: "language".into(),
            kind: Kind::Preference,
            scope: Scope::Project,
            summary: summary.into(),
            details: "DETAIL_NOT_IN_INDEX".into(),
            expires_on: None,
        };
        let old = agent.agent_memory.upsert(entry("回答使用中文")).unwrap();
        let background_memory = agent.agent_memory.clone();
        agent.new_session();
        let corrected = agent
            .agent_memory
            .upsert(entry("偏好简洁的中文回答"))
            .unwrap();
        assert_ne!(old.source, corrected.source);
        assert!(corrected.source.contains(agent.session_id()));
        let background_entry = background_memory
            .upsert(Note {
                key: "background-source".into(),
                ..entry("后台任务继承原始来源")
            })
            .unwrap();
        assert_eq!(background_entry.source, old.source);
        agent
            .agent_memory
            .forget("background-source", Scope::Project)
            .unwrap();
        agent.run_turn("你好", event::null_events()).await.unwrap();
        let initial = mock.request().await;
        let system = initial["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("偏好简洁的中文回答"));
        assert!(!system.contains("DETAIL_NOT_IN_INDEX"));
        assert!(system.contains("Do not resume old work"));
        assert_eq!(
            agent.agent_memory.entries(false).unwrap().len(),
            1,
            "a greeting with no memory write must not append a record"
        );
        agent.agent_memory.set_read_enabled(false);
        agent.agent_memory.set_write_enabled(false);
        assert!(!background_memory.read_enabled());
        assert!(!background_memory.write_enabled());
        agent.run_turn("hello", event::null_events()).await.unwrap();
        let disabled = mock.request().await;
        assert!(
            !disabled["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("<agent_memory>")
        );
        for tool in disabled["tools"].as_array().unwrap() {
            assert_ne!(tool["function"]["name"], "recall");
            assert_ne!(tool["function"]["name"], "remember");
        }
        agent.agent_memory.set_read_enabled(true);
        agent.run_turn("hello", event::null_events()).await.unwrap();
        let read_only = mock.request().await;
        assert!(
            read_only["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["function"]["name"] == "recall")
        );
        assert!(
            !read_only["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["function"]["name"] == "remember")
        );
        assert_eq!(agent.agent_memory.entries(false).unwrap().len(), 1);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn first_turn_crash_can_be_listed_restored_and_corruption_preserves_live_state() {
        let (mut agent, root) = mock_agent("http://localhost:1").await;
        let id = agent.session_id().to_owned();
        let journal = agent.background.journal.as_ref().unwrap().clone();
        journal
            .trace(work::Trace::User("not finished".into()))
            .unwrap();
        journal
            .context(&[Message::user("not finished")], &[])
            .unwrap();
        agent.new_session();
        assert!(agent.list_sessions().unwrap().iter().any(|s| s.id == id));
        agent.restore_session(&id).unwrap();
        assert_eq!(agent.history[0].content.as_deref(), Some("not finished"));
        let bad = journal.path.with_file_name("broken.work");
        std::fs::write(bad, "bad record\n").unwrap();
        assert!(agent.restore_session("broken").is_err());
        assert_eq!(agent.session_id(), id);
        assert_eq!(agent.history[0].content.as_deref(), Some("not finished"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn failed_plan_save_preserves_the_active_mode() {
        let (mut agent, root) = mock_agent("http://localhost:1").await;
        let path = agent.background.journal.as_ref().unwrap().path.clone();
        std::fs::create_dir_all(&path).unwrap();
        assert!(agent.toggle_plan_mode().is_err());
        assert!(!agent.plan_mode());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn restore_keeps_plan_mode_across_sessions_and_restart() {
        let (mut agent, root) = mock_agent("http://localhost:1").await;
        let id = agent.session_id().to_owned();
        agent
            .background
            .journal
            .as_ref()
            .unwrap()
            .context(&[Message::user("investigate only")], &[])
            .unwrap();
        agent.toggle_plan_mode().unwrap();
        agent.new_session();
        agent.toggle_plan_mode().unwrap();
        agent.restore_session(&id).unwrap();
        assert!(
            agent.plan_mode(),
            "restore must not enable execution for a planning session"
        );
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.md");
        let mut restarted = Agent::new(&cfg).await.unwrap();
        restarted.restore_session(&id).unwrap();
        assert!(restarted.plan_mode());
        assert_eq!(
            restarted.history[0].content.as_deref(),
            Some("investigate only")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn restore_recovers_tool_protocol_and_working_todos() {
        use crate::test_support::{MockLlm, stream};
        let mut mock = MockLlm::start(vec![
            stream(serde_json::json!({"tool_calls": [{"index":0,"id":"todo-call","function":{"name":"todo_write","arguments":r#"{"todos":[{"content":"resume this work","status":"in_progress"}]}"#}}]})),
            stream(serde_json::json!({"content":"planned"})),
            stream(serde_json::json!({"content":"continued"})),
        ]).await;
        let (mut agent, root) = mock_agent(&mock.url).await;
        agent.run_turn("work", event::null_events()).await.unwrap();
        let id = agent.session_id().to_owned();
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.llm.base_url = mock.url.clone();
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.md");
        drop(agent);
        let mut agent = Agent::new(&cfg).await.unwrap();
        agent.restore_session(&id).unwrap();
        assert_eq!(agent.todos.items.len(), 1, "working todos were lost");
        assert!(
            agent
                .history
                .iter()
                .any(|m| m.tool_call_id.as_deref() == Some("todo-call")),
            "tool results were lost"
        );
        agent
            .run_turn("continue", event::null_events())
            .await
            .unwrap();
        mock.request().await;
        mock.request().await;
        let request = mock.request().await;
        assert!(
            request["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("resume this work")
        );
        assert!(
            request["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["role"] == "tool" && message["tool_call_id"] == "todo-call")
        );
        let saved = agent
            .background
            .journal
            .as_ref()
            .unwrap()
            .load()
            .unwrap()
            .unwrap();
        assert!(saved.trace.iter().any(|trace| matches!(trace, work::Trace::ToolStart { name, arguments, .. } if name == "todo_write" && arguments.contains("resume this work"))));
        assert!(saved.trace.iter().any(|trace| matches!(
            trace,
            work::Trace::ToolEnd {
                is_error: false,
                ..
            }
        )));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn prompt_and_tool_definitions_refresh_after_state_changes() {
        use crate::test_support::{MockLlm, stream};
        let mut mock = MockLlm::start(vec![
            stream(serde_json::json!({"tool_calls": [
                {"index":0,"id":"todo","function":{"name":"todo_write","arguments":serde_json::json!({"todos":[{"content":"LIVE_TODO_MARKER","status":"in_progress"}]}).to_string()}},
                {"index":1,"id":"memory","function":{"name":"remember","arguments":serde_json::json!({"action":"upsert","key":"live-marker","kind":"constraint","summary":"LIVE_MEMORY_MARKER"}).to_string()}}
            ]})),
            stream(serde_json::json!({"content":"done"})),
            stream(serde_json::json!({"content":"plan"})),
        ]).await;
        let (mut agent, root) = mock_agent(&mock.url).await;
        agent.run_turn("work", event::null_events()).await.unwrap();
        let initial = mock.request().await;
        let next = mock.request().await;
        let system = |request: &serde_json::Value| {
            request["messages"][0]["content"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        assert!(!system(&initial).contains("LIVE_TODO_MARKER"));
        assert!(!system(&initial).contains("LIVE_MEMORY_MARKER"));
        assert!(system(&next).contains("<todos>\n1. [~] LIVE_TODO_MARKER"));
        assert!(system(&next).contains("LIVE_MEMORY_MARKER"));
        for name in ["read", "bash", "edit", "write"] {
            assert!(system(&initial).contains(&format!("- {name}: ")));
            assert!(
                initial["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|tool| tool["function"]["name"] == name)
            );
        }
        agent.toggle_plan_mode().unwrap();
        agent
            .run_turn("plan only", event::null_events())
            .await
            .unwrap();
        let plan = mock.request().await;
        assert!(system(&plan).contains("<plan_mode>"));
        for name in ["bash", "edit", "write", "remember"] {
            assert!(!system(&plan).contains(&format!("- {name}: ")));
            assert!(
                !plan["tools"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|tool| tool["function"]["name"] == name)
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn background_index_reaches_model_and_stay_in_their_session() {
        use crate::test_support::{MockLlm, stream};
        let mut mock = MockLlm::start(vec![stream(serde_json::json!({"content": "ok"})); 3]).await;
        let (mut agent, root) = mock_agent(&mock.url).await;
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
        let request = mock.request().await["messages"].to_string();
        assert!(request.contains("research"));
        assert!(!request.contains("UNIQUE_BACKGROUND_RESULT"));
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
        let request = mock.request().await["messages"].to_string();
        assert!(request.contains("new task"));
        assert!(!request.contains("NEW_BACKGROUND_FAILURE"));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn tool_round_limit_reports_failure_without_executing_extra_round() {
        use crate::test_support::{MockLlm, stream};
        let call = |text: &str| serde_json::json!({"tool_calls": [{"index": 0, "id": "c", "function": {"name": "remember", "arguments": serde_json::json!({"action":"upsert","key": text.replace(' ', "-").to_lowercase(),"kind":"constraint","summary":text}).to_string()}}]});
        let mock = MockLlm::start(vec![
            stream(call("first allowed write")),
            stream(call("EXTRA_WRITE")),
        ])
        .await;
        let (mut agent, root) = mock_agent(&mock.url).await;
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
            stream(serde_json::json!({"tool_calls": [{"index": 0, "id": "c", "function": {"name": "remember", "arguments": serde_json::json!({"action":"upsert","key":"fact","kind":"constraint","summary":"fact"}).to_string()}}]})),
            stream(serde_json::json!({"content": "finished"})),
        ]).await;
        let (mut agent, root) = mock_agent(&mock.url).await;
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
                "arguments": serde_json::json!({"action":"upsert","key":format!("fact-{round}"),"kind":"constraint","summary": format!("fact {round}")}).to_string()
            }}]})));
        }
        replies.push(stream(serde_json::json!({"content": "finished"})));
        let mock = MockLlm::start(replies).await;
        let (mut agent, root) = mock_agent(&mock.url).await;
        Arc::get_mut(&mut agent.shared).unwrap().max_tool_rounds =
            Config::default().agent.max_tool_rounds;
        assert_eq!(
            agent.run_turn("work", event::null_events()).await.unwrap(),
            "finished"
        );
        let memory = serde_json::to_string(&agent.agent_memory.entries(false).unwrap()).unwrap();
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
        let (mut agent, root) = mock_agent(&mock.url).await;
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
            let (mut agent, root) = mock_agent(&mock.url).await;
            assert!(agent.run_turn("work", event::null_events()).await.is_err());
            assert!(!root.join("memory.md").exists());
            assert!(agent.history.is_empty());
        }
    }

    #[tokio::test]
    async fn completed_and_interrupted_turns_are_not_replayed_after_write_failure() {
        let root = std::env::temp_dir().join(format!("koala-failed-save-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let blocked = root.join("not-a-directory");
        std::fs::write(&blocked, "keep").unwrap();
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();

        cfg.agent.memory_file = root.join("memory.md");
        cfg.agent.session_dir = blocked;
        let mut agent = Agent::new(&cfg).await.unwrap();
        agent.pending_input = Some("question".into());
        assert!(
            agent
                .record_turn("question", &[], "completed answer")
                .is_err()
        );
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

    #[tokio::test]
    async fn session_tree_keeps_siblings_and_restores_selected_context() {
        use crate::test_support::{MockLlm, stream};
        let root = std::env::temp_dir().join(format!("koala-tree-{}", uuid::Uuid::new_v4()));
        let mut mock = MockLlm::start(
            ["answer A", "answer B", "answer C", "answer D"]
                .into_iter()
                .map(|text| stream(serde_json::json!({"content":text})))
                .collect(),
        )
        .await;
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.llm.base_url = mock.url.clone();
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.md");
        let mut agent = Agent::new(&cfg).await.unwrap();
        let session_id = agent.session_id().to_owned();
        agent
            .run_turn("question A", event::null_events())
            .await
            .unwrap();
        mock.request().await;
        let a = agent
            .background
            .journal
            .as_ref()
            .unwrap()
            .graph_snapshot()
            .current_turn
            .unwrap();
        agent
            .run_turn("question B", event::null_events())
            .await
            .unwrap();
        mock.request().await;
        let b = agent
            .background
            .journal
            .as_ref()
            .unwrap()
            .graph_snapshot()
            .current_turn
            .unwrap();
        agent.navigate_turn(&a, true).unwrap();
        agent
            .run_turn("question C", event::null_events())
            .await
            .unwrap();
        let request = mock.request().await;
        let messages = request["messages"].to_string();
        assert!(
            messages.contains("question A")
                && messages.contains("answer A")
                && messages.contains("question C")
        );
        assert!(!messages.contains("question B") && !messages.contains("answer B"));
        let graph = agent.background.journal.as_ref().unwrap().graph_snapshot();
        let c = graph.current_turn.clone().unwrap();
        assert_eq!(graph.turn_parent(&b).as_deref(), Some(a.as_str()));
        assert_eq!(graph.turn_parent(&c).as_deref(), Some(a.as_str()));
        assert_eq!(agent.session_id(), session_id);
        agent.navigate_turn(&b, true).unwrap();
        agent.new_session();
        agent.restore_session(&session_id).unwrap();
        assert_eq!(
            agent
                .background
                .journal
                .as_ref()
                .unwrap()
                .graph_snapshot()
                .current_turn
                .as_deref(),
            Some(b.as_str())
        );
        assert!(
            agent
                .history
                .iter()
                .any(|m| m.content.as_deref() == Some("answer B"))
        );
        assert!(
            !agent
                .history
                .iter()
                .any(|m| m.content.as_deref() == Some("answer C"))
        );
        // Re-asking the first turn must not seed history from the linear transcript.
        assert_eq!(agent.branch_before_turn(&a).unwrap(), "question A");
        agent
            .run_turn("question D", event::null_events())
            .await
            .unwrap();
        let request = mock.request().await;
        let messages = request["messages"].as_array().unwrap();
        let users: Vec<_> = messages.iter().filter(|m| m["role"] == "user").collect();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0]["content"], "question D");
        let graph = agent.background.journal.as_ref().unwrap().graph_snapshot();
        assert!(
            graph
                .turn_parent(graph.current_turn.as_deref().unwrap())
                .is_none()
        );
        assert_eq!(
            graph
                .nodes
                .iter()
                .filter(|n| n.kind == graph::Kind::Turn)
                .count(),
            4
        );
        assert_eq!(
            std::fs::read_dir(root.join("sessions"))
                .unwrap()
                .filter_map(Result::ok)
                .filter(|e| e.path().extension().is_some_and(|x| x == "work"))
                .count(),
            1
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn branching_keeps_only_prior_context_and_preserves_original() {
        let root = std::env::temp_dir().join(format!("koala-branch-{}", uuid::Uuid::new_v4()));
        let mut cfg = crate::config::Config::default();
        cfg.llm.model = "test".into();
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.md");
        let mut agent = Agent::new(&cfg).await.unwrap();
        let journal = agent.background.journal.clone().unwrap();
        journal
            .trace(work::Trace::User("prior question".into()))
            .unwrap();
        journal
            .trace(work::Trace::Text("prior answer".into()))
            .unwrap();
        journal
            .context(
                &[
                    Message::user("prior question"),
                    Message::assistant("prior answer"),
                ],
                &[],
            )
            .unwrap();
        let mut turn = graph::Recorder::new(journal.clone())
            .start(
                graph::Kind::Turn,
                "retry",
                serde_json::json!({"input":"retry\nthis"}),
                vec![],
            )
            .unwrap();
        journal
            .trace(work::Trace::User("retry\nthis".into()))
            .unwrap();
        journal
            .context(&[Message::user("future content must be excluded")], &[])
            .unwrap();
        turn.finish(graph::Status::Failed, serde_json::json!({}))
            .unwrap();
        let before = std::fs::read(&journal.path).unwrap();
        let original = agent.session_id().to_owned();
        assert!(agent.branch_before_turn("missing").is_err());
        assert_eq!(agent.session_id(), original);
        assert_eq!(
            agent.branch_before_turn(&turn.node.id).unwrap(),
            "retry\nthis"
        );
        assert_eq!(agent.session_id(), original);
        assert_eq!(agent.history.len(), 2);
        assert_eq!(agent.history[1].content.as_deref(), Some("prior answer"));
        assert!(std::fs::read(&journal.path).unwrap().starts_with(&before));
        assert!(journal.graph_snapshot().get(&turn.node.id).is_some());
        let branch = agent.session_id().to_owned();
        agent.new_session();
        agent.restore_session(&branch).unwrap();
        assert_eq!(agent.history.len(), 2);
        assert_eq!(agent.history[0].content.as_deref(), Some("prior question"));
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
