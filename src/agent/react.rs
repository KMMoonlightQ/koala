use super::AgentError;
use super::event::UiEvent;
use super::hooks::{self, HookOutcome};
use super::permissions::{self, Policy};
use super::retry;
use super::tools::{self, ToolContext, ToolRegistry};
use crate::extensions::Stage;
use crate::llm::{DeltaAggregator, Message, ToolCall};
use futures_util::StreamExt;

/// The ReAct loop: stream → tool calls (hooked, permission-checked) → repeat.
/// `messages` must already contain system + history + the new user message.
/// Returns the final assistant text.
pub async fn run(
    ctx: &mut ToolContext<'_>,
    messages: &mut Vec<Message>,
    max_rounds: usize,
) -> Result<String, AgentError> {
    let registry = ToolRegistry::build(ctx.depth);
    let mut tool_defs = registry.definitions();
    tool_defs.extend(ctx.shared.extensions.tools());
    let mut reply = String::new();

    for _round in 0..=max_rounds {
        let _ = ctx.events.send(UiEvent::Status("正在生成".into()));
        let extension = ctx.shared.extensions.hook(Stage::BeforeModel,
            serde_json::json!({"messages": messages, "tools": tool_defs, "depth": ctx.depth, "plan_mode": ctx.plan_mode})).await.map_err(AgentError::Extension)?;
        let mut request = messages.clone();
        if let Some(context) = extension.context {
            if let Some(system) = request.first_mut().filter(|m| m.role == "system") {
                system.content.get_or_insert_default().push_str(&context);
            } else {
                request.insert(0, Message::system(context));
            }
        }
        let mut stream = connect_with_retry(ctx, &request, &tool_defs).await?;
        let mut agg = DeltaAggregator::default();
        while let Some(delta) = stream.next().await {
            let delta = delta?;
            if let Some(content) = &delta.content {
                let _ = ctx.events.send(UiEvent::Text(content.clone()));
            }
            agg.push(&delta);
        }
        let assistant = agg.into_message();
        ctx.shared.extensions.hook(Stage::AfterModel,
            serde_json::json!({"message": assistant, "depth": ctx.depth, "plan_mode": ctx.plan_mode})).await.map_err(AgentError::Extension)?;
        messages.push(assistant.clone());
        let calls = assistant.tool_calls.clone().unwrap_or_default();
        if calls.is_empty() {
            reply = assistant.content.unwrap_or_default();
            break;
        }
        for call in &calls {
            let result = execute_one(ctx, &registry, call).await;
            messages.push(Message::tool(call.id.clone(), result.content));
        }
    }
    Ok(reply)
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
    registry: &ToolRegistry,
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
    registry: &ToolRegistry,
    call: &ToolCall,
) -> tools::ToolResult {
    let mut call = call.clone();
    let args = match serde_json::from_str::<serde_json::Value>(&call.function.arguments) {
        Ok(args) if args.is_object() => args,
        _ => return tools::ToolResult::err("invalid arguments: expected JSON object"),
    };
    match ctx.shared.extensions.hook(Stage::PreToolUse, serde_json::json!({
        "tool": call.function.name, "arguments": args, "depth": ctx.depth, "plan_mode": ctx.plan_mode
    })).await {
        Ok(response) => if let Some(args) = response.arguments { call.function.arguments = args.to_string(); },
        Err(reason) => return tools::ToolResult::err(reason),
    }
    let name = call.function.name.as_str();
    let payload = serde_json::json!({
        "hook": "pre_tool_use",
        "tool": name,
        "arguments": call.function.arguments,
    });

    if let HookOutcome::Blocked(reason) =
        hooks::run_all(&ctx.shared.hooks.pre_tool_use, &payload).await
    {
        return tools::ToolResult::err(format!("blocked by hook: {reason}"));
    }
    if ctx.plan_mode
        && !permissions::is_plan_mode_tool(name)
        && !ctx.shared.extensions.read_only(name)
    {
        return tools::ToolResult::err(format!(
            "plan mode: {name} is read-only-restricted; finish planning first"
        ));
    }
    match ctx.shared.permissions.check(name) {
        Policy::Deny => return tools::ToolResult::err(format!("permission denied: {name}")),
        Policy::Ask => {
            let summary = tools::summarize_args(name, &call.function.arguments);
            let (tx, rx) = tokio::sync::oneshot::channel();
            let _ = ctx.events.send(UiEvent::PermissionRequest {
                text: format!("{name}({summary})\n{}", call.function.arguments),
                respond: tx,
            });
            match rx.await {
                Ok(true) => {}
                _ => return tools::ToolResult::err(format!("user rejected: {name}")),
            }
        }
        Policy::Allow => {}
    }

    let args = serde_json::from_str(&call.function.arguments).unwrap_or_default();
    let result = match ctx.shared.extensions.execute(name, &args).await {
        Some(Ok(response)) => match response.content {
            Some(content) => tools::ToolResult {
                content,
                is_error: response.is_error,
                display_content: None,
            },
            None => tools::ToolResult::err("extension tool returned no content"),
        },
        Some(Err(reason)) => tools::ToolResult::err(reason),
        None => registry.execute(ctx, &call).await,
    };

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

    async fn execute(policy: &str, command: &str) -> (tools::ToolResult, Vec<UiEvent>) {
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.permissions.default = policy.into();
        cfg.agent.memory_file =
            std::env::temp_dir().join(format!("kb-unused-{}", uuid::Uuid::new_v4()));
        let mut agent = Agent::new(&cfg).unwrap();
        let (events, mut rx) = tokio::sync::mpsc::unbounded_channel();
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
        let result = execute_one(&mut ctx, &ToolRegistry::build(0), &call).await;
        let mut captured = Vec::new();
        while let Ok(ev) = rx.try_recv() {
            captured.push(ev);
        }
        (result, captured)
    }

    #[tokio::test]
    async fn denied_tool_has_a_matching_failure_event() {
        let (result, events) = execute("deny", "must not run").await;
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
        let (result, events) = execute("allow", "printf '%09000d' 1").await;
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
        cfg.permissions.default = "allow".into();
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
        let registry = ToolRegistry::build(0);
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
        cfg.permissions.default = "deny".into();
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
}
