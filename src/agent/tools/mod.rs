pub mod bash;
pub mod catalog;
pub mod remember;
pub mod skill;
pub mod task;
pub mod todo;

use super::agentmem::AgentMemory;
use super::background::BackgroundManager;
use super::event::EventSender;
use super::plan::TodoList;
use super::skills::Skills;
use super::{AgentError, SharedState};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

#[derive(Debug)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
    /// Unabridged display text when the model receives a bounded excerpt.
    pub display_content: Option<String>,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            display_content: None,
        }
    }

    pub fn shell_output(output: String, success: bool) -> Self {
        const MODEL_LIMIT: usize = 8000;
        let mut end = output.len().min(MODEL_LIMIT);
        while !output.is_char_boundary(end) {
            end -= 1;
        }
        let content = if end < output.len() {
            format!(
                "{}\n…(truncated; full output available in transcript)",
                &output[..end]
            )
        } else {
            output.clone()
        };
        Self {
            content,
            is_error: !success,
            display_content: Some(output),
        }
    }

    pub fn err(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            display_content: None,
        }
    }
}

/// Everything a tool may touch. Borrows are scoped to one ReAct run.
pub struct ToolContext<'a> {
    pub todos: &'a mut TodoList,
    pub agent_memory: &'a AgentMemory,
    pub background: BackgroundManager,
    pub skills: &'a Arc<Skills>,
    pub events: &'a EventSender,
    pub shared: &'a Arc<SharedState>,
    pub depth: usize,
    pub plan_mode: bool,
}

/// The pieces of a `ToolContext` a sub-agent inherits, cloned so they can cross
/// a `tokio::spawn` boundary. Capturing the seed and building the sub-agent
/// context is the single point where `depth` increments and `plan_mode`
/// propagates — callers never fill those fields by hand.
pub struct SubagentSeed {
    background: BackgroundManager,
    skills: Arc<Skills>,
    shared: Arc<SharedState>,
    depth: usize,
    plan_mode: bool,
}

impl ToolContext<'_> {
    pub fn subagent_seed(&self) -> SubagentSeed {
        SubagentSeed {
            background: self.background.clone(),
            skills: Arc::clone(self.skills),
            shared: Arc::clone(self.shared),
            depth: self.depth + 1,
            plan_mode: self.plan_mode,
        }
    }
}

impl SubagentSeed {
    /// `todos`, `agent_memory` and `events` are supplied fresh per sub-agent;
    /// everything else comes from the parent captured in the seed.
    pub fn build<'a>(
        &'a self,
        todos: &'a mut TodoList,
        agent_memory: &'a AgentMemory,
        events: &'a EventSender,
    ) -> ToolContext<'a> {
        ToolContext {
            todos,
            agent_memory,
            background: self.background.clone(),
            skills: &self.skills,
            events,
            shared: &self.shared,
            depth: self.depth,
            plan_mode: self.plan_mode,
        }
    }
}

pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &str;
    fn schema(&self) -> serde_json::Value;
    fn execute<'a>(
        &'a self,
        ctx: &'a mut ToolContext,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>>;
}

pub use catalog::ToolCatalog;

