use super::super::agentmem::AgentMemory;
use super::super::event::null_events;
use super::super::plan::TodoList;
use super::super::subagent;
use super::{Tool, ToolContext, ToolResult};
use std::future::Future;
use std::pin::Pin;

pub struct TaskTool;

#[derive(serde::Deserialize)]
struct Args {
    description: String,
    prompt: String,
    #[serde(default)]
    background: bool,
}

impl Tool for TaskTool {
    fn name(&self) -> &'static str {
        "task"
    }

    fn description(&self) -> &str {
        "Spawn a sub-agent with its own context to work on a self-contained subtask. \
         The sub-agent cannot spawn further sub-agents. \
         Set background=true to run it asynchronously; completion is reported back."
    }

    fn prompt_snippet(&self, lang: crate::i18n::Lang) -> &str {
        crate::i18n::text(lang, crate::i18n::Key::ToolTaskSnippet)
    }

    fn prompt_guidelines(&self, lang: crate::i18n::Lang) -> &str {
        crate::i18n::text(lang, crate::i18n::Key::ToolTaskRules)
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "description": {"type": "string", "description": "short task label"},
                "prompt": {"type": "string", "description": "full instructions for the sub-agent"},
                "background": {"type": "boolean", "description": "run in background, default false"}
            },
            "required": ["description", "prompt"]
        })
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a mut ToolContext,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let Args {
                description,
                prompt,
                background,
            } = match serde_json::from_value::<Args>(args) {
                Ok(args) => args,
                Err(e) => return ToolResult::err(format!("invalid arguments: {e}")),
            };
            if prompt.is_empty() {
                return ToolResult::err("prompt must not be empty");
            }

            if background {
                let id = ctx.background.register("task", &description);
                let seed = ctx.subagent_seed();
                let bg = ctx.background.clone();
                let memory_file = ctx.shared.memory_file.clone();
                let handle = tokio::spawn(async move {
                    let outcome = async {
                        let mut todos = TodoList::default();
                        let mem = AgentMemory::load(memory_file);
                        let null_tx = null_events();
                        let mut sub_ctx = seed.build(&mut todos, &mem, &null_tx);
                        subagent::run(&mut sub_ctx, &prompt).await
                    }
                    .await;
                    match outcome {
                        Ok(text) => {
                            bg.finish(id, true, text);
                        }
                        Err(error) => {
                            bg.finish(id, false, error.to_string());
                        }
                    }
                });
                ctx.background.attach(id, handle);
                return ToolResult::ok(format!("background task #{id} started"));
            }

            let seed = ctx.subagent_seed();
            let mut sub_todos = TodoList::default();
            let null_tx = null_events();
            let mut sub_ctx = seed.build(&mut sub_todos, ctx.agent_memory, &null_tx);
            match subagent::run(&mut sub_ctx, &prompt).await {
                Ok(text) => ToolResult::ok(text),
                Err(e) => ToolResult::err(format!("sub-agent failed: {e}")),
            }
        })
    }
}
