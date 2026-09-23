use futures_util::StreamExt;
use koala_llm::{DeltaAggregator, LlmClient, Message};
use std::collections::HashMap;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn serve_once(response: &'static str) -> (String, tokio::task::JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}/v1/", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = vec![0; 8192];
        let n = socket.read(&mut bytes).await.unwrap();
        let request = String::from_utf8_lossy(&bytes[..n]).into_owned();
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",
                    if response.starts_with("event:") { "text/event-stream" } else { "application/json" },
                    response.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        request
    });
    (base_url, server)
}

#[tokio::test]
async fn anthropic_login_uses_native_messages_api_with_the_saved_key() {
    let response = r#"{"id":"msg_1","type":"message","role":"assistant","model":"claude-new","content":[{"type":"text","text":"hello"}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":1,"output_tokens":1}}"#;
    let (url, server) = serve_once(response).await;
    let client = LlmClient::with_provider(
        "anthropic",
        &url,
        "secret-from-login",
        "claude-new",
        &HashMap::new(),
    )
    .unwrap();

    let answer = client.chat(&[Message::user("hi")], None).await.unwrap();
    let request = server.await.unwrap();
    assert_eq!(answer.content.as_deref(), Some("hello"));
    assert!(request.starts_with("POST /v1/messages HTTP/1.1"));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("x-api-key: secret-from-login")
    );
}

#[tokio::test]
async fn model_picker_reads_new_ids_from_the_active_provider() {
    let response = r#"{"data":[{"id":"claude-brand-new","type":"model"}],"has_more":false}"#;
    let (url, server) = serve_once(response).await;
    let client = LlmClient::with_provider(
        "anthropic",
        &url,
        "secret-from-login",
        "claude-brand-new",
        &HashMap::new(),
    )
    .unwrap();

    let names = client.list_model_names().await.unwrap();
    let request = server.await.unwrap();
    assert_eq!(names, vec!["claude-brand-new"]);
    assert!(request.starts_with("GET /v1/models HTTP/1.1"));
}

#[tokio::test]
async fn anthropic_streamed_tool_call_has_one_complete_argument_object() {
    const SSE: &str = concat!(
        "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[],\"model\":\"claude-new\",\"stop_reason\":null,\"stop_sequence\":null,\"usage\":{\"input_tokens\":4,\"output_tokens\":0}}}\n\n",
        "event: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"call-1\",\"name\":\"lookup\",\"input\":{}}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"ci\"}}\n\n",
        "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"ty\\\":\\\"Paris\\\"}\"}}\n\n",
        "event: content_block_stop\ndata: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\",\"stop_sequence\":null},\"usage\":{\"output_tokens\":3}}\n\n",
        "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"
    );
    let (url, server) = serve_once(SSE).await;
    let client =
        LlmClient::with_provider("anthropic", &url, "secret", "claude-new", &HashMap::new())
            .unwrap();
    let mut stream = client
        .chat_stream(&[Message::user("look up Paris")], None)
        .await
        .unwrap();
    let mut aggregate = DeltaAggregator::default();
    while let Some(delta) = stream.next().await {
        aggregate.push(&delta.unwrap());
    }
    let message = aggregate.into_message();
    let calls = message.tool_calls.unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].function.name, "lookup");
    assert_eq!(calls[0].function.arguments, "{\"city\":\"Paris\"}");
    assert!(
        server
            .await
            .unwrap()
            .starts_with("POST /v1/messages HTTP/1.1")
    );
}
