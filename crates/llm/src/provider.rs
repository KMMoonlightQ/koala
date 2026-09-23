use super::{
    BoxedDeltaStream, Connection, FunctionCall, LlmError, Message, StreamDelta, TokenUsage, Tool,
    ToolCall, ToolCallDelta,
};
use futures_util::StreamExt;
use genai::{
    Client, ModelIden, ServiceTarget,
    adapter::AdapterKind,
    chat::{
        ChatMessage, ChatOptions, ChatRequest, ChatStreamEvent, ContentPart, MessageContent,
        ReasoningEffort, StopReason,
    },
    resolver::{AuthData, Endpoint, ProviderConfig},
};

fn kind(connection: &Connection) -> Result<AdapterKind, LlmError> {
    AdapterKind::from_lower_str(if connection.provider == "openai_compatible" {
        "openai"
    } else {
        &connection.provider
    })
    .ok_or_else(|| LlmError::UnsupportedProvider(connection.provider.clone()))
}

fn auth(connection: &Connection) -> AuthData {
    if connection.api_key.is_empty() {
        AuthData::None
    } else {
        AuthData::from_single(connection.api_key.clone())
    }
}

fn endpoint(connection: &Connection) -> Option<Endpoint> {
    (!connection.base_url.is_empty())
        .then(|| Endpoint::from_owned(format!("{}/", connection.base_url.trim_end_matches('/'))))
}

fn client(connection: &Connection) -> Result<Client, LlmError> {
    let auth = auth(connection);
    let custom_endpoint = endpoint(connection);
    Ok(Client::builder()
        .with_adapter_kind(kind(connection)?)
        .with_auth_resolver_fn(move |_model: ModelIden| Ok(Some(auth.clone())))
        .with_service_target_resolver_fn(move |mut target: ServiceTarget| {
            if let Some(endpoint) = &custom_endpoint {
                target.endpoint = endpoint.clone();
            }
            Ok(target)
        })
        .build())
}

pub async fn list_model_names(connection: &Connection) -> Result<Vec<String>, LlmError> {
    let mut config = ProviderConfig::from_auth(auth(connection));
    if let Some(endpoint) = endpoint(connection) {
        config = config.with_endpoint(endpoint);
    }
    Ok(Client::default()
        .all_model_names(kind(connection)?, config)
        .await?)
}

fn request(messages: &[Message], tools: Option<&[Tool]>) -> Result<ChatRequest, LlmError> {
    let mut converted = Vec::with_capacity(messages.len());
    for message in messages {
        let mut parts = Vec::new();
        for signature in &message.thought_signatures {
            parts.push(ContentPart::ThoughtSignature(signature.clone()));
        }
        if let Some(content) = &message.content {
            parts.push(ContentPart::Text(content.clone()));
        }
        for image in &message.images {
            let data = image
                .data_url
                .strip_prefix("data:")
                .and_then(|s| s.split_once(";base64,"));
            let (mime, base64) =
                data.ok_or_else(|| LlmError::Stream("invalid image data URL".into()))?;
            parts.push(ContentPart::from_binary_base64(
                mime.to_owned(),
                base64.to_owned(),
                None,
            ));
        }
        if let Some(reasoning) = &message.reasoning_content {
            parts.push(ContentPart::ReasoningContent(reasoning.clone()));
        }
        if let Some(calls) = &message.tool_calls {
            for call in calls {
                parts.push(ContentPart::ToolCall(genai::chat::ToolCall {
                    call_id: call.id.clone(),
                    fn_name: call.function.name.clone(),
                    fn_arguments: serde_json::from_str(&call.function.arguments)?,
                    thought_signatures: None,
                }));
            }
        }
        if message.role == "tool" {
            let id = message
                .tool_call_id
                .as_deref()
                .ok_or_else(|| LlmError::Stream("tool response has no call ID".into()))?;
            let mut response =
                genai::chat::ToolResponse::new(id, message.content.clone().unwrap_or_default());
            let name = message.name.clone().or_else(|| {
                messages
                    .iter()
                    .filter_map(|prior| prior.tool_calls.as_ref())
                    .flatten()
                    .find(|call| call.id == id)
                    .map(|call| call.function.name.clone())
            });
            if let Some(name) = name {
                response = response.with_fn_name(name);
            }
            converted.push(ChatMessage::tool(response));
        } else {
            let content = MessageContent::from_parts(parts);
            converted.push(match message.role.as_str() {
                "system" => ChatMessage::system(content),
                "user" => ChatMessage::user(content),
                "assistant" => ChatMessage::assistant(content),
                role => return Err(LlmError::Stream(format!("unknown message role: {role}"))),
            });
        }
    }
    let mut request = ChatRequest::new(converted);
    if let Some(tools) = tools {
        request = request.with_tools(tools.iter().map(|tool| {
            genai::chat::Tool::new(tool.function.name.clone())
                .with_description(tool.function.description.clone())
                .with_schema(tool.function.parameters.clone())
        }));
    }
    Ok(request)
}

