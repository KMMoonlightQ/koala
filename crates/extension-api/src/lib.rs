use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{future::Future, pin::Pin};

#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: ToolFunction,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolFunction {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl Tool {
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Self {
        Self {
            kind: "function",
            function: ToolFunction {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

pub type ExtensionFuture<'a> = Pin<Box<dyn Future<Output = Result<Response, String>> + Send + 'a>>;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    TurnStart,
    BeforeModel,
    AfterModel,
    PreToolUse,
    PostToolUse,
    BeforeCompact,
    AfterCompact,
    TurnEnd,
}

#[derive(Default, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Response {
    /// Context appended to the system prompt (turn_start / before_model).
    pub context: Option<String>,
    /// Veto a pre-action hook. Other phases report this as an error.
    pub block: Option<String>,
    /// Replacement JSON arguments (pre_tool_use only).
    pub arguments: Option<Value>,
    /// Tool execution or post_tool_use replacement result.
    pub content: Option<String>,
    pub is_error: bool,
}

pub trait Extension: Send + Sync {
    fn name(&self) -> &str;
    fn tools(&self) -> Vec<Tool> {
        Vec::new()
    }
    fn read_only(&self, _tool: &str) -> bool {
        false
    }
    fn hook<'a>(&'a self, stage: Stage, payload: &'a Value) -> ExtensionFuture<'a>;
    fn execute<'a>(&'a self, _name: &'a str, _args: &'a Value) -> ExtensionFuture<'a> {
        Box::pin(async { Err("unknown extension tool".into()) })
    }
}
