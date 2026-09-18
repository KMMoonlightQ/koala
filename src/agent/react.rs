use super::AgentError;
use super::event::UiEvent;
use super::hooks::{self, HookOutcome};
use super::permissions::Policy;
use super::tools::{self, ToolCatalog, ToolContext};
use super::{prompt, retry};
use crate::extensions::Stage;
use crate::llm::{DeltaAggregator, Message, ToolCall};
use futures_util::StreamExt;

/// The ReAct loop: stream → tool calls (hooked, permission-checked) → repeat.
/// `messages` contains history + the new user message; system state is refreshed
/// before every request, including after tools update memory or todos.
/// Returns the final assistant text.
pub async fn run(
    ctx: &mut ToolContext<'_>,
    messages: &mut Vec<Message>,
    max_rounds: Option<usize>,
    turn_context: Option<&str>,
) -> Result<String, AgentError> {
    let registry = ToolCatalog::build(ctx.depth, &ctx.shared.extensions)
        .with_memory_controls(ctx.agent_memory);
    let tool_defs = registry.definitions_for_mode(ctx.plan_mode);
    let resources = prompt::PromptResources::load(
        std::env::current_dir()?,
        dirs::config_dir().map(|path| path.join("koala")).as_deref(),
    )?;

    // Allow one final model response after the last permitted tool round,
    // but never execute tools beyond the configured budget.
    let mut remaining = max_rounds;
    let mut compact_attempts = 0;
    loop {
        let memory = ctx
            .agent_memory
            .content()
            .map_err(|source| AgentError::Io {
                path: ctx.shared.memory_file.display().to_string(),
                source,
            })?;
        let mut system = prompt::build_system(prompt::PromptOptions {
            resources: &resources,
            catalog: &registry,
            agent_memory: &memory,
            skills: ctx.skills,
            todos: ctx.todos,
            plan_mode: ctx.plan_mode,
            depth: ctx.depth,
            lang: ctx.shared.lang.get(),
        });
        if let Some(context) = turn_context {
            prompt::append_section(&mut system, "turn_context", context);
        }
        if messages
            .first()
            .is_some_and(|message| message.role == "system")
        {
            messages[0] = Message::system(system);
        } else {
            messages.insert(0, Message::system(system));
        }
        let _ = ctx.events.send(UiEvent::Status(
            crate::i18n::text(ctx.shared.lang.get(), crate::i18n::Key::StatusGenerating).into(),
        ));
        let extension = ctx.shared.extensions.hook(Stage::BeforeModel,
            serde_json::json!({"messages": messages, "tools": tool_defs, "depth": ctx.depth, "plan_mode": ctx.plan_mode})).await.map_err(AgentError::Extension)?;
        let mut request = messages.clone();
        if ctx.depth == 0 {
            let context = ctx.background.result_context();
            if !context.is_empty() {
                if let Some(system) = request.first_mut().filter(|m| m.role == "system") {
                    prompt::append_section(
                        system.content.get_or_insert_default(),
                        "background_results",
                        &context,
                    );
                } else {
                    request.insert(0, Message::system(context));
                }
            }
        }
        if let Some(context) = extension.context {
            if let Some(system) = request.first_mut().filter(|m| m.role == "system") {
                prompt::append_section(
                    system.content.get_or_insert_default(),
                    "extension_context",
                    &context,
                );
            } else {
                request.insert(0, Message::system(context));
            }
        }
        if ctx.depth == 0 {
            let _ = ctx.events.send(UiEvent::ContextUsage(None));
        }
        // Count the actual serialized request components, including dynamic
        // extension/background context and tool definitions. This is a byte
        // budget, not a provider-specific token-window claim.
        let used = serde_json::to_vec(&request)
            .expect("serializable messages")
            .len()
            + serde_json::to_vec(&tool_defs)
                .expect("serializable tools")
                .len()
            + 4096; // space reserved for the response
        let limit = ctx.shared.compact_threshold;
        if used > limit {
            if compact_attempts >= 2 {
                return Err(AgentError::ContextBudget { used, limit });
            }
            let _ = ctx.events.send(UiEvent::Status(
                crate::i18n::text(ctx.shared.lang.get(), crate::i18n::Key::StatusCompacting).into(),
            ));
            ctx.shared
                .extensions
                .hook(
                    Stage::BeforeCompact,
                    compaction_payload(ctx, messages, None),
                )
                .await
                .map_err(AgentError::Extension)?;
            let mut history: Vec<_> = messages
                .iter()
                .filter(|m| m.role != "system")
                .cloned()
                .collect();
            let keep = if compact_attempts == 0 {
                super::compact::KEEP_RECENT
            } else {
                1
            };
            let changed =
                super::compact::compact_keeping(&ctx.shared.llm, &mut history, keep, limit).await?;
            if changed {
                *messages = history;
                checkpoint(ctx, messages)?;
            }
            ctx.shared
                .extensions
                .hook(
                    Stage::AfterCompact,
                    compaction_payload(ctx, messages, Some(changed)),
                )
                .await
                .map_err(AgentError::Extension)?;
            compact_attempts += 1;
            // Rebuild system state and extension context after compaction.
            continue;
        }
        compact_attempts = 0;
        let mut stream = connect_with_retry(ctx, &request, &tool_defs).await?;
        let mut agg = DeltaAggregator::default();
        while let Some(delta) = stream.next().await {
            let delta = delta?;
            if ctx.depth == 0
                && let Some(usage) = &delta.usage
            {
                let _ = ctx.events.send(UiEvent::ContextUsage(Some(
                    usage.prompt_tokens.saturating_add(usage.completion_tokens),
                )));
            }
            if let Some(content) = &delta.content {
                emit(ctx, UiEvent::Text(content.clone()))?;
            }
            agg.push(&delta);
        }
        let assistant = agg.into_message();
        ctx.shared.extensions.hook(Stage::AfterModel,
            serde_json::json!({"message": assistant, "depth": ctx.depth, "plan_mode": ctx.plan_mode})).await.map_err(AgentError::Extension)?;
        messages.push(assistant.clone());
        checkpoint(ctx, messages)?;
        let calls = assistant.tool_calls.as_deref().unwrap_or_default();
        if calls.is_empty() {
            return Ok(assistant.content.unwrap_or_default());
        }
        if let Some(rounds) = remaining.as_mut() {
            if *rounds == 0 {
                return Err(AgentError::ToolRoundLimit(max_rounds.unwrap()));
            }
            *rounds -= 1;
        }
        for call in calls {
            let result = execute_one(ctx, &registry, call).await;
            messages.push(Message::tool(call.id.clone(), result.content));
            checkpoint(ctx, messages)?;
        }
    }
}

