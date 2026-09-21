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
    #[error("stream error: {0}")]
    Stream(String),
    #[error("stream ended before the model completed its response")]
    UnexpectedEof,
    #[error("response contained no choices")]
    EmptyResponse,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageAttachment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Persisted with the conversation, independently of the clipboard lifetime.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ImageAttachment {
    pub data_url: String,
    pub width: u32,
    pub height: u32,
}

/// Convert internal attachments only at the provider boundary. Legacy text
/// messages keep their original wire shape and journals keep image metadata.
fn serialize_messages<S: serde::Serializer>(
    messages: &[Message],
    serializer: S,
) -> Result<S::Ok, S::Error> {
    use serde::ser::SerializeSeq;
    let mut seq = serializer.serialize_seq(Some(messages.len()))?;
    for message in messages {
        let mut value = serde_json::to_value(message).map_err(serde::ser::Error::custom)?;
        if !message.images.is_empty() {
            value.as_object_mut().unwrap().remove("images");
            let mut parts = Vec::new();
            if let Some(text) = &message.content
                && !text.is_empty()
            {
                parts.push(serde_json::json!({"type":"text", "text":text}));
            }
            parts.extend(message.images.iter().map(|image| {
                serde_json::json!({
                    "type":"image_url", "image_url":{"url":image.data_url}
                })
            }));
            value["content"] = serde_json::Value::Array(parts);
        }
        seq.serialize_element(&value)?;
    }
    seq.end()
}

/// Text retains the existing serialized-byte budget. Images use a conservative
/// dimension-based allowance, not their Base64 transport size. Providers' actual
/// image token accounting varies; usage returned by the provider is authoritative.
pub fn context_size(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|message| {
            let text = Message {
                images: Vec::new(),
                role: message.role.clone(),
                content: message.content.clone(),
                reasoning_content: message.reasoning_content.clone(),
                tool_calls: message.tool_calls.clone(),
                tool_call_id: message.tool_call_id.clone(),
                name: message.name.clone(),
            };
            let bytes = serde_json::to_vec(&text)
                .expect("serializable message")
                .len();
            message.images.iter().fold(bytes + 1, |size, image| {
                // Estimate a vision input scaled to a 2048px long edge. The
                // original pixels are sent unchanged; this is only a heuristic.
                let long_edge = u64::from(image.width.max(image.height)).max(2048);
                let width = (u64::from(image.width) * 2048).div_ceil(long_edge);
                let height = (u64::from(image.height) * 2048).div_ceil(long_edge);
                let tiles = width.div_ceil(512) * height.div_ceil(512);
                size.saturating_add(
                    (1024 + tiles.saturating_mul(1024)).min(usize::MAX as u64) as usize
                )
            })
        })
        .fold(1usize, usize::saturating_add)
}

impl Message {
    pub fn user_with_images(content: impl Into<String>, images: Vec<ImageAttachment>) -> Self {
        Self {
            images,
            ..Self::user(content)
        }
    }

    pub fn display_content(&self) -> String {
        let mut text = self.content.clone().unwrap_or_default();
        for (index, image) in self.images.iter().enumerate() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&format!(
                "[Image {} · {}×{}]",
                index + 1,
                image.width,
                image.height
            ));
        }
        text
    }

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

pub use koala_extension_api::{Tool, ToolFunction};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamDelta {
    pub usage: Option<TokenUsage>,
    pub content: Option<String>,
    pub reasoning_content: Option<String>,
    pub tool_calls: Vec<ToolCallDelta>,
}

/// Usage for one request, including cached input tokens in prompt_tokens.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TokenUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
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
    usage: Option<TokenUsage>,
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    error: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct ChunkChoice {
    delta: ChunkDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ChunkDelta {
    #[serde(default)]
    reasoning_content: Option<String>,
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
    completed: bool,
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
        let mut out: Vec<_> = self.parse_line(&rest).into_iter().collect();
        if !self.done && !self.completed {
            out.push(Err(LlmError::UnexpectedEof));
        }
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
            Err(e) => {
                self.done = true;
                return Some(Err(LlmError::Sse(e)));
            }
        };
        if let Some(error) = chunk.error {
            self.done = true;
            return Some(Err(LlmError::Stream(error.to_string())));
        }
        let Some(choice) = chunk.choices.into_iter().next() else {
            return chunk.usage.map(|usage| {
                Ok(StreamDelta {
                    usage: Some(usage),
                    ..StreamDelta::default()
                })
            });
        };
        if let Some(reason) = choice.finish_reason {
            if !matches!(reason.as_str(), "stop" | "tool_calls" | "function_call") {
                self.done = true;
                return Some(Err(LlmError::Stream(format!("response stopped: {reason}"))));
            }
            self.completed = true;
        }
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
            usage: chunk.usage,
            content: choice.delta.content,
            reasoning_content: choice.delta.reasoning_content,
            tool_calls,
        }))
    }
}