fn options(connection: &Connection, effort: Option<&str>) -> ChatOptions {
    let mut options = ChatOptions::default().with_capture_usage(true);
    if let Some(effort) = effort.and_then(ReasoningEffort::from_keyword) {
        options = options.with_reasoning_effort(effort);
    }
    if !connection.headers.is_empty() {
        options = options.with_extra_headers(connection.headers.clone());
    }
    options
}

fn tool_call_delta(
    call: genai::chat::ToolCall,
    next_index: &mut usize,
    calls: &mut std::collections::HashMap<String, (usize, String)>,
) -> ToolCallDelta {
    let first = !calls.contains_key(&call.call_id);
    let entry = calls.entry(call.call_id.clone()).or_insert_with(|| {
        let index = *next_index;
        *next_index += 1;
        (index, String::new())
    });
    // genai emits the accumulated JSON string on each Anthropic chunk.
    let arguments = match call.fn_arguments {
        serde_json::Value::String(text) => text,
        value => value.to_string(),
    };
    let fragment = arguments
        .strip_prefix(&entry.1)
        .unwrap_or(&arguments)
        .to_owned();
    entry.1 = arguments;
    ToolCallDelta {
        index: entry.0,
        id: first.then_some(call.call_id),
        name: first.then_some(call.fn_name),
        arguments: Some(fragment),
    }
}

pub async fn chat(
    connection: &Connection,
    model: &str,
    effort: Option<&str>,
    messages: &[Message],
    tools: Option<&[Tool]>,
) -> Result<Message, LlmError> {
    let response = client(connection)?
        .exec_chat(
            ModelIden::new(kind(connection)?, model.to_owned()),
            request(messages, tools)?,
            Some(&options(connection, effort)),
        )
        .await?;
    let content = response.content.texts().join("");
    let mut thought_signatures: Vec<String> = response
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::ThoughtSignature(signature) => Some(signature.clone()),
            _ => None,
        })
        .collect();
    if thought_signatures.is_empty() {
        thought_signatures = response
            .content
            .tool_calls()
            .into_iter()
            .flat_map(|call| call.thought_signatures.clone().unwrap_or_default())
            .collect();
    }
    let tool_calls: Vec<_> = response
        .content
        .tool_calls()
        .into_iter()
        .map(|call| ToolCall {
            id: call.call_id.clone(),
            kind: "function".into(),
            function: FunctionCall {
                name: call.fn_name.clone(),
                arguments: call.fn_arguments.to_string(),
            },
        })
        .collect();
    Ok(Message {
        role: "assistant".into(),
        content: (!content.is_empty()).then_some(content),
        reasoning_content: response.reasoning_content,
        thought_signatures,
        tool_calls: (!tool_calls.is_empty()).then_some(tool_calls),
        ..Default::default()
    })
}

