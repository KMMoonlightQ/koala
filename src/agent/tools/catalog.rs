//! One tool directory for definitions, capabilities and execution ownership.
use super::{Tool, ToolContext, ToolResult, bash, remember, skill, task, todo};
use crate::agent::permissions::{Permissions, Policy};
use crate::extensions::{Extension, Extensions};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Copy)]
enum Approval {
    Always,
    Shell,
    Ask,
}

struct Entry {
    definition: crate::llm::Tool,
    plan_allowed: bool,
    approval: Approval,
    target: Target,
}

enum Target {
    Builtin(&'static dyn Tool),
    Extension(Arc<dyn Extension>),
}

struct Builtin {
    tool: &'static dyn Tool,
    plan_allowed: bool,
    approval: Approval,
    root_only: bool,
}

// Names, capabilities and visibility come from the same lightweight declarations.
static BUILTINS: &[Builtin] = &[
    Builtin {
        tool: &remember::Remember,
        plan_allowed: false,
        approval: Approval::Always,
        root_only: false,
    },
    Builtin {
        tool: &todo::TodoWrite,
        plan_allowed: true,
        approval: Approval::Always,
        root_only: false,
    },
    Builtin {
        tool: &skill::SkillTool,
        plan_allowed: true,
        approval: Approval::Always,
        root_only: false,
    },
    Builtin {
        tool: &bash::Bash,
        plan_allowed: false,
        approval: Approval::Shell,
        root_only: false,
    },
    Builtin {
        tool: &task::TaskTool,
        plan_allowed: true,
        approval: Approval::Always,
        root_only: true,
    },
];

pub(crate) fn builtin_names() -> impl Iterator<Item = &'static str> {
    BUILTINS.iter().map(|builtin| builtin.tool.name())
}

pub struct ToolCatalog {
    entries: Vec<Entry>,
}

impl ToolCatalog {
    pub fn build(depth: usize, extensions: &Extensions) -> Self {
        let mut entries: Vec<_> = BUILTINS
            .iter()
            .filter(|builtin| depth == 0 || !builtin.root_only)
            .map(|builtin| Entry {
                definition: crate::llm::Tool::function(
                    builtin.tool.name(),
                    builtin.tool.description(),
                    builtin.tool.schema(),
                ),
                plan_allowed: builtin.plan_allowed,
                approval: builtin.approval,
                target: Target::Builtin(builtin.tool),
            })
            .collect();
        entries.extend(
            extensions
                .tool_entries()
                .into_iter()
                .map(|(definition, extension)| {
                    let read_only = extension.read_only(&definition.function.name);
                    Entry {
                        definition,
                        plan_allowed: read_only,
                        approval: if read_only {
                            Approval::Always
                        } else {
                            Approval::Ask
                        },
                        target: Target::Extension(extension),
                    }
                }),
        );
        Self { entries }
    }

    fn find(&self, name: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|entry| entry.definition.function.name == name)
    }

    pub fn definitions(&self) -> Vec<crate::llm::Tool> {
        self.entries
            .iter()
            .map(|entry| entry.definition.clone())
            .collect()
    }

    pub fn plan_allowed(&self, name: &str) -> bool {
        self.find(name).is_some_and(|entry| entry.plan_allowed)
    }

    /// Argument-rewriting hooks must run before this check.
    pub fn policy(&self, permissions: &Permissions, name: &str, args: &Value) -> Policy {
        let automatic = self.find(name).is_some_and(|entry| match entry.approval {
            Approval::Always => true,
            Approval::Ask => false,
            Approval::Shell => args
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(read_only_shell),
        });
        permissions.check(name, automatic)
    }

    pub async fn execute(&self, ctx: &mut ToolContext<'_>, name: &str, args: Value) -> ToolResult {
        let Some(entry) = self.find(name) else {
            return ToolResult::err(format!("unknown tool: {name}"));
        };
        match &entry.target {
            Target::Builtin(tool) => tool.execute(ctx, args).await,
            Target::Extension(extension) => match extension.execute(name, &args).await {
                Ok(response) => match response.content {
                    Some(content) => ToolResult {
                        content,
                        is_error: response.is_error,
                        display_content: None,
                    },
                    None => ToolResult::err("extension tool returned no content"),
                },
                Err(reason) => ToolResult::err(reason),
            },
        }
    }
}