/// One-line argument digest for the UI: `bash(ls -la)`, `skill(review)`...
pub fn summarize_args(name: &str, arguments: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(arguments).unwrap_or_default();
    let pick = |key: &str| {
        args.get(key)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    let summary = match name {
        "bash" => pick("command"),
        "remember" => pick("text"),
        "skill" => pick("name"),
        "task" => pick("description"),
        "todo_write" => args
            .get("todos")
            .and_then(|v| v.as_array())
            .map(|a| format!("{} items", a.len()))
            .unwrap_or_default(),
        _ => String::new(),
    };
    const MAX: usize = 60;
    if summary.chars().count() > MAX {
        let truncated: String = summary.chars().take(MAX).collect();
        format!("{truncated}…")
    } else {
        summary
    }
}

/// First/last lines of a tool result for the UI, folded past 5 lines.
pub fn result_preview(content: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    if lines.len() <= 5 {
        return content.to_string();
    }
    let mut out: Vec<&str> = lines[..2].to_vec();
    out.push("  …");
    out.extend_from_slice(&lines[lines.len() - 2..]);
    out.join("\n")
}

impl From<std::io::Error> for AgentError {
    fn from(source: std::io::Error) -> Self {
        AgentError::Io {
            path: String::new(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::permissions::Permissions;
    use super::*;
    use crate::config::{HooksConfig, PermissionsConfig};
    use crate::llm::LlmClient;

    fn shared_state() -> Arc<SharedState> {
        Arc::new(SharedState {
            llm: LlmClient::new(
                "http://localhost",
                "key",
                "model",
                &std::collections::HashMap::new(),
            ),
            permissions: Permissions::new(&PermissionsConfig::default()),
            hooks: HooksConfig::default(),
            extensions: crate::extensions::Extensions::default(),
            max_tool_rounds: Some(4),
            max_retries: 0,
            compact_threshold: 1000,
            subagent_max_rounds: Some(2),
            memory_file: std::path::PathBuf::from("/tmp/kb-agent-ctx-test.md"),
            lang: crate::i18n::LangCell::new(crate::i18n::Lang::En),
        })
    }

    /// Regression: the background `task` branch used to hardcode
    /// `plan_mode: false`, letting sub-agents escape plan mode.
    #[test]
    fn subagent_ctx_inherits_plan_mode_and_increments_depth() {
        let mut todos = TodoList::default();
        let mem = AgentMemory::load(std::env::temp_dir().join("kb-agent-ctx-test.md"));
        let background = BackgroundManager::default();
        let skills = Arc::new(Skills::default());
        let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
        let shared = shared_state();
        let root = ToolContext {
            todos: &mut todos,
            agent_memory: &mem,
            background,
            skills: &skills,
            events: &events,
            shared: &shared,
            depth: 0,
            plan_mode: true,
        };
        let seed = root.subagent_seed();
        let mut sub_todos = TodoList::default();
        let sub = seed.build(&mut sub_todos, &mem, &events);
        assert_eq!(sub.depth, 1);
        assert!(sub.plan_mode);
    }
    #[test]
    fn shell_output_keeps_full_display_text_and_limits_model_content() {
        let text = format!("{}END", "中文".repeat(5000));
        let result = super::ToolResult::shell_output(text.clone(), false);
        assert!(result.is_error);
        assert_eq!(result.display_content.as_deref(), Some(text.as_str()));
        assert!(result.content.len() < 8200);
        assert!(result.content.contains("truncated"));
        assert!(!result.content.ends_with("END"));
    }

    #[tokio::test]
    async fn invalid_tool_arguments_leave_state_unchanged() {
        let mut todos = TodoList::default();
        let mem = AgentMemory::load(
            std::env::temp_dir().join(format!("kb-args-{}", uuid::Uuid::new_v4())),
        );
        let skills = Arc::new(Skills::default());
        let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
        let shared = shared_state();
        let mut ctx = ToolContext {
            todos: &mut todos,
            agent_memory: &mem,
            background: BackgroundManager::default(),
            skills: &skills,
            events: &events,
            shared: &shared,
            depth: 0,
            plan_mode: false,
        };
        let registry = ToolCatalog::build(0, &shared.extensions);
        let valid =
            serde_json::json!({"todos": [{"content": "keep this", "status": "in_progress"}]});
        assert!(
            !registry
                .execute(&mut ctx, "todo_write", valid)
                .await
                .is_error
        );
        for (name, args) in [
            (
                "todo_write",
                serde_json::json!({"todos": [{"content": "bad", "status": "typo"}]}),
            ),
            (
                "todo_write",
                serde_json::json!({"todos": [{"content": "missing status"}]}),
            ),
            (
                "bash",
                serde_json::json!({"command": "must-not-execute", "background": "true"}),
            ),
            (
                "bash",
                serde_json::json!({"command": "must-not-execute", "timeout": -1}),
            ),
            ("task", serde_json::json!({"prompt": "missing description"})),
            ("remember", serde_json::json!({"text": 123})),
            ("skill", serde_json::json!({"name": 123})),
        ] {
            let result = registry.execute(&mut ctx, name, args).await;
            assert!(result.is_error, "{name}: {}", result.content);
        }
        assert_eq!(ctx.todos.render_prompt(), "1. [~] keep this");
        assert_eq!(mem.content().unwrap(), "");
    }
}
