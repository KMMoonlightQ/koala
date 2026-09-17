use super::super::event::{TodoView, UiEvent};
use super::super::plan::{TodoItem, TodoStatus};
use super::{Tool, ToolContext, ToolResult};
use std::future::Future;
use std::pin::Pin;

pub struct TodoWrite;

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
            let Some(items) = args.get("todos").and_then(|v| v.as_array()) else {
                return ToolResult::err("todos must be an array");
            };
            let mut todos = Vec::with_capacity(items.len());
            for item in items {
                let content = item
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if content.is_empty() {
                    return ToolResult::err("todo content must not be empty");
                }
                let status = match item.get("status").and_then(|v| v.as_str()) {
                    Some("done") => TodoStatus::Done,
                    Some("in_progress") => TodoStatus::InProgress,
                    _ => TodoStatus::Pending,
                };
                todos.push(TodoItem { content, status });
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
