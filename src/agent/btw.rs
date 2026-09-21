//! Temporary, conversational asides. No tools, hooks, journals or memory writes.
use super::event::{EventSender, SessionCommand, UiEvent};
use super::{AgentError, SharedState, retry};
use crate::i18n::{self, Key};
use crate::llm::{DeltaAggregator, Message};
use futures_util::StreamExt;
use std::sync::Arc;
use tokio::sync::mpsc;

pub(super) async fn run(
    shared: Arc<SharedState>,
    mut history: Vec<Message>,
    mut commands: mpsc::UnboundedReceiver<SessionCommand>,
    events: EventSender,
) {
    history.insert(0, Message::system(
        "You are in a temporary side conversation. Use the preceding main conversation as context, \
         but answer only the user's new side questions. Do not continue the main task. \
         No tools are available in this conversation."
    ));
    while let Some(command) = commands.recv().await {
        let input = match command {
            SessionCommand::Submit(text) if !text.trim().is_empty() => Message::user(text),
            SessionCommand::SubmitWithImages { text, images }
                if !text.trim().is_empty() || !images.is_empty() =>
            {
                if let Err(error) = crate::images::validate(&images) {
                    let _ = events.send(UiEvent::Error(error));
                    let _ = events.send(UiEvent::Done);
                    continue;
                }
                Message::user_with_images(text, images)
            }
            SessionCommand::Shutdown => break,
            _ => continue,
        };
        let mut request = history.clone();
        request.push(input.clone());
        let response = reply(&shared, &request, &events);
        tokio::pin!(response);
        loop {
            tokio::select! {
                biased;
                command = commands.recv() => match command {
                    None | Some(SessionCommand::Shutdown) => return,
                    Some(SessionCommand::Cancel) => {
                        let _ = events.send(UiEvent::Cancelled);
                        break;
                    }
                    _ => {
                        let _ = events.send(UiEvent::Note(i18n::text(
                            shared.lang.get(), Key::NoteBusyInterruptFirst).into()));
                    }
                },
                result = &mut response => {
                    match result {
                        Ok(answer) => {
                            history.push(input);
                            history.push(answer);
                        }
                        Err(error) => { let _ = events.send(UiEvent::Error(error.to_string())); }
                    }
                    let _ = events.send(UiEvent::Done);
                    break;
                }
            }
        }
    }
}

async fn reply(
    shared: &SharedState,
    messages: &[Message],
    events: &EventSender,
) -> Result<Message, AgentError> {
    let used = crate::llm::context_size(messages).saturating_add(4096);
    if used > shared.compact_threshold {
        return Err(AgentError::ContextBudget {
            used,
            limit: shared.compact_threshold,
        });
    }
    let mut delays = retry::backoff_delays(shared.max_retries).into_iter();
    let mut stream = loop {
        match shared.llm.chat_stream(messages, None).await {
            Ok(stream) => break stream,
            Err(error) => match delays.next().filter(|_| retry::is_retryable(&error)) {
                Some(delay) => tokio::time::sleep(delay).await,
                None => return Err(error.into()),
            },
        }
    };
    let mut answer = DeltaAggregator::default();
    while let Some(delta) = stream.next().await {
        let delta = delta?;
        if let Some(text) = &delta.content {
            let _ = events.send(UiEvent::Text(text.clone()));
        }
        if let Some(usage) = &delta.usage {
            let _ = events.send(UiEvent::ContextUsage(Some(
                usage.prompt_tokens.saturating_add(usage.completion_tokens),
            )));
            let _ = events.send(UiEvent::TokenUsage(usage.clone()));
        }
        answer.push(&delta);
    }
    let answer = answer.into_message();
    if answer.tool_calls.is_some() {
        return Err(crate::llm::LlmError::Stream(
            "Temporary conversations do not support tool calls".into(),
        )
        .into());
    }
    Ok(answer)
}
