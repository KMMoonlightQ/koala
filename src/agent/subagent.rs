use super::tools::ToolContext;
use super::{AgentError, prompt, react};
use crate::llm::Message;

/// Run a sub-agent with a fresh message context over the shared tool context.
/// Does not see the parent's history, todos, or the knowledge base.
pub async fn run(ctx: &mut ToolContext<'_>, task_prompt: &str) -> Result<String, AgentError> {
    let mut messages = vec![
        Message::system(prompt::subagent_system()),
        Message::user(task_prompt),
    ];
    react::run(ctx, &mut messages, ctx.shared.subagent_max_rounds).await
}
