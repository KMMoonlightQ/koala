use super::{Tool, ToolContext, ToolResult};
use std::future::Future;
use std::pin::Pin;

/// Agent-private scratch memory (<config dir>/koala/memory.md), injected into every
/// system prompt. Distinct from the curated knowledge base.
pub struct Remember;

impl Tool for Remember {
    fn name(&self) -> &'static str {
        "remember"
    }

    fn description(&self) -> &str {
        "Append a note to your private agent memory (not the shared knowledge base). \
         Use it for working state, hints to future you, and ephemeral preferences."
    }

    fn prompt_snippet(&self, lang: crate::i18n::Lang) -> &str {
        crate::i18n::text(lang, crate::i18n::Key::ToolRememberSnippet)
    }

    fn prompt_guidelines(&self, lang: crate::i18n::Lang) -> &str {
        crate::i18n::text(lang, crate::i18n::Key::ToolRememberRules)
    }

    fn schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {"type": "string", "description": "note to remember"}
            },
            "required": ["text"]
        })
    }

    fn execute<'a>(
        &'a self,
        ctx: &'a mut ToolContext,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let Some(text) = args.get("text").and_then(|v| v.as_str()) else {
                return ToolResult::err("text must be a string");
            };
            if text.trim().is_empty() {
                return ToolResult::err("text must not be empty");
            }
            match ctx.agent_memory.remember(text) {
                Ok(()) => ToolResult::ok("remembered"),
                Err(e) => ToolResult::err(format!("remember failed: {e}")),
            }
        })
    }
}