pub async fn chat_stream(
    connection: &Connection,
    model: &str,
    effort: Option<&str>,
    messages: &[Message],
    tools: Option<&[Tool]>,
) -> Result<BoxedDeltaStream, LlmError> {
    let response = client(connection)?
        .exec_chat_stream(
            ModelIden::new(kind(connection)?, model.to_owned()),
            request(messages, tools)?,
            Some(&options(connection, effort)),
        )
        .await?;
    let stream = response
        .stream
        .scan(
            (0usize, std::collections::HashMap::new(), false),
            |(next_index, calls, saw_signatures), event| {
                let result = match event {
                    Ok(ChatStreamEvent::Chunk(chunk)) => Some(Ok(StreamDelta {
                        content: Some(chunk.content),
                        ..Default::default()
                    })),
                    Ok(ChatStreamEvent::ReasoningChunk(chunk)) => Some(Ok(StreamDelta {
                        reasoning_content: Some(chunk.content),
                        ..Default::default()
                    })),
                    Ok(ChatStreamEvent::ThoughtSignatureChunk(chunk)) => {
                        *saw_signatures = true;
                        Some(Ok(StreamDelta {
                            thought_signatures: vec![chunk.content],
                            ..Default::default()
                        }))
                    }
                    Ok(ChatStreamEvent::ToolCallChunk(chunk)) => {
                        let signatures = if *saw_signatures {
                            Vec::new()
                        } else {
                            chunk
                                .tool_call
                                .thought_signatures
                                .clone()
                                .unwrap_or_default()
                        };
                        if !signatures.is_empty() {
                            *saw_signatures = true;
                        }
                        let delta = tool_call_delta(chunk.tool_call, next_index, calls);
                        Some(Ok(StreamDelta {
                            tool_calls: vec![delta],
                            thought_signatures: signatures,
                            ..Default::default()
                        }))
                    }
                    Ok(ChatStreamEvent::End(end)) => {
                        if let Some(reason) = end.captured_stop_reason.as_ref()
                            && !matches!(
                                reason,
                                StopReason::Completed(_)
                                    | StopReason::ToolCall(_)
                                    | StopReason::StopSequence(_)
                            )
                        {
                            Some(Err(LlmError::Stream(format!(
                                "model response stopped: {reason}"
                            ))))
                        } else {
                            end.captured_usage.map(|usage| {
                                Ok(StreamDelta {
                                    usage: Some(TokenUsage {
                                        prompt_tokens: usage.prompt_tokens.unwrap_or(0).max(0)
                                            as u64,
                                        completion_tokens: usage
                                            .completion_tokens
                                            .unwrap_or(0)
                                            .max(0)
                                            as u64,
                                    }),
                                    ..Default::default()
                                })
                            })
                        }
                    }
                    Ok(ChatStreamEvent::Start) => None,
                    Err(error) => Some(Err(error.into())),
                };
                futures_util::future::ready(Some(result))
            },
        )
        .filter_map(futures_util::future::ready);
    Ok(Box::pin(stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_tool_round_trip_preserves_signature_and_function_name() {
        let mut assistant = Message::assistant("");
        assistant.thought_signatures.push("signed-thought".into());
        assistant.tool_calls = Some(vec![ToolCall {
            id: "call-1".into(),
            kind: "function".into(),
            function: FunctionCall {
                name: "lookup".into(),
                arguments: "{}".into(),
            },
        }]);
        let request = request(&[assistant, Message::tool("call-1", "result")], None).unwrap();
        assert!(request.messages[0].content.iter().any(
            |part| matches!(part, ContentPart::ThoughtSignature(value) if value == "signed-thought")
        ));
        assert!(request.messages[1].content.iter().any(|part| matches!(part, ContentPart::ToolResponse(response) if response.fn_name.as_deref() == Some("lookup"))));
    }

    #[test]
    fn accumulated_tool_chunks_become_append_only_deltas() {
        let mut index = 0;
        let mut calls = std::collections::HashMap::new();
        let call = |args: &str| genai::chat::ToolCall {
            call_id: "call-1".into(),
            fn_name: "lookup".into(),
            fn_arguments: serde_json::Value::String(args.into()),
            thought_signatures: None,
        };
        let deltas = ["", "{\"ci", "{\"city\":\"Paris\"}"]
            .into_iter()
            .map(|args| tool_call_delta(call(args), &mut index, &mut calls))
            .collect::<Vec<_>>();
        let mut aggregate = super::super::DeltaAggregator::default();
        for delta in deltas {
            aggregate.push(&StreamDelta {
                tool_calls: vec![delta],
                ..Default::default()
            });
        }
        let message = aggregate.into_message();
        let tool = &message.tool_calls.unwrap()[0];
        assert_eq!(tool.function.name, "lookup");
        assert_eq!(tool.function.arguments, "{\"city\":\"Paris\"}");
    }
}
