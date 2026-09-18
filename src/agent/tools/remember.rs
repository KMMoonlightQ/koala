use super::{Tool, ToolContext, ToolResult};
use crate::agent::agentmem::{Kind, Note, Scope};
use serde::Deserialize;
use std::future::Future;
use std::pin::Pin;

pub struct Remember;
pub struct Recall;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum WriteArgs {
    Upsert {
        key: String,
        kind: Kind,
        #[serde(default)]
        scope: Scope,
        summary: String,
        #[serde(default)]
        details: String,
        #[serde(default)]
        expires_on: Option<String>,
    },
    Forget {
        key: String,
        #[serde(default)]
        scope: Scope,
    },
}

impl Tool for Remember {
    fn name(&self) -> &'static str {
        "remember"
    }
    fn description(&self) -> &str {
        "Maintain curated, reusable private memory. Upsert by stable key to correct or merge; forget obsolete keys. Save durable preferences, user corrections, constraints or references only. Never save completion reports, test counts, todos, temporary state or raw outputs. Search with recall before writing; no new learning means no write. Project is the default scope; global is only for cross-project preferences/feedback. Time-limited context requires expires_on."
    }
    fn prompt_snippet(&self, lang: crate::i18n::Lang) -> &str {
        crate::i18n::text(lang, crate::i18n::Key::ToolRememberSnippet)
    }
    fn prompt_guidelines(&self, lang: crate::i18n::Lang) -> &str {
        crate::i18n::text(lang, crate::i18n::Key::ToolRememberRules)
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object", "additionalProperties":false, "properties":{
            "action":{"type":"string","enum":["upsert","forget"]},
            "key":{"type":"string","description":"Stable topic key, 1-64 lowercase letters/digits/-/_. Reuse existing keys for corrections."},
            "scope":{"type":"string","enum":["project","global"],"default":"project"},
            "kind":{"type":"string","enum":["preference","feedback","constraint","reference"]},
            "summary":{"type":"string","description":"Required for upsert: one reusable conclusion, one line, at most 160 characters."},
            "details":{"type":"string","description":"Optional reusable rationale and supporting references, at most 4000 characters. Replaces previous details."},
            "expires_on":{"type":"string","description":"Optional UTC date YYYY-MM-DD; inactive on and after this date. Required for time-limited facts."}
        },"required":["action","key"]})
    }
    fn execute<'a>(
        &'a self,
        ctx: &'a mut ToolContext,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            // Defense in depth for callers that do not go through the ReAct gate.
            if ctx.plan_mode {
                return ToolResult::err("memory writes are unavailable in plan mode");
            }
            let result = match serde_json::from_value::<WriteArgs>(args) {
                Ok(WriteArgs::Upsert { key, kind, scope, summary, details, expires_on }) =>
                    ctx.agent_memory.upsert(Note { key, kind, scope, summary, details, expires_on })
                        .map(|entry| serde_json::json!({"saved":entry.note.key,"scope":entry.note.scope,"updated_at":entry.updated_at}).to_string()),
                Ok(WriteArgs::Forget { key, scope }) => ctx.agent_memory.forget(&key, scope)
                    .map(|deleted| serde_json::json!({"deleted":deleted,"key":key,"scope":scope}).to_string()),
                Err(e) => return ToolResult::err(format!("invalid memory arguments: {e}")),
            };
            match result {
                Ok(text) => ToolResult::ok(text),
                Err(e) => ToolResult::err(e.to_string()),
            }
        })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    key: Option<String>,
    #[serde(default)]
    scope: Scope,
    #[serde(default)]
    query: String,
    #[serde(default)]
    offset: usize,
}

impl Tool for Recall {
    fn name(&self) -> &'static str {
        "recall"
    }
    fn description(&self) -> &str {
        "Read private memory as historical reference, never as current authorization or verified state. Supply key and scope for full details; otherwise search key/summary/details by query (empty lists active memories). Returns 20 summaries per page; use offset for omitted results. Only global and current-project, unexpired entries are visible."
    }
    fn schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","additionalProperties":false,"properties":{
            "key":{"type":"string"},"scope":{"type":"string","enum":["project","global"],"default":"project"},
            "query":{"type":"string"},"offset":{"type":"integer","minimum":0}
        }})
    }
    fn execute<'a>(
        &'a self,
        ctx: &'a mut ToolContext,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = ToolResult> + Send + 'a>> {
        Box::pin(async move {
            let args: ReadArgs = match serde_json::from_value(args) {
                Ok(args) => args,
                Err(e) => return ToolResult::err(format!("invalid recall arguments: {e}")),
            };
            let result = if let Some(key) = args.key {
                ctx.agent_memory
                    .get(&key, args.scope)
                    .map(|e| serde_json::json!(e))
            } else {
                ctx.agent_memory.search(&args.query).map(|entries| {
                    let total = entries.len();
                    let page: Vec<_> = entries.into_iter().skip(args.offset).take(20).map(|e| serde_json::json!({
                        "key":e.note.key,"scope":e.note.scope,"kind":e.note.kind,"summary":e.note.summary,
                        "updated_at":e.updated_at,"expires_on":e.note.expires_on
                    })).collect();
                    serde_json::json!({"total":total,"offset":args.offset,"entries":page})
                })
            };
            match result {
                Ok(value) => ToolResult::ok(value.to_string()),
                Err(e) => ToolResult::err(e.to_string()),
            }
        })
    }
}
