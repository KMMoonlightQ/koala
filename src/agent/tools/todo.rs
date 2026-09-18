use super::super::event::{TodoView, UiEvent};
use super::super::plan::TodoItem;
use super::{Tool, ToolContext, ToolResult};
use std::future::Future;
use std::pin::Pin;

pub struct TodoWrite;

#[derive(serde::Deserialize)]
struct Args {
    todos: Vec<TodoItem>,
}

impl Tool for TodoWrite {
    fn name(&self) -> &'static str {
        "todo_write"
    }

    fn description(&self) -> &str {
        "Replace the working todo list. Use it to plan multi-step work and track progress. \
         Each item: {content, status: pending|in_progress|done}."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": {"type": "string"},
                            "status": {"type": "string", "enum": ["pending", "in_progress", "done"]}
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a mut ToolContext,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let Args { todos } = match serde_json::from_value::<Args>(args) {
                Ok(args) => args,
                Err(e) => return ToolResult::err(format!("invalid arguments: {e}")),
            };
            if todos.iter().any(|item| item.content.trim().is_empty()) {
                return ToolResult::err("todo content must not be empty");
            }
            ctx.todos.replace(todos);
            let _ = ctx.events.send(UiEvent::Todos(
                ctx.todos.items.iter().map(TodoView::from).collect(),
            ));
            ToolResult::ok(format!(
                "todo list updated ({} items)",
                ctx.todos.items.len()
            ))
        })
    }
}