/// Deliberately narrow: no shell operators, expansion, quoting, scripts or
/// extensible commands. Anything outside this subset goes through approval.
fn read_only_shell(command: &str) -> bool {
    if !command
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || " /._-".contains(c))
    {
        return false;
    }
    let mut words = command.split_whitespace();
    matches!(
        words.next(),
        Some("pwd" | "ls" | "cat" | "head" | "tail" | "wc")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{PermissionMode, PermissionsConfig};
    use serde_json::json;

    #[tokio::test]
    async fn memory_can_be_removed_and_retrieves_fresh_data() {
        let dir = std::env::temp_dir().join(format!("kb-catalog-memory-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cfg = crate::config::Config::default();
        cfg.memory.workspace = dir.join("memory");
        cfg.extensions.memory = false;
        let disabled = crate::extensions::load(&cfg).unwrap();
        assert!(disabled.tools().is_empty());
        disabled
            .hook(
                crate::extensions::Stage::TurnStart,
                json!({"input": "ownership"}),
            )
            .await
            .unwrap();
        assert!(!cfg.memory.workspace.exists());
        cfg.extensions.memory = true;
        cfg.llm.model = "test".into();
        cfg.agent.memory_file = dir.join("private-memory.md");
        let mut agent = crate::agent::Agent::new(&cfg).unwrap();
        let enabled = &agent.shared.extensions;
        let catalog = ToolCatalog::build(0, enabled);
        let events = crate::agent::event::null_events();
        let mut ctx = ToolContext {
            todos: &mut agent.todos,
            agent_memory: &agent.agent_memory,
            background: agent.background.clone(),
            skills: &agent.skills,
            events: &events,
            shared: &agent.shared,
            depth: 0,
            plan_mode: false,
        };
        let written = catalog.execute(&mut ctx, "memory_write", json!({"path": "digest/wiki/rust", "name": "rust", "content": "ownership and borrowing", "description": "Rust"})).await;
        assert!(!written.is_error);
        let retrieved = enabled
            .hook(
                crate::extensions::Stage::TurnStart,
                json!({"input": "ownership"}),
            )
            .await
            .unwrap();
        assert!(
            retrieved
                .context
                .unwrap()
                .contains("ownership and borrowing")
        );
        assert!(catalog.plan_allowed("memory_read"));
        assert!(!catalog.plan_allowed("memory_write"));
        let failure = catalog
            .execute(&mut ctx, "memory_read", json!({"path": "../secret"}))
            .await;
        assert!(failure.is_error);
        std::fs::remove_dir_all(dir).unwrap();
    }

    struct Echo;
    impl Extension for Echo {
        fn name(&self) -> &str {
            "catalog-test"
        }
        fn tools(&self) -> Vec<crate::llm::Tool> {
            vec![crate::llm::Tool::function(
                "echo",
                "echo input",
                json!({"type":"object"}),
            )]
        }
        fn read_only(&self, _: &str) -> bool {
            true
        }
        fn hook<'a>(
            &'a self,
            _: crate::extensions::Stage,
            _: &'a Value,
        ) -> crate::extensions::ExtensionFuture<'a> {
            Box::pin(async { Ok(crate::extensions::Response::default()) })
        }
        fn execute<'a>(
            &'a self,
            name: &'a str,
            args: &'a Value,
        ) -> crate::extensions::ExtensionFuture<'a> {
            Box::pin(async move {
                Ok(crate::extensions::Response {
                    content: Some(format!("{name}: {}", args["text"].as_str().unwrap())),
                    ..Default::default()
                })
            })
        }
    }

    #[tokio::test]
    async fn one_catalog_routes_both_tool_sources_and_hidden_tools_cannot_execute() {
        let mut cfg = crate::config::Config::default();
        cfg.llm.model = "test".into();
        cfg.extensions.memory = false;
        let mut agent = crate::agent::Agent::new(&cfg).unwrap();
        Arc::get_mut(&mut agent.shared)
            .unwrap()
            .extensions
            .register(Arc::new(Echo))
            .unwrap();
        let catalog = ToolCatalog::build(1, &agent.shared.extensions);
        let names: Vec<_> = catalog
            .definitions()
            .into_iter()
            .map(|tool| tool.function.name)
            .collect();
        assert!(names.iter().any(|name| name == "echo"));
        assert!(names.iter().any(|name| name == "todo_write"));
        assert!(!names.iter().any(|name| name == "task"));
        let events = crate::agent::event::null_events();
        let mut ctx = ToolContext {
            todos: &mut agent.todos,
            agent_memory: &agent.agent_memory,
            background: agent.background.clone(),
            skills: &agent.skills,
            events: &events,
            shared: &agent.shared,
            depth: 1,
            plan_mode: false,
        };
        let result = catalog
            .execute(&mut ctx, "echo", json!({"text":"hello"}))
            .await;
        assert!(!result.is_error);
        assert_eq!(result.content, "echo: hello");
        let result = catalog
            .execute(
                &mut ctx,
                "todo_write",
                json!({"todos":[{"content":"from catalog","status":"pending"}]}),
            )
            .await;
        assert!(!result.is_error);
        assert!(ctx.todos.render_prompt().contains("from catalog"));
        let result = catalog
            .execute(
                &mut ctx,
                "task",
                json!({"prompt":"must not start", "description":"hidden task"}),
            )
            .await;
        assert!(result.is_error);
        assert_eq!(result.content, "unknown tool: task");
    }

    #[test]
    fn capabilities_keep_approval_distinct_from_plan_mode() {
        let mut extensions = Extensions::default();
        extensions
            .register(Arc::new(crate::extensions::memory::MemoryExtension::new(
                "unused".into(),
            )))
            .unwrap();
        let catalog = ToolCatalog::build(0, &extensions);
        let permissions = Permissions::new(&PermissionsConfig {
            mode: PermissionMode::AskWhenNeed,
            ..Default::default()
        });
        for (name, args, policy, plan) in [
            ("remember", json!({}), Policy::Allow, false),
            ("todo_write", json!({}), Policy::Allow, true),
            ("skill", json!({}), Policy::Allow, true),
            ("task", json!({}), Policy::Allow, true),
            ("bash", json!({"command":"ls"}), Policy::Allow, false),
            (
                "bash",
                json!({"command":"ls; touch marker"}),
                Policy::Ask,
                false,
            ),
            ("memory_search", json!({}), Policy::Allow, true),
            ("memory_write", json!({}), Policy::Ask, false),
            ("unknown", json!({}), Policy::Ask, false),
        ] {
            assert_eq!(catalog.policy(&permissions, name, &args), policy, "{name}");
            assert_eq!(catalog.plan_allowed(name), plan, "{name}");
        }
        let child = ToolCatalog::build(1, &extensions);
        assert!(
            !child
                .definitions()
                .iter()
                .any(|tool| tool.function.name == "task")
        );
        assert!(!child.plan_allowed("task"));
        assert_eq!(child.policy(&permissions, "task", &json!({})), Policy::Ask);
    }

    #[test]
    fn shell_approval_cannot_be_bypassed_with_composition_or_expansion() {
        for command in [
            "ls; rm x",
            "ls\nrm x",
            "cat $(touch x)",
            "ls `touch x`",
            "ls > x",
            "cat a | sh",
            "ls && rm x",
            "ls || rm x",
            "ls &",
            "ls${IFS}x",
            "env ls",
            "bash -c ls",
            "python script.py",
            "curl example.com",
            "sudo ls",
            "",
        ] {
            assert!(!read_only_shell(command), "{command:?}");
        }
        for command in [
            "pwd",
            "ls -la",
            "cat src/main.rs",
            "head -n 10 README.md",
            "wc -l README.md",
        ] {
            assert!(read_only_shell(command), "{command}");
        }
    }
}
