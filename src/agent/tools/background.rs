use super::{Tool, ToolContext, ToolResult};
use std::{future::Future, pin::Pin};

pub struct BackgroundTasks;
impl Tool for BackgroundTasks {
    fn name(&self) -> &'static str {
        "background_tasks"
    }
    fn description(&self) -> &str {
        "List this session's background tasks (newest first), or read a task's output by id. Results are paginated; pass next_offset to continue. Output offsets are UTF-8 bytes."
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"id":{"type":"integer","minimum":1},"offset":{"type":"integer","minimum":0}}})
    }
    fn execute<'a>(
        &'a self,
        ctx: &'a mut ToolContext,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            #[derive(serde::Deserialize)]
            struct Args {
                id: Option<usize>,
                #[serde(default)]
                offset: usize,
            }
            let args = match serde_json::from_value::<Args>(args) {
                Ok(args) => args,
                Err(e) => return ToolResult::err(format!("invalid arguments: {e}")),
            };
            match args.id {
                Some(id) => match ctx.background.read_output(id, args.offset) {
                    Ok(page) => ToolResult::ok(page.to_string()),
                    Err(e) => ToolResult::err(e),
                },
                None => ToolResult::ok(ctx.background.task_page(args.offset, 12).to_string()),
            }
        })
    }
}