fn compaction_payload(
    ctx: &ToolContext<'_>,
    messages: &[Message],
    changed: Option<bool>,
) -> serde_json::Value {
    let mut payload = serde_json::json!({"messages": messages, "depth": ctx.depth});
    if ctx.depth == 0
        && let Some(session) = ctx
            .background
            .journal
            .as_ref()
            .and_then(|j| j.path.file_stem())
            .and_then(|s| s.to_str())
    {
        payload["session"] = session.into();
    }
    if let Some(changed) = changed {
        payload["changed"] = changed.into();
    }
    payload
}

fn checkpoint(ctx: &ToolContext<'_>, messages: &[Message]) -> Result<(), AgentError> {
    if ctx.depth == 0
        && let Some(journal) = &ctx.background.journal
    {
        journal.context(messages, &ctx.todos.items)?;
    }
    Ok(())
}

fn emit(ctx: &ToolContext<'_>, event: UiEvent) -> Result<(), AgentError> {
    use super::work::Trace;
    if ctx.depth == 0
        && let Some(journal) = &ctx.background.journal
    {
        let trace = match &event {
            UiEvent::Text(text) => Trace::Text(text.clone()),
            UiEvent::ToolStart {
                id,
                name,
                summary,
                arguments,
            } => Trace::ToolStart {
                id: id.clone(),
                name: name.clone(),
                summary: summary.clone(),
                arguments: arguments.clone(),
            },
            UiEvent::ToolEnd {
                id,
                output,
                is_error,
                duration_ms,
            } => Trace::ToolEnd {
                id: id.clone(),
                output: output.clone(),
                is_error: *is_error,
                duration_ms: *duration_ms,
            },
            _ => unreachable!(),
        };
        journal.trace(trace)?;
    }
    let _ = ctx.events.send(event);
    Ok(())
}

