use crate::llm::{LlmClient, LlmError, Message};

/// Recent messages kept verbatim through compaction.
pub const KEEP_RECENT: usize = 4;
const MAX_SUMMARY_INPUT_CHARS: usize = 20_000;

/// Rough size estimate (chars ≈ tokens × 4 for mixed CJK/English).
pub fn estimate_chars(history: &[Message]) -> usize {
    history
        .iter()
        .map(|m| m.content.as_deref().unwrap_or("").len())
        .sum()
}

/// Where old history ends and the kept tail begins.
pub fn split_point(len: usize) -> Option<usize> {
    (len > KEEP_RECENT + 1).then(|| len - KEEP_RECENT)
}

/// Replace all but the last KEEP_RECENT messages with an LLM summary.
/// Returns true when compaction happened. Failure keeps history untouched.
pub async fn compact(llm: &LlmClient, history: &mut Vec<Message>) -> Result<bool, LlmError> {
    let Some(split) = split_point(history.len()) else {
        return Ok(false);
    };
    let mut transcript = String::new();
    for m in &history[..split] {
        let content = m.content.as_deref().unwrap_or("");
        if content.is_empty() {
            continue;
        }
        transcript.push_str(&m.role);
        transcript.push_str(": ");
        transcript.push_str(content);
        transcript.push_str("\n\n");
        if transcript.len() > MAX_SUMMARY_INPUT_CHARS {
            let mut end = MAX_SUMMARY_INPUT_CHARS;
            while !transcript.is_char_boundary(end) {
                end -= 1;
            }
            transcript.truncate(end);
            break;
        }
    }
    let prompt = format!(
        "把以下对话历史压缩成一份紧凑的摘要，保留：用户的偏好与约束、做过的决定、\
         进行中的任务状态、后续要用到的事实。直接输出摘要文本，不要解释。\n\n{transcript}"
    );
    let summary = llm
        .chat(&[Message::user(prompt)], None)
        .await?
        .content
        .unwrap_or_default();
    if summary.trim().is_empty() {
        return Ok(false);
    }
    let mut compacted = vec![
        Message::user(format!("[早前对话摘要]\n{summary}")),
        Message::assistant("了解，我会基于这些背景继续。"),
    ];
    compacted.extend_from_slice(&history[split..]);
    *history = compacted;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn long_unicode_history_is_compacted_without_panicking() {
        let mock = crate::test_support::MockLlm::start(vec![crate::test_support::reply(
            Message::assistant("summary"),
        )])
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

    #[test]
    fn split_point_keeps_recent_four() {
        assert_eq!(split_point(0), None);
        assert_eq!(split_point(2), None);
        assert_eq!(split_point(4), None);
        assert_eq!(split_point(5), None);
        assert_eq!(split_point(6), Some(2));
        assert_eq!(split_point(10), Some(6));
    }

    #[test]
    fn estimate_counts_content() {
        let history = vec![Message::user("abcd"), Message::assistant("ef")];
        assert_eq!(estimate_chars(&history), 6);
    }
}
