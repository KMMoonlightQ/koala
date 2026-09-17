use super::super::agentmem::AgentMemory;
use super::super::event::{UiEvent, null_events};
use super::super::plan::TodoList;
use super::super::subagent;
use super::{Tool, ToolContext, ToolResult};
use std::future::Future;
use std::pin::Pin;

pub struct TaskTool;

impl Tool for TaskTool {
    fn name(&self) -> &'static str {
        "task"
    }

    fn description(&self) -> &str {
        "Spawn a sub-agent with its own context to work on a self-contained subtask. \
         The sub-agent cannot spawn further sub-agents. \
         Set background=true to run it asynchronously; completion is reported back."
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
            let description = args
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("subtask")
                .to_string();
            let prompt = args.get("prompt").and_then(|v| v.as_str()).unwrap_or("");
            if prompt.is_empty() {
                return ToolResult::err("prompt must not be empty");
            }
            let background = args
                .get("background")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            if background {
                let id = ctx.background.register("task", &description);
                let seed = ctx.subagent_seed();
                let bg = ctx.background.clone();
                let events = ctx.events.clone();
                let memory_file = ctx.shared.memory_file.clone();
                let prompt = prompt.to_string();
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
                            if !bg.finish(id, true, text.clone()) {
                                return;
                            }
                            let _ = events.send(UiEvent::Note(format!(
                                "✓ #{id} task: {description}\n{}",
                                super::result_preview(&text)
                            )));
                        }
                        Err(e) => {
                            if !bg.finish(id, false, e.to_string()) {
                                return;
                            }
                            let _ = events
                                .send(UiEvent::Note(format!("✗ #{id} task: {description}\n{e}")));
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
            match subagent::run(&mut sub_ctx, prompt).await {
                Ok(text) => ToolResult::ok(text),
                Err(e) => ToolResult::err(format!("sub-agent failed: {e}")),
            }
        })
    }
}
