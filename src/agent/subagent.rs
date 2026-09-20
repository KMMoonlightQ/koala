use super::tools::ToolContext;
use super::{AgentError, react};
use crate::llm::Message;

/// Run a sub-agent with a fresh message context over the shared tool context.
/// Does not see the parent's history or todos; inherits registered extensions.
pub async fn run(ctx: &mut ToolContext<'_>, task_prompt: &str) -> Result<String, AgentError> {
    let mut span = ctx
        .graph
        .as_ref()
        .map(|r| {
            r.start(
                super::graph::Kind::Subagent,
                task_prompt.lines().next().unwrap_or("Sub-agent"),
                serde_json::json!({"input": task_prompt}),
                vec![],
            )
        })
        .transpose()?;
    let parent = ctx.graph.clone();
    if let Some(span) = &span {
        ctx.graph = Some(span.recorder());
    }
    let result = run_recorded(ctx, task_prompt).await;
    ctx.graph = parent;
    if let Some(span) = &mut span {
        span.finish(
            if result.is_ok() {
                super::graph::Status::Succeeded
            } else {
                super::graph::Status::Failed
            },
            match &result {
                Ok(text) => serde_json::json!({"output": text}),
                Err(e) => serde_json::json!({"error": e.to_string()}),
            },
        )?;
    }
    result
}

async fn run_recorded(ctx: &mut ToolContext<'_>, task_prompt: &str) -> Result<String, AgentError> {
    let extension = ctx.shared.extensions.hook(crate::extensions::Stage::TurnStart,
        serde_json::json!({"input": task_prompt, "depth": ctx.depth, "plan_mode": ctx.plan_mode})).await.map_err(AgentError::Extension)?;
    let mut messages = vec![Message::user(task_prompt)];
    let result = react::run(
        ctx,
        &mut messages,
        ctx.shared.subagent_max_rounds,
        extension.context.as_deref(),
    )
    .await;
    if let Err(reason) = ctx.shared.extensions.hook(crate::extensions::Stage::TurnEnd,
        serde_json::json!({"input": task_prompt, "reply": result.as_ref().ok(), "error": result.as_ref().err().map(ToString::to_string), "depth": ctx.depth, "plan_mode": ctx.plan_mode})).await {
        let _ = ctx.events.send(super::event::UiEvent::Note(format!("extension: {reason}")));
    }
    result
}
