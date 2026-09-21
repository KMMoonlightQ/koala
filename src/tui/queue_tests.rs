use super::*;

fn setup() -> (App, mpsc::UnboundedReceiver<SessionCommand>) {
    let (session, commands) = SessionHandle::test_channel();
    let mut app = App::new(session);
    app.set_lang(Lang::Zh);
    (app, commands)
}

fn enter(app: &mut App, text: &str, modifiers: KeyModifiers) {
    app.input = input::editor(text, app.lang);
    handle_key(app, KeyEvent::new(KeyCode::Enter, modifiers));
}

fn submitted(commands: &mut mpsc::UnboundedReceiver<SessionCommand>, expected: &str) {
    assert!(matches!(commands.try_recv(), Ok(SessionCommand::Submit(text)) if text == expected));
}

#[test]
fn queued_messages_run_fifo_after_completion_without_overwriting_draft() {
    let (mut app, mut commands) = setup();
    enter(&mut app, "first", KeyModifiers::NONE);
    submitted(&mut commands, "first");
    enter(&mut app, "second", KeyModifiers::NONE);
    enter(&mut app, "third", KeyModifiers::NONE);
    assert!(app.input.is_empty());
    assert!(commands.try_recv().is_err());
    app.input.insert_str("unfinished draft");
    handle_ui_event(&mut app, UiEvent::Done);
    submitted(&mut commands, "second");
    assert!(app.busy);
    assert_eq!(app.input.lines(), ["unfinished draft"]);
    handle_ui_event(&mut app, UiEvent::Done);
    submitted(&mut commands, "third");
    handle_ui_event(&mut app, UiEvent::Done);
    assert!(!app.busy);
    assert!(commands.try_recv().is_err());
}

#[test]
fn up_recalls_and_removes_latest_queued_message_before_history() {
    let (mut app, mut commands) = setup();
    app.history.record("history").unwrap();
    app.busy = true;
    enter(&mut app, "older queued", KeyModifiers::NONE);
    enter(&mut app, "latest\nqueued", KeyModifiers::NONE);
    handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(app.input.lines(), ["latest", "queued"]);
    handle_ui_event(&mut app, UiEvent::Done);
    submitted(&mut commands, "older queued");
    handle_ui_event(&mut app, UiEvent::Done);
    assert!(commands.try_recv().is_err());
    assert_eq!(app.input.lines(), ["latest", "queued"]);
}

#[test]
fn up_only_recalls_history_when_editor_is_empty() {
    let (mut app, _) = setup();
    app.history.record("older").unwrap();
    app.history.record("latest").unwrap();
    app.input.insert_str("draft");
    handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(app.input.lines(), ["draft"]);
    app.input = new_input(app.lang);
    handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(app.input.lines(), ["latest"]);
    handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(app.input.lines(), ["latest"]);
}

#[test]
fn command_enter_waits_for_cancellation_then_runs_before_queue() {
    let (mut app, mut commands) = setup();
    app.busy = true;
    enter(&mut app, "queued", KeyModifiers::NONE);
    enter(&mut app, "urgent", KeyModifiers::SUPER);
    assert!(matches!(commands.try_recv(), Ok(SessionCommand::Cancel)));
    assert!(commands.try_recv().is_err());
    // Completion can already be in flight when cancellation is requested.
    handle_ui_event(&mut app, UiEvent::Done);
    assert!(commands.try_recv().is_err());
    handle_ui_event(&mut app, UiEvent::Cancelled);
    submitted(&mut commands, "urgent");
    handle_ui_event(&mut app, UiEvent::Done);
    submitted(&mut commands, "queued");
}

#[test]
fn new_session_clears_queued_messages() {
    let (mut app, mut commands) = setup();
    app.busy = true;
    enter(&mut app, "queued", KeyModifiers::NONE);
    enter(&mut app, "/new", KeyModifiers::NONE);
    assert!(matches!(
        commands.try_recv(),
        Ok(SessionCommand::NewSession)
    ));
    handle_ui_event(&mut app, UiEvent::Done);
    handle_ui_event(&mut app, UiEvent::SessionReset);
    handle_ui_event(&mut app, UiEvent::Done);
    assert!(commands.try_recv().is_err());
}

#[test]
fn queue_preview_shows_count_and_pending_text() {
    let (mut app, _) = setup();
    app.busy = true;
    enter(&mut app, "run the tests", KeyModifiers::NONE);
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(110, 24)).unwrap();
    terminal.draw(|f| draw(f, &mut app)).unwrap();
    let visible = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect::<String>();
    assert!(
        visible
            .split_whitespace()
            .collect::<String>()
            .contains("待执行1")
    );
    assert!(visible.contains("run the tests"));
}

