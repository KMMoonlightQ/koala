use crate::llm::{LlmClient, LlmError, Message};

/// Recent messages kept verbatim through compaction.
pub const KEEP_RECENT: usize = 4;
const MAX_SUMMARY_INPUT_BYTES: usize = 20_000;

/// Replace all but the last KEEP_RECENT messages with an LLM summary.
/// Returns true when compaction happened. Failure keeps history untouched.
pub async fn compact(llm: &LlmClient, history: &mut Vec<Message>) -> Result<bool, LlmError> {
    compact_keeping(llm, history, KEEP_RECENT, usize::MAX).await
}

pub async fn compact_keeping(
    llm: &LlmClient,
    history: &mut Vec<Message>,
    keep: usize,
    budget: usize,
) -> Result<bool, LlmError> {
    let Some(mut split) = history.len().checked_sub(keep).filter(|n| *n > 1) else {
        return Ok(false);
    };
    // Keep each assistant tool call and all its results on the same side.
    while split > 0 && history[split].role == "tool" {
        split -= 1;
    }
    if split == 0 {
        return Ok(false);
    }
    let mut transcript = String::new();
    for m in &history[..split] {
        if let Some(calls) = &m.tool_calls {
            for call in calls {
                transcript.push_str(&format!(
                    "tool call {}: {} {}\n\n",
                    call.id, call.function.name, call.function.arguments
                ));
            }
        }
        let content = m.display_content();
        if content.is_empty() {
            continue;
        }
        transcript.push_str(&m.role);
        transcript.push_str(": ");
        transcript.push_str(&content);
        transcript.push_str("\n\n");
    }
    // Summarize every byte before replacing any history. A single long message
    // can span batches; UTF-8 characters are never split.
    let mut remaining = transcript.as_str();
    let mut summaries = Vec::new();
    while !remaining.is_empty() {
        let mut end = remaining.len().min(MAX_SUMMARY_INPUT_BYTES);
        let (rest, prompt) = loop {
            while !remaining.is_char_boundary(end) {
                end -= 1;
            }
            if end == 0 {
                return Ok(false);
            }
            let (batch, rest) = remaining.split_at(end);
            let prompt = format!(
                "把以下对话历史片段压缩成一份紧凑的摘要，保留：用户的偏好与约束、做过的决定、\
                 进行中的任务状态、后续要用到的事实。片段可能从一条消息中间开始或结束。直接输出摘要文本，不要解释。\n\n{batch}"
            );
            if serde_json::to_vec(&[Message::user(&prompt)])
                .expect("serializable summary")
                .len()
                .saturating_add(4096)
                <= budget
            {
                break (rest, prompt);
            }
            end /= 2;
        };
        let summary = llm
            .chat(&[Message::user(prompt)], None)
            .await?
            .content
            .unwrap_or_default();
        if summary.trim().is_empty() {
            return Ok(false);
        }
        summaries.push(summary);
        remaining = rest;
    }
    if summaries.is_empty() {
        return Ok(false);
    }
    let summary = summaries.join("\n\n");
    let mut compacted = vec![
        Message::user(format!("[早前对话摘要]\n{summary}")),
        Message::assistant("了解，我会基于这些背景继续。"),
    ];
    // Text summaries cannot substitute for visual evidence. Retain the original
    // image questions so future turns still see their pixels and associations.
    compacted.extend(
        history[..split]
            .iter()
            .filter(|m| !m.images.is_empty())
            .cloned(),
    );
    compacted.extend_from_slice(&history[split..]);
    *history = compacted;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn compaction_retains_old_images_with_their_original_question() {
        let mock = crate::test_support::MockLlm::start(vec![crate::test_support::reply(
            Message::assistant("summary"),
        )])
        .await;
        let image: Message = serde_json::from_value(serde_json::json!({"role":"user", "content":"what is this?", "images":[{"data_url":"data:image/png;base64,AQID", "width":1, "height":1}]})).unwrap();
        let mut history = vec![
            image,
            Message::assistant("a diagram"),
            Message::user("2"),
            Message::assistant("ok"),
            Message::user("3"),
            Message::assistant("ok"),
        ];
        assert!(compact(&mock.client, &mut history).await.unwrap());
        assert!(
            history
                .iter()
                .any(|m| !m.images.is_empty() && m.content.as_deref() == Some("what is this?"))
        );
    }

    #[tokio::test]
    async fn compaction_keeps_tool_calls_with_their_results() {
        let mock = crate::test_support::MockLlm::start(vec![crate::test_support::reply(
            Message::assistant("summary"),
        )])
        .await;
        let call: Message = serde_json::from_value(serde_json::json!({"role":"assistant","tool_calls":[{"id":"a","type":"function","function":{"name":"read","arguments":"{}"}},{"id":"b","type":"function","function":{"name":"read","arguments":"{}"}}]})).unwrap();
        let mut history = vec![
            Message::user("old"),
            Message::assistant("old reply"),
            Message::user("work"),
            call,
            Message::tool("a", "result a"),
            Message::tool("b", "result b"),
            Message::assistant("done"),
            Message::user("next"),
        ];
        assert!(compact(&mock.client, &mut history).await.unwrap());
        assert!(history[2].tool_calls.is_some());
        assert_eq!(history[3].tool_call_id.as_deref(), Some("a"));
        assert_eq!(history[4].tool_call_id.as_deref(), Some("b"));
    }

    #[tokio::test]
    async fn long_unicode_history_is_compacted_without_panicking() {
        let mock = crate::test_support::MockLlm::start(vec![
            crate::test_support::reply(
                Message::assistant("summary"),
            );
            2
        ])
        .await;
        let mut history = vec![
            Message::user("中".repeat(7000)),
            Message::assistant("ok"),
            Message::user("2"),
            Message::assistant("ok"),
            Message::user("3"),
            Message::assistant("ok"),
        ];
        assert!(compact(&mock.client, &mut history).await.unwrap());
        assert!(history[0].content.as_ref().unwrap().contains("summary"));
    }

    #[tokio::test]
    async fn every_old_message_reaches_summary_and_recent_messages_stay_verbatim() {
        use crate::test_support::{MockLlm, reply};
        let mut mock = MockLlm::start(vec![
            reply(Message::assistant("prefix summary")),
            reply(Message::assistant("CRITICAL_MIDDLE_FACT")),
        ])
        .await;
        let mut history = vec![
            Message::user("中".repeat(7000)),
            Message::assistant("first answer"),
            Message::user("CRITICAL_MIDDLE_FACT"),
            Message::assistant("ack"),
            Message::user("recent1"),
            Message::assistant("ok"),
            Message::user("recent2"),
            Message::assistant("ok"),
        ];
        assert!(compact(&mock.client, &mut history).await.unwrap());
        let first = mock.request().await;
        let second = mock.request().await;
        assert!(!first.to_string().contains("CRITICAL_MIDDLE_FACT"));
        assert!(second.to_string().contains("CRITICAL_MIDDLE_FACT"));
        let sent_history = [&first, &second]
            .into_iter()
            .map(|request| {
                request["messages"][0]["content"]
                    .as_str()
                    .unwrap()
                    .split_once("\n\n")
                    .unwrap()
                    .1
            })
            .collect::<String>();
        assert_eq!(
            sent_history,
            format!(
                "user: {}\n\nassistant: first answer\n\nuser: CRITICAL_MIDDLE_FACT\n\nassistant: ack\n\n",
                "中".repeat(7000)
            )
        );
        assert!(
            history[0]
                .content
                .as_ref()
                .unwrap()
                .contains("CRITICAL_MIDDLE_FACT")
        );
        assert_eq!(history[2].content.as_deref(), Some("recent1"));
        assert_eq!(history[4].content.as_deref(), Some("recent2"));
    }

    #[tokio::test]
    async fn later_batch_error_or_empty_summary_keeps_all_history() {
        use crate::test_support::{MockLlm, reply};
        for response in [(500, "failure".into()), reply(Message::assistant(""))] {
            let mock =
                MockLlm::start(vec![reply(Message::assistant("prefix summary")), response]).await;
            let mut history = vec![
                Message::user("x".repeat(21000)),
                Message::assistant("old"),
                Message::user("1"),
                Message::assistant("2"),
                Message::user("3"),
                Message::assistant("4"),
            ];
            let original = serde_json::to_value(&history).unwrap();
            assert!(!compact(&mock.client, &mut history).await.unwrap_or(false));
            assert_eq!(serde_json::to_value(&history).unwrap(), original);
        }
    }
}
