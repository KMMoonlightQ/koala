use super::{Tool, ToolContext, ToolResult};
use std::future::Future;
use std::pin::Pin;

pub struct SkillTool;

impl Tool for SkillTool {
    fn name(&self) -> &'static str {
        "skill"
    }

    fn description(&self) -> &str {
        "Load the full instructions of a skill listed in the system prompt by name."
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "skill name"}
            },
            "required": ["name"]
        })
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a mut ToolContext,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let Some(name) = args.get("name").and_then(|v| v.as_str()) else {
                return ToolResult::err("name must be a string");
            };
            match ctx.skills.get(name) {
                Some(skill) => ToolResult::ok(skill.body.clone()),
                None => ToolResult::err(format!("unknown skill: {name}")),
            }
        })
    }
}