#[test]
fn queued_images_are_recalled_and_do_not_consume_unsent_attachments() {
    let (mut app, mut commands) = setup();
    app.busy = true;
    let queued = crate::images::from_rgba(1, 1, &[255, 0, 0, 255]).unwrap();
    app.input.insert_str("queued image");
    clipboard::apply(&mut app, Ok(clipboard::Paste::Image(queued.clone())));
    handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(app.images.is_empty());
    let draft = crate::images::from_rgba(1, 1, &[0, 255, 0, 255]).unwrap();
    clipboard::apply(&mut app, Ok(clipboard::Paste::Image(draft.clone())));
    handle_ui_event(&mut app, UiEvent::Done);
    assert!(
        matches!(commands.try_recv(), Ok(SessionCommand::SubmitWithImages { text, images }) if text == "queued image" && images == vec![queued])
    );
    assert_eq!(app.images.len(), 1);
    enter(&mut app, "recall image [image 1]", KeyModifiers::NONE);
    handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    assert_eq!(app.input.lines(), ["recall image [image 1]"]);
    assert_eq!(app.images.len(), 1);
}

#[test]
fn empty_command_enter_does_not_interrupt_and_idle_command_enter_sends() {
    let (mut app, mut commands) = setup();
    app.busy = true;
    enter(&mut app, "  ", KeyModifiers::SUPER);
    assert!(commands.try_recv().is_err());
    app.busy = false;
    enter(&mut app, "now", KeyModifiers::SUPER);
    submitted(&mut commands, "now");
}

#[test]
fn extension_cancel_pauses_queue_when_done_arrives_before_acknowledgement() {
    let (mut app, mut commands) = setup();
    app.busy = true;
    enter(&mut app, "queued", KeyModifiers::NONE);
    app.extension_ui.open();
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    );
    assert!(matches!(commands.try_recv(), Ok(SessionCommand::Cancel)));
    handle_ui_event(&mut app, UiEvent::Done);
    assert!(commands.try_recv().is_err());
    assert!(app.busy);
    handle_ui_event(&mut app, UiEvent::Cancelled);
    assert!(commands.try_recv().is_err());
    assert!(!app.busy);
    assert_eq!(app.queue.pending.len(), 1);
}

#[test]
fn plain_cancel_pauses_queue_and_new_input_resumes_it() {
    let (mut app, mut commands) = setup();
    app.busy = true;
    enter(&mut app, "queued", KeyModifiers::NONE);
    app.cancel();
    assert!(matches!(commands.try_recv(), Ok(SessionCommand::Cancel)));
    handle_ui_event(&mut app, UiEvent::Done);
    assert!(commands.try_recv().is_err());
    handle_ui_event(&mut app, UiEvent::Cancelled);
    assert!(!app.busy);
    assert!(commands.try_recv().is_err());
    enter(&mut app, "resume", KeyModifiers::NONE);
    submitted(&mut commands, "resume");
    handle_ui_event(&mut app, UiEvent::Done);
    submitted(&mut commands, "queued");
}

#[tokio::test]
async fn queued_turn_reaches_model_with_previous_answer_in_context() {
    use crate::test_support::{MockLlm, stream};
    let mut mock = MockLlm::start(vec![
        stream(serde_json::json!({"content": "first answer"})),
        stream(serde_json::json!({"content": "second answer"})),
    ])
    .await;
    let root =
        std::env::temp_dir().join(format!("koala-queue-integration-{}", uuid::Uuid::new_v4()));
    let mut cfg = Config::default();
    cfg.llm.model = "test".into();
    cfg.llm.base_url = mock.url.clone();
    cfg.agent.session_dir = root.join("sessions");
    cfg.agent.memory_file = root.join("memory.md");
    let (session, mut events) = session::spawn(Agent::new(&cfg).await.unwrap());
    let mut app = App::new(session);
    enter(&mut app, "first question", KeyModifiers::NONE);
    enter(&mut app, "second question", KeyModifiers::NONE);
    tokio::time::timeout(Duration::from_secs(5), async {
        while app.busy {
            let event = events.recv().await.unwrap();
            if let UiEvent::Error(error) = &event {
                panic!("{error}");
            }
            handle_ui_event(&mut app, event);
        }
    })
    .await
    .unwrap();
    mock.request().await;
    let request = mock.request().await;
    let messages = request["messages"].as_array().unwrap();
    let content: Vec<_> = messages
        .iter()
        .filter_map(|m| m["content"].as_str())
        .collect();
    assert!(
        content
            .windows(3)
            .any(|items| items == ["first question", "first answer", "second question"])
    );
    app.session.send(SessionCommand::Shutdown);
    std::fs::remove_dir_all(root).unwrap();
}
