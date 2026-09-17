use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use thiserror::Error;

pub type BoxedDeltaStream = BoxStream<'static, Result<StreamDelta, LlmError>>;

#[derive(Debug, Error)]
pub enum LlmError {
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("api error (status {status}): {body}")]
    Api { status: u16, body: String },
    #[error("invalid SSE payload: {0}")]
    Sse(#[from] serde_json::Error),
    #[error("response contained no choices")]
    EmptyResponse,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: Some(content.into()),
            ..Default::default()
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: Some(content.into()),
            ..Default::default()
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: Some(content.into()),
            ..Default::default()
        }
    }

    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content.into()),
            tool_call_id: Some(tool_call_id.into()),
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

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

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamDelta {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCallDelta>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolCallDelta {
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChatChunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
}

#[derive(Debug, Deserialize)]
struct ChunkChoice {
    delta: ChunkDelta,
}

#[derive(Debug, Deserialize)]
struct ChunkDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChunkToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ChunkToolCall {
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<ChunkFunction>,
}

#[derive(Debug, Deserialize)]
struct ChunkFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// Incremental SSE line parser for `stream=true` chat completions.
/// Pure and byte-oriented so tests can feed it `&str` fixtures.
#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    done: bool,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<Result<StreamDelta, LlmError>> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            if let Some(ev) = self.parse_line(&line) {
                out.push(ev);
            }
        }
        out
    }

    pub fn finish(&mut self) -> Vec<Result<StreamDelta, LlmError>> {
        let rest = std::mem::take(&mut self.buf);
        let out: Vec<_> = self.parse_line(&rest).into_iter().collect();
        self.done = true;
        out
    }

    fn parse_line(&mut self, raw: &[u8]) -> Option<Result<StreamDelta, LlmError>> {
        if self.done {
            return None;
        }
        let line = String::from_utf8_lossy(raw);
        let line = line.trim();
        if line.is_empty() || line.starts_with(':') {
            return None;
        }
        let payload = line.strip_prefix("data:")?.trim();
        if payload == "[DONE]" {
            self.done = true;
            return None;
        }
        let chunk: ChatChunk = match serde_json::from_str(payload) {
            Ok(c) => c,
            Err(e) => return Some(Err(LlmError::Sse(e))),
        };
        let choice = chunk.choices.into_iter().next()?;
        let tool_calls = choice
            .delta
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| {
                let (name, arguments) = match tc.function {
                    Some(f) => (f.name, f.arguments),
                    None => (None, None),
                };
                ToolCallDelta {
                    index: tc.index,
                    id: tc.id,
                    name,
                    arguments,
                }
            })
            .collect();
        Some(Ok(StreamDelta {
            content: choice.delta.content,
            tool_calls,
        }))
    }
}

/// Aggregates streamed deltas into a final assistant message.
/// Tool call fragments are concatenated in arrival order, grouped by `index`.
#[derive(Debug, Default)]
pub struct DeltaAggregator {
    content: String,
    tool_calls: BTreeMap<usize, ToolCallBuild>,
}

#[derive(Debug, Default)]
struct ToolCallBuild {
    id: Option<String>,
    name: String,
    arguments: String,
}

impl DeltaAggregator {
    pub fn push(&mut self, delta: &StreamDelta) {
        if let Some(content) = &delta.content {
            self.content.push_str(content);
        }
        for tc in &delta.tool_calls {
            let entry = self.tool_calls.entry(tc.index).or_default();
            if let Some(id) = &tc.id {
                entry.id = Some(id.clone());
            }
            if let Some(name) = &tc.name {
                entry.name.push_str(name);
            }
            if let Some(arguments) = &tc.arguments {
                entry.arguments.push_str(arguments);
            }
        }
    }

    pub fn into_message(self) -> Message {
        let tool_calls: Vec<ToolCall> = self
            .tool_calls
            .into_values()
            .map(|b| ToolCall {
                id: b.id.unwrap_or_default(),
                kind: "function".into(),
                function: FunctionCall {
                    name: b.name,
                    arguments: b.arguments,
                },
            })
            .collect();
        Message {
            role: "assistant".into(),
            content: if self.content.is_empty() {
                None
            } else {
                Some(self.content)
            },
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(tool_calls)
            },
            ..Default::default()
        }
    }
}

pub struct LlmClient {
    base_url: String,
    api_key: String,
    model: String,
    headers: Vec<(String, String)>,
    http: reqwest::Client,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [Tool]>,
    stream: bool,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: Message,
}

