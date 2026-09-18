#![allow(dead_code)]
// Scripted local HTTP server for exercising real model request/response paths.
use crate::llm::{LlmClient, Message};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub struct MockLlm {
    pub client: LlmClient,
    pub url: String,
    requests: tokio::sync::mpsc::UnboundedReceiver<Value>,
    server: tokio::task::JoinHandle<()>,
}

impl MockLlm {
    pub async fn start(responses: Vec<(u16, String)>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, requests) = tokio::sync::mpsc::unbounded_channel();
        let server = tokio::spawn(async move {
            for (status, body) in responses {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut data = Vec::new();
                loop {
                    let mut buf = [0; 4096];
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0, "request closed before body arrived");
                    data.extend_from_slice(&buf[..n]);
                    if let Some(end) = data.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&data[..end]).to_lowercase();
                        let len: usize = headers
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length: "))
                            .unwrap()
                            .parse()
                            .unwrap();
                        if data.len() >= end + 4 + len {
                            tx.send(serde_json::from_slice(&data[end + 4..end + 4 + len]).unwrap())
                                .unwrap();
                            break;
                        }
                    }
                }
                let kind = if body.starts_with("data:") {
                    "text/event-stream"
                } else {
                    "application/json"
                };
                socket.write_all(format!("HTTP/1.1 {status} Mock\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
        });
        Self {
            client: LlmClient::new(&url, "", "test", &Default::default()),
            url,
            requests,
            server,
        }
    }

    pub async fn request(&mut self) -> Value {
        tokio::time::timeout(std::time::Duration::from_secs(3), self.requests.recv())
            .await
            .unwrap()
            .unwrap()
    }
}

impl Drop for MockLlm {
    fn drop(&mut self) {
        self.server.abort();
    }
}

pub fn reply(message: Message) -> (u16, String) {
    (
        200,
        serde_json::json!({"choices": [{"message": message}]}).to_string(),
    )
}

pub fn stream(delta: Value) -> (u16, String) {
    (
        200,
        format!(
            "data: {}\n\ndata: [DONE]\n\n",
            serde_json::json!({"choices": [{"delta": delta}]})
        ),
    )
}