async fn connect_with_retry(
    ctx: &ToolContext<'_>,
    messages: &[Message],
    tool_defs: &[crate::llm::Tool],
) -> Result<crate::llm::BoxedDeltaStream, AgentError> {
    let mut delays = retry::backoff_delays(ctx.shared.max_retries).into_iter();
    loop {
        match ctx.shared.llm.chat_stream(messages, Some(tool_defs)).await {
            Ok(stream) => return Ok(stream),
            Err(e) if retry::is_retryable(&e) => match delays.next() {
                Some(delay) => tokio::time::sleep(delay).await,
                None => return Err(e.into()),
            },
            Err(e) => return Err(e.into()),
        }
    }
}

async fn execute_one(
    ctx: &mut ToolContext<'_>,
    registry: &ToolCatalog,
    call: &ToolCall,
) -> tools::ToolResult {
    // Use a UI-local invocation id: providers may reuse call ids across rounds.
    let id = uuid::Uuid::new_v4().to_string();
    let started = std::time::Instant::now();
    if let Err(error) = emit(
        ctx,
        UiEvent::ToolStart {
            id: id.clone(),
            name: call.function.name.clone(),
            summary: tools::summarize_args(&call.function.name, &call.function.arguments),
            arguments: call.function.arguments.clone(),
        },
    ) {
        return tools::ToolResult::err(error.to_string());
    }
    let mut result = execute_checked(ctx, registry, call).await;
    match ctx.shared.extensions.hook(Stage::PostToolUse, serde_json::json!({
        "tool": call.function.name, "arguments": serde_json::from_str::<serde_json::Value>(&call.function.arguments).unwrap_or_default(),
        "content": result.content, "is_error": result.is_error, "depth": ctx.depth, "plan_mode": ctx.plan_mode
    })).await {
        Ok(response) => if let Some(content) = response.content {
            result.content = content; result.is_error = response.is_error; result.display_content = None;
        },
        Err(reason) => { let _ = ctx.events.send(UiEvent::Note(format!("extension: {reason}"))); }
    }
    if let Err(error) = emit(
        ctx,
        UiEvent::ToolEnd {
            id,
            output: result
                .display_content
                .as_ref()
                .unwrap_or(&result.content)
                .clone(),
            is_error: result.is_error,
            duration_ms: started.elapsed().as_millis() as u64,
        },
    ) {
        let _ = ctx.events.send(UiEvent::Error(error.to_string()));
    }
    result
}