/// Aggregates streamed deltas into a final assistant message.
/// Tool call fragments are concatenated in arrival order, grouped by `index`.
#[derive(Debug, Default)]
pub struct DeltaAggregator {
    reasoning_content: Option<String>,
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
        if let Some(reasoning) = &delta.reasoning_content {
            self.reasoning_content
                .get_or_insert_with(String::new)
                .push_str(reasoning);
        }
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
            reasoning_content: self.reasoning_content,
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
    selection: std::sync::RwLock<(String, Option<String>)>,
    headers: Vec<(String, String)>,
    http: reqwest::Client,
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<String>,
    #[serde(serialize_with = "serialize_messages")]
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [Tool]>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
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
            selection: std::sync::RwLock::new((model.into(), None)),
            headers: headers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
            http: reqwest::Client::new(),
        }
    }

    pub fn reasoning_effort(&self) -> Option<String> {
        self.selection.read().unwrap().1.clone()
    }

    pub fn set_reasoning_effort(&self, effort: Option<String>) {
        self.selection.write().unwrap().1 = effort;
    }

    pub fn model(&self) -> String {
        self.selection.read().unwrap().0.clone()
    }

    pub fn select_model(&self, model: String, effort: Option<String>) {
        *self.selection.write().unwrap() = (model, effort);
    }

    fn endpoint(&self) -> String {
        format!("{}/chat/completions", self.base_url.trim_end_matches('/'))
    }

    /// DeepSeek requires the field even for synthetic/legacy assistant messages.
    /// Preserve returned reasoning verbatim; an empty value marks unavailable
    /// reasoning, rather than inventing it or rewriting persisted history.
    fn request_messages<'a>(
        &self,
        model: &str,
        messages: &'a [Message],
    ) -> std::borrow::Cow<'a, [Message]> {
        let deepseek = model.to_ascii_lowercase().contains("deepseek")
            || reqwest::Url::parse(&self.base_url)
                .ok()
                .and_then(|url| url.host_str().map(str::to_owned))
                .is_some_and(|host| host == "api.deepseek.com");
        if !deepseek
            || !messages
                .iter()
                .any(|m| m.role == "assistant" && m.reasoning_content.is_none())
        {
            return std::borrow::Cow::Borrowed(messages);
        }
        let mut prepared = messages.to_vec();
        for message in &mut prepared {
            if message.role == "assistant" && message.reasoning_content.is_none() {
                message.reasoning_content = Some(String::new());
            }
        }
        std::borrow::Cow::Owned(prepared)
    }

    async fn send(
        &self,
        messages: &[Message],
        tools: Option<&[Tool]>,
        stream: bool,
    ) -> Result<reqwest::Response, LlmError> {
        let (model, reasoning_effort) = self.selection.read().unwrap().clone();
        let messages = self.request_messages(&model, messages);
        let request = ChatRequest {
            model: &model,
            reasoning_effort,
            messages: &messages,
            tools,
            stream,
            stream_options: stream.then_some(StreamOptions {
                include_usage: true,
            }),
        };
        let mut req = self.http.post(self.endpoint()).bearer_auth(&self.api_key);
        for (key, value) in &self.headers {
            req = req.header(key, value);
        }
        let resp = req.json(&request).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let mut body = resp.text().await.unwrap_or_default();
            if matches!(status.as_u16(), 400 | 415 | 422)
                && messages.iter().any(|m| !m.images.is_empty())
            {
                body = format!(
                    "Image request rejected; select a vision-capable model or check the provider's image limits. Provider error: {body}"
                );
            }
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
    fn stream_error_is_not_silently_ignored() {
        let mut parser = SseParser::new();
        let events =
            parser.feed(b"data: {\"error\":{\"message\":\"upstream failed\"}}\n\ndata: [DONE]\n");
        assert!(events.iter().any(Result::is_err));
    }

    #[test]
    fn incomplete_stream_fails_at_eof() {
        let mut parser = SseParser::new();
        parser.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n");
        assert!(parser.finish().iter().any(Result::is_err));
    }

    #[test]
    fn explicit_finish_reason_allows_eof_without_done() {
        let mut parser = SseParser::new();
        parser.feed(b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n");
        assert!(parser.finish().iter().all(Result::is_ok));
    }

    #[test]
    fn length_limit_does_not_commit_truncated_tool_arguments() {
        let mut parser = SseParser::new();
        let events = parser.feed(
            b"data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\ndata: [DONE]\n",
        );
        assert!(events.iter().any(Result::is_err));
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
        let events = collect(p.feed(
            b"data: {\"choices\":[{\"delta\":{\"content\":\"y\"},\"finish_reason\":\"stop\"}]}",
        ));
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
    fn reasoning_compatibility_is_scoped_to_deepseek_and_preserves_existing_values() {
        let client = LlmClient::new(
            "https://gateway.example/v1",
            "",
            "test",
            &Default::default(),
        );
        let mut original = Message::assistant("answer");
        original.reasoning_content = Some("full original reasoning".into());
        let messages = [
            Message::user("question"),
            Message::assistant("synthetic"),
            original,
        ];
        let generic = client.request_messages("other-model", &messages);
        assert!(generic[1].reasoning_content.is_none());
        let prepared = client.request_messages("vendor/DeepSeek-v4-flash", &messages);
        assert_eq!(prepared[1].reasoning_content.as_deref(), Some(""));
        assert_eq!(prepared[2].reasoning_content, messages[2].reasoning_content);
        assert!(prepared[0].reasoning_content.is_none());
        let native = LlmClient::new(
            "https://api.deepseek.com/v1",
            "",
            "alias",
            &Default::default(),
        );
        assert_eq!(
            native.request_messages("alias", &messages)[1]
                .reasoning_content
                .as_deref(),
            Some("")
        );
        assert!(messages[1].reasoning_content.is_none());
    }

    #[test]
    fn reasoning_content_survives_tool_round_trip() {
        let mut parser = SseParser::new();
        let mut aggregator = DeltaAggregator::default();
        let fixture = [
            sse(chunk(serde_json::json!({"reasoning_content":"think "}))),
            sse(chunk(serde_json::json!({"reasoning_content":"more"}))),
            sse(chunk(serde_json::json!({"tool_calls":[{"index":0,"id":"c1","function":{"name":"read","arguments":"{}"}}]}))),
            "data: [DONE]\n".into(),
        ].concat();
        for delta in parser.feed(fixture.as_bytes()) {
            aggregator.push(&delta.unwrap());
        }
        let messages = vec![aggregator.into_message(), Message::tool("c1", "result")];
        let request = serde_json::to_value(messages).unwrap();
        assert_eq!(request[0]["reasoning_content"], "think more");
        assert_eq!(request[0]["tool_calls"][0]["id"], "c1");
        assert!(request[1].get("reasoning_content").is_none());
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
            reasoning_effort: None,
            messages: &[Message::user("hi")],
            tools: Some(&tools),
            stream: false,
            stream_options: None,
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(json["tools"][0]["function"]["name"], "memory_search");
        assert_eq!(json["stream"], false);
        assert!(json.get("stream_options").is_none());
        assert!(json.get("reasoning_effort").is_none());
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;

    #[test]
    fn image_budget_accepts_a_4k_screenshot_without_counting_full_resolution_tiles() {
        let message = Message::user_with_images(
            "describe",
            vec![ImageAttachment {
                data_url: "data:image/png;base64,AQID".into(),
                width: 3840,
                height: 2160,
            }],
        );
        assert!(context_size(&[message]) + 4096 < 40_000);
    }

    #[test]
    fn image_context_estimate_ignores_base64_transport_length() {
        let plain = [Message::user("hello")];
        assert_eq!(
            context_size(&plain),
            serde_json::to_vec(&plain).unwrap().len()
        );
        let mut message = Message::user_with_images(
            "look",
            vec![ImageAttachment {
                data_url: "data:image/png;base64,AQID".into(),
                width: 1920,
                height: 1080,
            }],
        );
        let before = context_size(&[message.clone()]);
        message.images[0].data_url.push_str(&"A".repeat(1_000_000));
        assert_eq!(context_size(&[message]), before);
        assert!(before < 40_000);
    }

    #[test]
    fn image_request_sends_content_parts_and_preserves_text_requests() {
        let image_message: Message = serde_json::from_value(serde_json::json!({
            "role": "user", "content": "Describe this",
            "images": [{"data_url": "data:image/png;base64,AQID", "width": 2, "height": 3}]
        }))
        .unwrap();
        let messages = [Message::user("hello"), image_message];
        let request = ChatRequest {
            model: "vision",
            reasoning_effort: None,
            messages: &messages,
            tools: None,
            stream: true,
            stream_options: None,
        };
        let json = serde_json::to_value(request).unwrap();
        assert_eq!(json["messages"][0]["content"], "hello");
        assert_eq!(
            json["messages"][1]["content"],
            serde_json::json!([
                {"type":"text", "text":"Describe this"},
                {"type":"image_url", "image_url":{"url":"data:image/png;base64,AQID"}}
            ])
        );
        assert!(json["messages"][1].get("images").is_none());
        let saved = serde_json::to_value(&messages[1]).unwrap();
        assert_eq!(saved["images"][0]["width"], 2);
    }
}
