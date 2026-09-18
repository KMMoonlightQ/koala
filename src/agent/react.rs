use super::AgentError;
use super::event::UiEvent;
use super::hooks::{self, HookOutcome};
use super::permissions::Policy;
use super::retry;
use super::tools::{self, ToolCatalog, ToolContext};
use crate::extensions::Stage;
use crate::llm::{DeltaAggregator, Message, ToolCall};
use futures_util::StreamExt;

/// The ReAct loop: stream → tool calls (hooked, permission-checked) → repeat.
/// `messages` must already contain system + history + the new user message.
/// Returns the final assistant text.
pub async fn run(
    ctx: &mut ToolContext<'_>,
    messages: &mut Vec<Message>,
    max_rounds: Option<usize>,
) -> Result<String, AgentError> {
    let registry = ToolCatalog::build(ctx.depth, &ctx.shared.extensions);
    let tool_defs = registry.definitions();

    // Allow one final model response after the last permitted tool round,
    // but never execute tools beyond the configured budget.
    let mut remaining = max_rounds;
    loop {
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
                    system.content.get_or_insert_default().push_str(&context);
                } else {
                    request.insert(0, Message::system(context));
                }
            }
        }
        if let Some(context) = extension.context {
            if let Some(system) = request.first_mut().filter(|m| m.role == "system") {
                system.content.get_or_insert_default().push_str(&context);
            } else {
                request.insert(0, Message::system(context));
            }
        }
        if ctx.depth == 0 {
            let _ = ctx.events.send(UiEvent::ContextUsage(None));
        }
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
                let _ = ctx.events.send(UiEvent::Text(content.clone()));
            }
            agg.push(&delta);
        }
        let assistant = agg.into_message();
        ctx.shared.extensions.hook(Stage::AfterModel,
            serde_json::json!({"message": assistant, "depth": ctx.depth, "plan_mode": ctx.plan_mode})).await.map_err(AgentError::Extension)?;
        messages.push(assistant.clone());
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
        }
    }
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
    let _ = ctx.events.send(UiEvent::ToolStart {
        id: id.clone(),
        name: call.function.name.clone(),
        summary: tools::summarize_args(&call.function.name, &call.function.arguments),
        arguments: call.function.arguments.clone(),
    });
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
    let _ = ctx.events.send(UiEvent::ToolEnd {
        id,
        output: result
            .display_content
            .as_ref()
            .unwrap_or(&result.content)
            .clone(),
        is_error: result.is_error,
        duration_ms: started.elapsed().as_millis() as u64,
    });
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

    if let HookOutcome::Blocked(reason) =
        hooks::run_all(&ctx.shared.hooks.pre_tool_use, &payload).await
    {
        return tools::ToolResult::err(format!("blocked by hook: {reason}"));
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
            std::env::temp_dir().join(format!("kb-unused-{}", uuid::Uuid::new_v4()));
        let mut agent = Agent::new(&cfg).unwrap();
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
        cfg.extensions.memory = false;
        let marker = std::env::temp_dir().join(format!("kb-approval-{}", uuid::Uuid::new_v4()));
        let mut agent = Agent::new(&cfg).unwrap();
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
        let directory = std::env::temp_dir().join(format!("kb-ext-plan-{}", uuid::Uuid::new_v4()));
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.memory.workspace = directory.clone();
        cfg.permissions.mode = crate::config::PermissionMode::NeverAsk;
        let mut agent = Agent::new(&cfg).unwrap();
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
                name: "memory_write".into(),
                arguments: serde_json::json!({
                    "path": "digest/wiki/test", "name": "test", "content": "unique-memory-test"
                })
                .to_string(),
            },
        };
        let blocked = execute_one(&mut ctx, &registry, &call).await;
        assert!(blocked.is_error && blocked.content.contains("plan mode"));
        assert!(!directory.exists());
        ctx.plan_mode = false;
        assert!(!execute_one(&mut ctx, &registry, &call).await.is_error);
        ctx.plan_mode = true;
        call.function.name = "memory_search".into();
        call.function.arguments = serde_json::json!({"query": "unique-memory-test"}).to_string();
        let found = execute_one(&mut ctx, &registry, &call).await;
        assert!(!found.is_error && found.content.contains("unique-memory-test"));
        cfg.permissions.mode = crate::config::PermissionMode::Normal;
        cfg.permissions.deny.push("memory_search".into());
        let mut denied_agent = Agent::new(&cfg).unwrap();
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
