//! One tool directory for definitions, capabilities and execution ownership.
use super::{Tool, ToolContext, ToolResult, background, bash, files, remember, skill, task, todo};
use crate::agent::permissions::{Permissions, Policy};
use crate::extensions::{Extension, Extensions};
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Copy)]
enum Approval {
    Always,
    Shell,
    WorkspaceEdit,
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
        tool: &background::BackgroundTasks,
        plan_allowed: true,
        approval: Approval::Always,
        root_only: false,
    },
    Builtin {
        tool: &files::Read,
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
        tool: &files::Edit,
        plan_allowed: false,
        approval: Approval::WorkspaceEdit,
        root_only: false,
    },
    Builtin {
        tool: &files::Write,
        plan_allowed: false,
        approval: Approval::WorkspaceEdit,
        root_only: false,
    },
    Builtin {
        tool: &remember::Recall,
        plan_allowed: true,
        approval: Approval::Always,
        root_only: false,
    },
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
    workspace: Option<std::path::PathBuf>,
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
        Self {
            entries,
            workspace: std::env::current_dir()
                .ok()
                .and_then(|path| path.canonicalize().ok()),
        }
    }

    pub fn with_memory_controls(mut self, memory: &crate::agent::agentmem::AgentMemory) -> Self {
        self.entries
            .retain(|entry| match entry.definition.function.name.as_str() {
                "remember" => memory.write_enabled(),
                "recall" => memory.read_enabled(),
                _ => true,
            });
        self
    }

    fn find(&self, name: &str) -> Option<&Entry> {
        self.entries
            .iter()
            .find(|entry| entry.definition.function.name == name)
    }

    pub fn definitions(&self) -> Vec<crate::llm::Tool> {
        self.definitions_for_mode(false)
    }

    /// Prompt metadata and API definitions share the same visibility decisions.
    pub fn prompt_sections(
        &self,
        lang: crate::i18n::Lang,
        plan_mode: bool,
    ) -> (String, Vec<String>) {
        let mut tools = Vec::new();
        let mut rules = Vec::new();
        for entry in &self.entries {
            if plan_mode && !entry.plan_allowed {
                continue;
            }
            let (snippet, guidelines) = match &entry.target {
                Target::Builtin(tool) => (tool.prompt_snippet(lang), tool.prompt_guidelines(lang)),
                Target::Extension(_) => (entry.definition.function.description.as_str(), ""),
            };
            tools.push(format!("- {}: {}", entry.definition.function.name, snippet));
            for rule in guidelines.lines().map(str::trim).filter(|s| !s.is_empty()) {
                if !rules.iter().any(|existing| existing == rule) {
                    rules.push(rule.to_owned());
                }
            }
        }
        (tools.join("\n"), rules)
    }

    pub fn definitions_for_mode(&self, plan_mode: bool) -> Vec<crate::llm::Tool> {
        self.entries
            .iter()
            .filter(|entry| !plan_mode || entry.plan_allowed)
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
            Approval::WorkspaceEdit => {
                permissions.mode() == crate::config::PermissionMode::AutoEdit
                    && self.workspace.as_deref().is_some_and(|root| {
                        args.get("path")
                            .and_then(Value::as_str)
                            .is_some_and(|path| workspace_edit(root, path))
                    })
            }
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

/// Resolve symlinks in the nearest existing ancestor before allowing new files
/// or directories. Broken links, traversal through missing parents and errors ask.
fn workspace_edit(root: &std::path::Path, path: &str) -> bool {
    let Ok(mut path) = files::resolve_path(path) else {
        return false;
    };
    let mut missing = Vec::new();
    loop {
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                let Ok(mut target) = path.canonicalize() else {
                    return false;
                };
                for component in missing.iter().rev() {
                    target.push(component);
                }
                return target.starts_with(root);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = path.file_name() else {
                    return false;
                };
                missing.push(name.to_owned());
                if !path.pop() {
                    return false;
                }
            }
            Err(_) => return false,
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

    struct Echo;
    impl Extension for Echo {
        fn name(&self) -> &str {
            "catalog-test"
        }
        fn tools(&self) -> Vec<crate::llm::Tool> {
            ["echo", "save"]
                .into_iter()
                .map(|name| crate::llm::Tool::function(name, "test tool", json!({"type":"object"})))
                .collect()
        }
        fn read_only(&self, name: &str) -> bool {
            name == "echo"
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

        let mut agent = crate::agent::Agent::new(&cfg).await.unwrap();
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
        extensions.register(Arc::new(Echo)).unwrap();
        let catalog = ToolCatalog::build(0, &extensions);
        let permissions = Permissions::new(&PermissionsConfig {
            mode: PermissionMode::AskWhenNeed,
            ..Default::default()
        });
        for (name, args, policy, plan) in [
            ("read", json!({}), Policy::Allow, true),
            ("edit", json!({}), Policy::Ask, false),
            ("write", json!({}), Policy::Ask, false),
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
            ("echo", json!({}), Policy::Allow, true),
            ("save", json!({}), Policy::Ask, false),
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
    fn auto_edit_limits_mutations_to_resolved_workspace_paths() {
        let root = std::env::temp_dir().join(format!("koala-permissions-{}", uuid::Uuid::new_v4()));
        let workspace = root.join("workspace");
        let outside = root.join("workspace-other");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(workspace.join("existing"), "original").unwrap();
        std::fs::write(outside.join("existing"), "original").unwrap();
        let mut extensions = Extensions::default();
        extensions.register(Arc::new(Echo)).unwrap();
        let mut catalog = ToolCatalog::build(0, &extensions);
        catalog.workspace = Some(workspace.canonicalize().unwrap());
        let permissions = Permissions::new(&PermissionsConfig {
            mode: PermissionMode::AutoEdit,
            ..Default::default()
        });
        let policy = |name: &str, path: std::path::PathBuf| {
            catalog.policy(&permissions, name, &json!({"path": path}))
        };
        for name in ["edit", "write"] {
            assert_eq!(policy(name, workspace.join("existing")), Policy::Allow);
            assert_eq!(policy(name, workspace.join("new")), Policy::Allow);
            assert_eq!(policy(name, outside.join("new")), Policy::Ask);
            assert_eq!(
                policy(name, workspace.join("../workspace-other/existing")),
                Policy::Ask
            );
            assert_eq!(policy(name, workspace.join("missing/new")), Policy::Allow);
            assert_eq!(catalog.policy(&permissions, name, &json!({})), Policy::Ask);
            assert_eq!(
                catalog.policy(&permissions, name, &json!({"path":""})),
                Policy::Ask
            );
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, workspace.join("linked-dir")).unwrap();
            std::os::unix::fs::symlink(outside.join("existing"), workspace.join("linked-file"))
                .unwrap();
            std::os::unix::fs::symlink(outside.join("missing"), workspace.join("dangling"))
                .unwrap();
            std::os::unix::fs::symlink(workspace.join("existing"), workspace.join("internal"))
                .unwrap();
            for path in ["linked-dir/new", "linked-file", "dangling"] {
                assert_eq!(policy("write", workspace.join(path)), Policy::Ask, "{path}");
            }
            assert_eq!(policy("edit", workspace.join("internal")), Policy::Allow);
        }
        for (name, args, expected) in [
            ("bash", json!({"command":"ls"}), Policy::Allow),
            ("bash", json!({"command":"cargo test"}), Policy::Ask),
            ("bash", json!({"command":"rm file"}), Policy::Ask),
            ("echo", json!({}), Policy::Allow),
            ("save", json!({}), Policy::Ask),
            ("unknown", json!({}), Policy::Ask),
        ] {
            assert_eq!(catalog.policy(&permissions, name, &args), expected);
        }
        permissions.set_mode(PermissionMode::AskWhenNeed);
        assert_eq!(policy("edit", workspace.join("existing")), Policy::Ask);
        let denied = Permissions::new(&PermissionsConfig {
            mode: PermissionMode::AutoEdit,
            deny: vec!["write".into()],
            ..Default::default()
        });
        assert_eq!(
            catalog.policy(&denied, "write", &json!({"path":workspace.join("new")})),
            Policy::Deny
        );
        catalog.workspace = None;
        permissions.set_mode(PermissionMode::AutoEdit);
        assert_eq!(
            catalog.policy(
                &permissions,
                "write",
                &json!({"path":workspace.join("new")})
            ),
            Policy::Ask
        );
        std::fs::remove_dir_all(root).unwrap();
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