async fn execute_checked(
    ctx: &mut ToolContext<'_>,
    registry: &ToolCatalog,
    call: &ToolCall,
) -> tools::ToolResult {
    let name = call.function.name.as_str();
    let mut arguments = call.function.arguments.clone();
    let mut args = match serde_json::from_str::<serde_json::Value>(&arguments) {
        Ok(args) if args.is_object() => args,
        _ => return tools::ToolResult::err("invalid arguments: expected JSON object"),
    };
    match ctx
        .shared
        .extensions
        .hook(
            Stage::PreToolUse,
            serde_json::json!({
                "tool": name, "arguments": args, "depth": ctx.depth, "plan_mode": ctx.plan_mode
            }),
        )
        .await
    {
        Ok(response) => {
            if let Some(replacement) = response.arguments {
                arguments = replacement.to_string();
                args = replacement;
            }
        }
        Err(reason) => return tools::ToolResult::err(reason),
    }
    let payload = serde_json::json!({
        "hook": "pre_tool_use",
        "tool": name,
        "arguments": arguments,
    });

    match hooks::run_all(&ctx.shared.hooks.pre_tool_use, &payload).await {
        HookOutcome::Ok => {}
        HookOutcome::Blocked(reason) => {
            return tools::ToolResult::err(format!("blocked by hook: {reason}"));
        }
        HookOutcome::Failed(reason) => {
            return tools::ToolResult::err(format!(
                "pre-tool hook failed; execution blocked: {reason}"
            ));
        }
    }
    if ctx.plan_mode && !registry.plan_allowed(name) {
        return tools::ToolResult::err(format!(
            "plan mode: {name} is read-only-restricted; finish planning first"
        ));
    }
    match registry.policy(&ctx.shared.permissions, name, &args) {
        Policy::Deny => return tools::ToolResult::err(format!("permission denied: {name}")),
        Policy::Ask => {
            let summary = tools::summarize_args(name, &arguments);
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = ctx.events.send(UiEvent::PermissionRequest {
                text: format!("{name}({summary})\n{arguments}"),
                respond: tx,
            });
            match rx.await {
                Ok(true) => {}
                _ => return tools::ToolResult::err(format!("user rejected: {name}")),
            }
        }
        Policy::Allow => {}
    }

    let result = registry.execute(ctx, name, args).await;

    let payload = serde_json::json!({
        "hook": "post_tool_use",
        "tool": name,
        "is_error": result.is_error,
    });
    if let HookOutcome::Failed(reason) =
        hooks::run_all(&ctx.shared.hooks.post_tool_use, &payload).await
    {
        let _ = ctx.events.send(UiEvent::Note(format!("hook: {reason}")));
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::Agent;
    use crate::config::Config;
    use crate::llm::FunctionCall;

    async fn execute(
        policy: &str,
        command: &str,
        silent: bool,
    ) -> (tools::ToolResult, Vec<UiEvent>) {
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.permissions.mode = if policy == "allow" {
            crate::config::PermissionMode::NeverAsk
        } else {
            crate::config::PermissionMode::Normal
        };
        if policy == "deny" {
            cfg.permissions.deny.push("bash".into());
        }
        cfg.agent.memory_file =
            std::env::temp_dir().join(format!("koala-unused-{}", uuid::Uuid::new_v4()));
        let mut agent = Agent::new(&cfg).await.unwrap();
        let (events, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let events = if silent {
            crate::agent::event::null_events()
        } else {
            events
        };
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
        let call = ToolCall {
            id: "provider-id".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "bash".into(),
                arguments: serde_json::json!({"command": command}).to_string(),
            },
        };
        let registry = ToolCatalog::build(0, &ctx.shared.extensions);
        let result = execute_one(&mut ctx, &registry, &call).await;
        let mut captured = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            captured.push(ev);
        }
        (result, captured)
    }

    #[tokio::test]
    async fn approval_response_controls_side_effects_and_mode_switches_apply() {
        use crate::config::PermissionMode;
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();

        let marker = std::env::temp_dir().join(format!("koala-approval-{}", uuid::Uuid::new_v4()));
        let mut agent = Agent::new(&cfg).await.unwrap();
        let (events, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let shared = agent.shared.clone();
        let mut ctx = ToolContext {
            todos: &mut agent.todos,
            agent_memory: &agent.agent_memory,
            background: agent.background.clone(),
            skills: &agent.skills,
            events: &events,
            shared: &shared,
            depth: 0,
            plan_mode: false,
        };
        let registry = ToolCatalog::build(0, &ctx.shared.extensions);
        let call = ToolCall {
            id: "approval".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "bash".into(),
                arguments: serde_json::json!({"command": format!("touch '{}'", marker.display())})
                    .to_string(),
            },
        };
        for (mode, approve) in [
            (PermissionMode::Normal, false),
            (PermissionMode::AskWhenNeed, false),
            (PermissionMode::Normal, true),
        ] {
            shared.permissions.set_mode(mode);
            let responder = async {
                loop {
                    if let Some(UiEvent::PermissionRequest { respond, .. }) = rx.recv().await {
                        assert!(!marker.exists(), "side effect happened before approval");
                        respond.send(approve).unwrap();
                        break;
                    }
                }
            };
            let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(3), async {
                tokio::join!(execute_one(&mut ctx, &registry, &call), responder)
            })
            .await
            .unwrap();
            assert_eq!(result.is_error, !approve);
            assert_eq!(marker.exists(), approve);
        }
        std::fs::remove_file(&marker).unwrap();
        shared.permissions.set_mode(PermissionMode::NeverAsk);
        assert!(
            !tokio::time::timeout(
                std::time::Duration::from_secs(3),
                execute_one(&mut ctx, &registry, &call)
            )
            .await
            .unwrap()
            .is_error
        );
        assert!(marker.exists());
        while let Ok(event) = rx.try_recv() {
            assert!(!matches!(event, UiEvent::PermissionRequest { .. }));
        }
        std::fs::remove_file(marker).unwrap();
    }

    #[tokio::test]
    async fn file_tools_obey_plan_mode_and_permissions_before_mutation() {
        let root = std::env::current_dir()
            .unwrap()
            .join(format!(".koala-file-policy-{}", uuid::Uuid::new_v4()));
        let path = root.join("file.txt");
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.permissions.mode = crate::config::PermissionMode::AskWhenNeed;
        cfg.permissions.deny.push("write".into());
        let mut agent = Agent::new(&cfg).await.unwrap();
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
        let registry = ToolCatalog::build(1, &ctx.shared.extensions);
        let call = |name: &str, args: serde_json::Value| ToolCall {
            id: "file-call".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: name.into(),
                arguments: args.to_string(),
            },
        };
        let write = call(
            "write",
            serde_json::json!({"path":path, "content":"original"}),
        );
        let blocked = execute_one(&mut ctx, &registry, &write).await;
        assert!(blocked.content.contains("permission denied"));
        assert!(!path.exists());
        ctx.shared
            .permissions
            .set_mode(crate::config::PermissionMode::NeverAsk);
        ctx.plan_mode = true;
        assert!(
            execute_one(&mut ctx, &registry, &write)
                .await
                .content
                .contains("plan mode")
        );
        assert!(!path.exists());
        ctx.plan_mode = false;
        assert!(!execute_one(&mut ctx, &registry, &write).await.is_error);
        let edit = call(
            "edit",
            serde_json::json!({"path":path, "edits":[{"oldText":"original","newText":"updated"}]}),
        );
        ctx.plan_mode = true;
        assert!(
            execute_one(&mut ctx, &registry, &edit)
                .await
                .content
                .contains("plan mode")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
        ctx.shared
            .permissions
            .set_mode(crate::config::PermissionMode::AskWhenNeed);
        let read = call("read", serde_json::json!({"path":path}));
        let result = execute_one(&mut ctx, &registry, &read).await;
        assert!(!result.is_error && result.content.contains("original"));
        ctx.plan_mode = false;
        assert!(
            execute_one(&mut ctx, &registry, &edit)
                .await
                .content
                .contains("user rejected")
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
        ctx.shared
            .permissions
            .set_mode(crate::config::PermissionMode::AutoEdit);
        ctx.plan_mode = true;
        assert!(execute_one(&mut ctx, &registry, &edit).await.is_error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "original");
        ctx.plan_mode = false;
        assert!(!execute_one(&mut ctx, &registry, &edit).await.is_error);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "updated");
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn denied_tool_has_a_matching_failure_event() {
        let (result, events) = execute("deny", "must not run", false).await;
        assert!(result.is_error);
        let UiEvent::ToolStart { id, arguments, .. } = &events[0] else {
            panic!("missing start")
        };
        assert!(arguments.contains("must not run"));
        let UiEvent::ToolEnd {
            id: end_id,
            is_error,
            output,
            ..
        } = &events[1]
        else {
            panic!("missing end")
        };
        assert_eq!(id, end_id);
        assert!(*is_error);
        assert!(output.contains("permission denied"));
    }

    #[tokio::test]
    async fn complete_shell_output_reaches_ui_without_expanding_model_context() {
        let (result, events) = execute("allow", "printf '%09000d' 1", false).await;
        assert!(!result.is_error);
        assert!(result.content.len() < 8200);
        let output = events
            .iter()
            .find_map(|ev| match ev {
                UiEvent::ToolEnd { output, .. } => Some(output),
                _ => None,
            })
            .unwrap();
        assert_eq!(output.len(), 9000);
        assert!(output.ends_with('1'));
    }
    #[tokio::test]
    async fn extension_tools_obey_plan_mode_and_permissions() {
        let directory =
            std::env::temp_dir().join(format!("koala-ext-plan-{}", uuid::Uuid::new_v4()));
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        std::fs::create_dir_all(&directory).unwrap();
        let manifest = directory.join("extension.toml");
        std::fs::write(
            &manifest,
            r#"api_version = 1
name = "test-tools"
command = ["bash", "-c", "cat >/dev/null; printf '%s' '{\"content\":\"unique-memory-test\"}'"]
[[tools]]
name = "test_write"
description = "write"
parameters = { type = "object" }
[[tools]]
name = "test_search"
description = "search"
parameters = { type = "object" }
read_only = true
"#,
        )
        .unwrap();
        cfg.extensions.manifests = vec![manifest];
        cfg.permissions.mode = crate::config::PermissionMode::NeverAsk;
        let mut agent = Agent::new(&cfg).await.unwrap();
        let (events, _rx) = tokio::sync::mpsc::unbounded_channel();
        let mut ctx = ToolContext {
            todos: &mut agent.todos,
            agent_memory: &agent.agent_memory,
            background: agent.background.clone(),
            skills: &agent.skills,
            events: &events,
            shared: &agent.shared,
            depth: 0,
            plan_mode: true,
        };
        let registry = ToolCatalog::build(0, &ctx.shared.extensions);
        let mut call = ToolCall {
            id: "extension-call".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "test_write".into(),
                arguments: serde_json::json!({
                    "path": "digest/wiki/test", "name": "test", "content": "unique-memory-test"
                })
                .to_string(),
            },
        };
        let blocked = execute_one(&mut ctx, &registry, &call).await;
        assert!(blocked.is_error && blocked.content.contains("plan mode"));
        assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        ctx.plan_mode = false;
        assert!(!execute_one(&mut ctx, &registry, &call).await.is_error);
        ctx.plan_mode = true;
        call.function.name = "test_search".into();
        call.function.arguments = serde_json::json!({"query": "unique-memory-test"}).to_string();
        let found = execute_one(&mut ctx, &registry, &call).await;
        assert!(!found.is_error && found.content.contains("unique-memory-test"));
        cfg.permissions.mode = crate::config::PermissionMode::Normal;
        cfg.permissions.deny.push("test_search".into());
        let mut denied_agent = Agent::new(&cfg).await.unwrap();
        let mut denied_ctx = ToolContext {
            todos: &mut denied_agent.todos,
            agent_memory: &denied_agent.agent_memory,
            background: denied_agent.background.clone(),
            skills: &denied_agent.skills,
            events: &events,
            shared: &denied_agent.shared,
            depth: 0,
            plan_mode: false,
        };
        let denied = execute_one(&mut denied_ctx, &registry, &call).await;
        assert!(denied.is_error && denied.content.contains("permission denied"));
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[tokio::test]
    async fn silent_tools_reject_permissions_without_hanging() {
        let (denied, _) = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            execute("ask", "printf must-not-run", true),
        )
        .await
        .expect("silent permission request must resolve");
        assert!(denied.is_error);
        assert!(denied.content.contains("user rejected"));
        let (allowed, _) = execute("allow", "printf allowed", true).await;
        assert!(!allowed.is_error);
        assert_eq!(allowed.content, "allowed");
    }
}
