use super::tools::ToolContext;
use super::{AgentError, prompt, react};
use crate::llm::Message;

/// Run a sub-agent with a fresh message context over the shared tool context.
/// Does not see the parent's history or todos; inherits registered extensions.
pub async fn run(ctx: &mut ToolContext<'_>, task_prompt: &str) -> Result<String, AgentError> {
    let extension = ctx.shared.extensions.hook(crate::extensions::Stage::TurnStart,
        serde_json::json!({"input": task_prompt, "depth": ctx.depth, "plan_mode": ctx.plan_mode})).await.map_err(AgentError::Extension)?;
    let mut system = prompt::subagent_system();
    if let Some(context) = extension.context {
        system.push_str(&context);
    }
    let mut messages = vec![Message::system(system), Message::user(task_prompt)];
    let result = react::run(ctx, &mut messages, ctx.shared.subagent_max_rounds).await;
    if let Err(reason) = ctx.shared.extensions.hook(crate::extensions::Stage::TurnEnd,
        serde_json::json!({"input": task_prompt, "reply": result.as_ref().ok(), "error": result.as_ref().err().map(ToString::to_string), "depth": ctx.depth, "plan_mode": ctx.plan_mode})).await {
        let _ = ctx.events.send(super::event::UiEvent::Note(format!("extension: {reason}")));
    }
    result
}