impl LlmClient {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
        headers: &std::collections::HashMap<String, String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            api_key: api_key.into(),
            model: model.into(),
            headers: headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            http: reqwest::Client::new(),
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    async fn send(
        &self,
        messages: &[Message],
        tools: Option<&[Tool]>,
        stream: bool,
    ) -> Result<reqwest::Response, LlmError> {
        let request = ChatRequest {
            model: &self.model,
            messages,
            tools,
            stream,
        };
        let mut req = self.http.post(self.endpoint()).bearer_auth(&self.api_key);
        for (key, value) in &self.headers {
            req = req.header(key, value);
        }
        let resp = req.json(&request).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(LlmError::Api {
                status: status.as_u16(),
                body,
            });
        }
        Ok(resp)
    }

    pub async fn chat(
        &self,
        messages: &[Message],
        tools: Option<&[Tool]>,
    ) -> Result<Message, LlmError> {
        let resp = self.send(messages, tools, false).await?;
        let parsed: ChatResponse = resp.json().await?;
        parsed
            .choices
            .into_iter()
            .next()
            .map(|c| c.message)
            .ok_or(LlmError::EmptyResponse)
    }

    pub async fn chat_stream(
        &self,
        messages: &[Message],
        tools: Option<&[Tool]>,
    ) -> Result<BoxStream<'static, Result<StreamDelta, LlmError>>, LlmError> {
        let resp = self.send(messages, tools, true).await?;
        let state = (
            resp.bytes_stream(),
            SseParser::new(),
            VecDeque::new(),
            false,
        );
        let stream = futures_util::stream::unfold(
            state,
            |(mut bytes, mut parser, mut pending, mut failed)| async move {
                loop {
                    if let Some(ev) = pending.pop_front() {
                        return Some((ev, (bytes, parser, pending, failed)));
                    }
                    if failed || parser.is_done() {
                        return None;
                    }
                    match bytes.next().await {
                        Some(Ok(chunk)) => pending.extend(parser.feed(&chunk)),
                        Some(Err(e)) => {
                            failed = true;
                            pending.push_back(Err(LlmError::Http(e)));
                        }
                        None => pending.extend(parser.finish()),
                    }
                }
            },
        );
        Ok(Box::pin(stream))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(events: Vec<Result<StreamDelta, LlmError>>) -> Vec<StreamDelta> {
        events.into_iter().map(Result::unwrap).collect()
    }

    #[test]
    fn parses_content_deltas_split_across_feeds() {
        let mut p = SseParser::new();
        assert!(
            p.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"Hel")
                .is_empty()
        );
        let events = collect(
            p.feed(b"lo\"}}]}\n\ndata: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}\n"),
        );
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].content.as_deref(), Some("Hello"));
        assert_eq!(events[1].content.as_deref(), Some(" world"));
        assert!(!p.is_done());
    }

    #[test]
    fn done_marker_terminates_stream() {
        let mut p = SseParser::new();
        assert!(p.feed(b"data: [DONE]\n").is_empty());
        assert!(p.is_done());
        let events = p.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"late\"}}]}\n");
        assert!(events.is_empty());
    }

    #[test]
    fn finish_flushes_trailing_partial_line() {
        let mut p = SseParser::new();
        p.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n");
        let events = collect(p.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"y\"}}]}"));
        assert!(events.is_empty());
        let events = collect(p.finish());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].content.as_deref(), Some("y"));
    }

    #[test]
    fn skips_comments_and_empty_lines() {
        let mut p = SseParser::new();
        let events = p.feed(b": ping\n\n  \n");
        assert!(events.is_empty());
    }

    #[test]
    fn malformed_payload_yields_error() {
        let mut p = SseParser::new();
        let events = p.feed(b"data: {not json}\n");
        assert_eq!(events.len(), 1);
        assert!(matches!(events[0], Err(LlmError::Sse(_))));
    }

    fn sse(value: serde_json::Value) -> String {
        format!("data: {value}\n")
    }

    fn chunk(delta: serde_json::Value) -> serde_json::Value {
        serde_json::json!({"choices": [{"delta": delta}]})
    }

    #[test]
    fn tool_call_deltas_aggregate_by_index() {
        let mut p = SseParser::new();
        let mut agg = DeltaAggregator::default();
        let fixture = [
            sse(chunk(serde_json::json!({"tool_calls": [{"index": 0, "id": "call_1", "function": {"name": "memory_search", "arguments": "{\"que"}}]}))),
            sse(chunk(serde_json::json!({"tool_calls": [{"index": 0, "function": {"arguments": "ry\":\"rust\"}"}}]}))),
            sse(chunk(serde_json::json!({"tool_calls": [{"index": 1, "id": "call_2", "function": {"name": "memory_read", "arguments": "{\"path\":\"a.md\"}"}}]}))),
            "data: [DONE]\n".to_string(),
        ]
        .concat();
        for ev in p.feed(fixture.as_bytes()) {
            agg.push(&ev.unwrap());
        }
        let msg = agg.into_message();
        assert_eq!(msg.role, "assistant");
        assert_eq!(msg.content, None);
        let calls = msg.tool_calls.unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].function.name, "memory_search");
        assert_eq!(calls[0].function.arguments, "{\"query\":\"rust\"}");
        assert_eq!(calls[1].id, "call_2");
        assert_eq!(calls[1].function.name, "memory_read");
        assert_eq!(calls[1].function.arguments, "{\"path\":\"a.md\"}");
    }

    #[test]
    fn mixed_content_and_tool_calls() {
        let mut p = SseParser::new();
        let mut agg = DeltaAggregator::default();
        let fixture = [
            sse(chunk(serde_json::json!({"content": "thinking "}))),
            sse(chunk(serde_json::json!({"content": "...", "tool_calls": [{"index": 0, "id": "c", "function": {"name": "f", "arguments": "{}"}}]}))),
        ]
        .concat();
        for ev in p.feed(fixture.as_bytes()) {
            agg.push(&ev.unwrap());
        }
        let msg = agg.into_message();
        assert_eq!(msg.content.as_deref(), Some("thinking ..."));
        assert_eq!(msg.tool_calls.unwrap().len(), 1);
    }

    #[test]
    fn message_serializes_tool_result() {
        let msg = Message::tool("call_1", "result body");
        let json = serde_json::to_value(&msg).unwrap();
        assert_eq!(json["role"], "tool");
        assert_eq!(json["tool_call_id"], "call_1");
        assert_eq!(json["content"], "result body");
        assert!(json.get("tool_calls").is_none());
    }

    #[test]
    fn request_serializes_tools() {
        let tools = vec![Tool::function(
            "memory_search",
            "search memory",
            serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}}),
        )];
        let request = ChatRequest {
            model: "m",
            messages: &[Message::user("hi")],
            tools: Some(&tools),
            stream: false,
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(json["tools"][0]["function"]["name"], "memory_search");
        assert_eq!(json["stream"], false);
    }
}
