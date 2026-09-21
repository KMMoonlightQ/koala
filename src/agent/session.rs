use super::Agent;
use super::event::{EventSender, SessionCommand, UiEvent};
use crate::i18n::{self, Key, Lang};
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle;

/// Frontends drive the agent exclusively through commands and events.
pub struct SessionHandle {
    tx: mpsc::UnboundedSender<SessionCommand>,
}

impl SessionHandle {
    pub fn send(&self, cmd: SessionCommand) {
        let _ = self.tx.send(cmd);
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }

    pub fn open_btw(&self) -> (Self, mpsc::UnboundedReceiver<UiEvent>) {
        let (tx, commands) = mpsc::unbounded_channel();
        let (events, rx) = mpsc::unbounded_channel();
        self.send(SessionCommand::OpenBtw { commands, events });
        (Self { tx }, rx)
    }

    #[cfg(test)]
    pub(crate) fn test_channel() -> (Self, mpsc::UnboundedReceiver<SessionCommand>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }
}

/// Only one foreground operation owns the agent. Control commands never wait
/// for its mutex: cancellation first drops and joins the operation. Each new
/// session gets a fresh event channel so old background output cannot leak in.
pub fn spawn(agent: Agent) -> (SessionHandle, mpsc::UnboundedReceiver<UiEvent>) {
    spawn_inner(agent, false)
}
pub fn spawn_tui(agent: Agent) -> (SessionHandle, mpsc::UnboundedReceiver<UiEvent>) {
    spawn_inner(agent, true)
}
fn spawn_inner(agent: Agent, has_ui: bool) -> (SessionHandle, mpsc::UnboundedReceiver<UiEvent>) {
    let ui_session = agent.transcript.id().to_owned();
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let (mut work_tx, mut work_rx) = mpsc::unbounded_channel();
    let background = agent.background.clone();
    let mut current_background = background.clone();
    // Shared language cell, cloned out before the agent goes behind its mutex:
    // /lang neither waits for a running turn nor blocks cancellation.
    let shared = agent.shared.clone();
    let mut background_count = background.subscribe_count();
    let _ = ev_tx.send(UiEvent::PlanMode(agent.plan_mode()));
    let _ = ev_tx.send(agent.model_settings());
    let _ = ev_tx.send(UiEvent::PermissionMode(agent.shared.permissions.mode()));
    let _ = ev_tx.send(UiEvent::BackgroundCount(*background_count.borrow()));
    let mut snapshot = agent.history.clone();
    let agent = Arc::new(Mutex::new(agent));
    tokio::spawn(async move {
        let mut ui_host = has_ui.then(|| {
            super::extension_ui::Host::start(
                ui_session,
                shared.extensions.ui_extensions(),
                ev_tx.clone(),
            )
        });
        let mut active: Option<JoinHandle<()>> = None;
        let mut progress = String::new();
        let mut watch_tasks = false;
        let mut asides = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                // Complete and drain an old operation before accepting another.
                biased;
                result = async { active.as_mut().unwrap().await }, if active.is_some() => {
                    active = None;
                    drain(&mut work_rx, &ev_tx, &mut progress, shared.lang.get());
                    // A failed turn also leaves a pending input. Preserve its
                    // visible progress before another operation can begin.
                    if let Err(e) = agent.lock().await.record_interruption(&progress) {
                        let _ = ev_tx.send(UiEvent::Error(e.to_string()));
                    }
                    if let Err(e) = result {
                        let _ = ev_tx.send(UiEvent::Error(format!("operation failed: {e}")));
                    }
                    let _ = ev_tx.send(UiEvent::Done);
                }
                cmd = cmd_rx.recv() => {
                    let Some(cmd) = cmd else {
                        stop(&mut active, &agent, &mut work_rx, &ev_tx, &mut progress, shared.lang.get()).await;
                        break;
                    };
                    let (cmd, images) = match cmd {
                        SessionCommand::SubmitWithImages { text, images } => (SessionCommand::Submit(text), images),
                        other => (other, Vec::new()),
                    };
                    match cmd {
                        SessionCommand::ExtensionUi(action) => {
                            if let Some(host) = &ui_host { host.action(action); }
                        }
                        SessionCommand::OpenBtw { commands, events } => {
                            // The foreground task may be scheduled but not yet holding
                            // the mutex. Its saved snapshot already includes its input.
                            let history = if active.is_some() {
                                snapshot.clone()
                            } else {
                                agent.lock().await.history.clone()
                            };
                            asides.spawn(super::btw::run(shared.clone(), history, commands, events));
                        }
                        SessionCommand::SetPermissionMode(mode) => {
                            // Save before applying, without waiting for a running turn's mutex.
                            if let Some(journal) = &current_background.journal
                                && let Err(error) = journal.permission_mode(mode)
                            {
                                let _ = ev_tx.send(UiEvent::Error(error.to_string()));
                                continue;
                            }
                            shared.permissions.set_mode(mode);
                            let lang = shared.lang.get();
                            let _ = ev_tx.send(UiEvent::PermissionMode(mode));
                            let _ = ev_tx.send(UiEvent::Info(i18n::fill(
                                lang,
                                Key::InfoPermissionSwitched,
                                &[
                                    ("mode", mode.label()),
                                    ("description", mode.description(lang)),
                                ],
                            )));
                        }
                        SessionCommand::SetLang(value) => shared.lang.set(value),
                        SessionCommand::Cancel => {
                            if let Some(host) = &ui_host { host.cancel().await; }
                            stop(&mut active, &agent, &mut work_rx, &ev_tx, &mut progress, shared.lang.get()).await;
                            // A completion may win the select before this command.
                            // Always acknowledge so the composer can submit its next turn.
                            let _ = ev_tx.send(UiEvent::Cancelled);
                        }
                        SessionCommand::Shutdown => {
                            stop(&mut active, &agent, &mut work_rx, &ev_tx, &mut progress, shared.lang.get()).await;
                            break;
                        }
                        SessionCommand::NewSession => {
                            stop(&mut active, &agent, &mut work_rx, &ev_tx, &mut progress, shared.lang.get()).await;
                            {
                                let mut guard = agent.lock().await;
                                guard.new_session();
                                current_background = guard.background.clone();
                            }
                            (work_tx, work_rx) = mpsc::unbounded_channel();
                            progress.clear();
                            restart_ui(&mut ui_host, &shared, agent.lock().await.transcript.id().to_owned(), &ev_tx).await;
                            let _ = ev_tx.send(UiEvent::SessionReset);
                            let _ = ev_tx.send(UiEvent::PlanMode(agent.lock().await.plan_mode()));
                        }
                        SessionCommand::Submit(text) if active.is_none() => {
                            snapshot = agent.lock().await.history.clone();
                            snapshot.push(crate::llm::Message::user_with_images(&text, images.clone()));
                            progress.clear();
                            let ui_context = ui_host.as_ref().map(|h| h.context()).unwrap_or_default();
                            let agent = agent.clone();
                            let events = work_tx.clone();
                            active = Some(tokio::spawn(async move {
                                let mut guard = agent.lock().await;
                                if let Err(e) = crate::extensions::ui::scope(ui_context, guard.run_turn_with_images(&text, images, events.clone())).await {
                                    let _ = events.send(UiEvent::Error(e.to_string()));
                                }
                            }));
                        }
                        SessionCommand::Compact if active.is_none() => {
                            snapshot = agent.lock().await.history.clone();
                            progress.clear();
                            let ui_context = ui_host.as_ref().map(|h| h.context()).unwrap_or_default();
                            let agent = agent.clone();
                            let events = work_tx.clone();
                            // Read the language before the closure takes the
                            // shared state by value.
                            let lang = shared.lang.get();
                            active = Some(tokio::spawn(async move {
                                let _ = events.send(UiEvent::Status(
                                    i18n::text(lang, Key::StatusCompacting).into(),
                                ));
                                let ev = match crate::extensions::ui::scope(ui_context, agent.lock().await.compact_now()).await {
                                    Ok(true) => {
                                        let _ = events.send(UiEvent::ContextUsage(None));
                                        UiEvent::Note(i18n::text(lang, Key::InfoContextCompacted).into())
                                    },
                                    Ok(false) => UiEvent::Note(
                                        i18n::text(lang, Key::InfoNothingToCompact).into(),
                                    ),
                                    Err(e) => UiEvent::Error(e.to_string()),
                                };
                                let _ = events.send(ev);
                            }));
                        }
                        SessionCommand::ShowGraph => {
                            let graph = current_background.journal.as_ref().map(|j| j.graph_snapshot()).unwrap_or_default();
                            let _ = ev_tx.send(UiEvent::Graph(graph));
                        }
                        SessionCommand::ShowTasks => {
                            watch_tasks = true;
                            let _ = ev_tx.send(UiEvent::Tasks(background.list()));
                        }
                        SessionCommand::HideTasks => watch_tasks = false,
                        SessionCommand::StopTask(id) => {
                            let stopped = background.stop(id).await;
                            if !stopped {
                                let lang = shared.lang.get();
                                let _ = ev_tx.send(UiEvent::Note(i18n::fill(
                                    lang,
                                    Key::NoteTaskNotStoppable,
                                    &[("id", &id.to_string())],
                                )));
                            }
                            let _ = ev_tx.send(UiEvent::Tasks(background.list()));
                        }
                        SessionCommand::RestoreSession(_) if active.is_some() => {
                            let lang = shared.lang.get();
                            let _ = ev_tx.send(UiEvent::SessionRestoreFailed(
                                i18n::text(lang, Key::NoteBusyInterruptFirst).into(),
                            ));
                        }
                        _ if active.is_some() => {
                            let lang = shared.lang.get();
                            let _ = ev_tx.send(UiEvent::Note(
                                i18n::text(lang, Key::NoteBusyInterruptFirst).into(),
                            ));
                        }
                        cmd @ (SessionCommand::BranchBeforeTurn(_) | SessionCommand::ContinueAfterTurn(_)) => {
                            let (id, after) = match cmd {
                                SessionCommand::BranchBeforeTurn(id) => (id, false),
                                SessionCommand::ContinueAfterTurn(id) => (id, true),
                                _ => unreachable!(),
                            };
                            let mut guard = agent.lock().await;
                            match guard.navigate_turn(&id, after) {
                                Ok(draft) => {
                                    current_background = guard.background.clone();
                                    (work_tx, work_rx) = mpsc::unbounded_channel();
                                    progress.clear();
                                    restart_ui(&mut ui_host, &shared, guard.transcript.id().to_owned(), &ev_tx).await;
                                    let _ = ev_tx.send(UiEvent::SessionReset);
                                    if let Some(journal) = &guard.background.journal
                                        && let Ok(Some(saved)) = journal.load()
                                    {
                                        let _ = ev_tx.send(UiEvent::WorkRestored(saved.trace));
                                    }
                                    let _ = ev_tx.send(UiEvent::Todos(guard.todos.items.iter().map(super::event::TodoView::from).collect()));
                                    let _ = ev_tx.send(UiEvent::Tasks(background.list()));
                                    let images = if after { Vec::new() } else {
                                        guard.background.journal.as_ref().and_then(|j| j.graph_snapshot().get(&id).map(|n| n.data["images"].clone()))
                                            .and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
                                    };
                                    let _ = ev_tx.send(UiEvent::Draft(draft));
                                    let _ = ev_tx.send(UiEvent::DraftImages(images));
                                }
                                Err(error) => { let _ = ev_tx.send(UiEvent::Error(error)); }
                            }
                        }
                        SessionCommand::Memory { control } => {
                            let guard = agent.lock().await;
                            match control {
                                Some(super::event::MemoryControl::Read(enabled)) => guard.agent_memory.set_read_enabled(enabled),
                                Some(super::event::MemoryControl::Write(enabled)) => guard.agent_memory.set_write_enabled(enabled),
                                None => {},
                            }
                            let memory = &guard.agent_memory;
                            let result = memory.content().map(|index| format!("{}\n{}\n{index}",
                                i18n::text(shared.lang.get(), Key::MemoryIndex), memory.status()));
                            let _ = ev_tx.send(match result {
                                Ok(text) => UiEvent::Info(text), Err(e) => UiEvent::Error(e.to_string())
                            });
                        }
                        SessionCommand::ShowSessions => {
                            match agent.lock().await.list_sessions() {
                                Ok(items) => { let _ = ev_tx.send(UiEvent::Sessions(items)); }
                                Err(error) => {
                                    let _ = ev_tx.send(UiEvent::Sessions(Vec::new()));
                                    let _ = ev_tx.send(UiEvent::Note(error));
                                }
                            }
                        }
                        SessionCommand::RestoreSession(id) => {
                            let restored = agent.lock().await.restore_session(&id);
                            match restored {
                                Ok(records) => {
                                    (work_tx, work_rx) = mpsc::unbounded_channel();
                                    progress.clear();
                                    watch_tasks = false;
                                    restart_ui(&mut ui_host, &shared, id.clone(), &ev_tx).await;
                                    let _ = ev_tx.send(UiEvent::SessionRestored { id, records });
                                    let guard = agent.lock().await;
                                    current_background = guard.background.clone();
                                    if let Some(journal) = &guard.background.journal {
                                        match journal.load() {
                                            Ok(Some(saved)) if !saved.trace.is_empty() => { let _ = ev_tx.send(UiEvent::WorkRestored(saved.trace)); }
                                            Ok(Some(_)) => {}
                                            Ok(None) => {}
                                            Err(error) => { let _ = ev_tx.send(UiEvent::Error(error)); }
                                        }
                                    }
                                    for text in current_background.take_notifications() { let _ = ev_tx.send(UiEvent::Note(text)); }
                                    let _ = ev_tx.send(UiEvent::Todos(guard.todos.items.iter().map(super::event::TodoView::from).collect()));
                                    let _ = ev_tx.send(UiEvent::Tasks(background.list()));
                                    let _ = ev_tx.send(UiEvent::BackgroundCount(*background_count.borrow()));
                                    let _ = ev_tx.send(UiEvent::PlanMode(guard.plan_mode()));
                                    let _ = ev_tx.send(UiEvent::PermissionMode(shared.permissions.mode()));
                                }
                                Err(error) => { let _ = ev_tx.send(UiEvent::SessionRestoreFailed(error)); }
                            }
                        }
                        SessionCommand::TogglePlanMode => {
                            match agent.lock().await.toggle_plan_mode() {
                                Ok(on) => { let _ = ev_tx.send(UiEvent::PlanMode(on)); }
                                Err(error) => { let _ = ev_tx.send(UiEvent::Error(error.to_string())); }
                            }
                        }
                        SessionCommand::SelectModel(name) => {
                            let mut agent = agent.lock().await;
                            match agent.select_model(&name) {
                                Ok(()) => {
                                    let _ = ev_tx.send(agent.model_settings());
                                    let lang = agent.lang();
                                    let _ = ev_tx.send(UiEvent::Info(i18n::fill(
                                        lang,
                                        Key::InfoModelSwitched,
                                        &[("name", &name)],
                                    )));
                                }
                                Err(message) => { let _ = ev_tx.send(UiEvent::Note(message)); }
                            }
                        }
                        SessionCommand::SetReasoningEffort(value) => {
                            let agent = agent.lock().await;
                            match agent.set_reasoning_effort(&value) {
                                Ok(effort) => {
                                    let _ = ev_tx.send(agent.model_settings());
                                    let lang = agent.lang();
                                    let _ = ev_tx.send(UiEvent::Info(i18n::fill(
                                        lang,
                                        Key::InfoEffortSwitched,
                                        &[("effort", &effort)],
                                    )));
                                }
                                Err(message) => { let _ = ev_tx.send(UiEvent::Note(message)); }
                            }
                        }
                        SessionCommand::ShowSkills => {
                            let text = agent.lock().await.skills().listing();
                            let _ = ev_tx.send(UiEvent::Info(text));
                        }
                        _ => {}
                    }
                }
                Ok(()) = background_count.changed() => {
                    for text in current_background.take_notifications() { let _ = ev_tx.send(UiEvent::Note(text)); }
                    for error in background.take_errors() { let _ = ev_tx.send(UiEvent::Note(error)); }
                    let _ = ev_tx.send(UiEvent::BackgroundCount(*background_count.borrow_and_update()));
                    if watch_tasks { let _ = ev_tx.send(UiEvent::Tasks(background.list())); }
                }
                _ = asides.join_next(), if !asides.is_empty() => {},
                Some(ev) = work_rx.recv() => forward(ev, &ev_tx, &mut progress, shared.lang.get()),
            }
        }
        if let Some(host) = &ui_host {
            host.shutdown().await;
        }
    });
    (SessionHandle { tx: cmd_tx }, ev_rx)
}

async fn restart_ui(
    host: &mut Option<super::extension_ui::Host>,
    shared: &Arc<super::SharedState>,
    session: String,
    events: &EventSender,
) {
    if let Some(old) = host.take() {
        old.shutdown().await;
        let _ = events.send(UiEvent::ExtensionUi(Default::default()));
        *host = Some(super::extension_ui::Host::start(
            session,
            shared.extensions.ui_extensions(),
            events.clone(),
        ));
    }
}

/// `progress` becomes the visible record of an interrupted turn, so it is
/// written in the frontend's language too.
fn forward(ev: UiEvent, events: &EventSender, progress: &mut String, lang: Lang) {
    match &ev {
        UiEvent::Text(text) => progress.push_str(text),
        UiEvent::ToolStart { name, summary, .. } => progress.push_str(&i18n::fill(
            lang,
            Key::ProgressToolCall,
            &[("name", name), ("summary", summary)],
        )),
        UiEvent::ToolEnd { output, .. } => progress.push_str(&i18n::fill(
            lang,
            Key::ProgressToolResult,
            &[("output", &super::tools::result_preview(output))],
        )),
        _ => {}
    }
    let _ = events.send(ev);
}

fn drain(
    rx: &mut mpsc::UnboundedReceiver<UiEvent>,
    events: &EventSender,
    progress: &mut String,
    lang: Lang,
) {
    while let Ok(ev) = rx.try_recv() {
        forward(ev, events, progress, lang);
    }
}

async fn stop(
    active: &mut Option<JoinHandle<()>>,
    agent: &Arc<Mutex<Agent>>,
    rx: &mut mpsc::UnboundedReceiver<UiEvent>,
    events: &EventSender,
    progress: &mut String,
    lang: Lang,
) -> bool {
    let Some(task) = active.take() else {
        return false;
    };
    task.abort();
    let _ = task.await;
    drain(rx, events, progress, lang);
    if let Err(e) = agent.lock().await.record_interruption(progress) {
        let _ = events.send(UiEvent::Error(e.to_string()));
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::time::{Duration, timeout};

    #[tokio::test]
    async fn image_commands_reach_main_and_btw_and_branch_restores_attachments() {
        use crate::test_support::{MockLlm, stream};
        let mut mock = MockLlm::respond(|_| stream(serde_json::json!({"content":"answer"}))).await;
        let root =
            std::env::temp_dir().join(format!("koala-image-session-{}", uuid::Uuid::new_v4()));
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.llm.base_url = mock.url.clone();
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.md");
        let red = crate::images::from_rgba(1, 1, &[255, 0, 0, 255]).unwrap();
        let green = crate::images::from_rgba(1, 1, &[0, 255, 0, 255]).unwrap();
        let (main, mut events) = spawn(Agent::new(&cfg).await.unwrap());
        main.send(SessionCommand::SubmitWithImages {
            text: "main".into(),
            images: vec![red.clone()],
        });
        let (aside, mut side_events) = main.open_btw();
        aside.send(SessionCommand::SubmitWithImages {
            text: "side".into(),
            images: vec![green.clone()],
        });
        receive_until(&mut side_events, |e| matches!(e, UiEvent::Done)).await;
        receive_until(&mut events, |e| matches!(e, UiEvent::Done)).await;
        for _ in 0..2 {
            let request = mock.request().await;
            let messages = request["messages"].as_array().unwrap();
            assert!(
                messages
                    .iter()
                    .any(|m| m["content"][1]["image_url"]["url"] == red.data_url)
            );
            if messages.last().unwrap()["content"][0]["text"] == "side" {
                assert_eq!(
                    messages.last().unwrap()["content"][1]["image_url"]["url"],
                    green.data_url
                );
            } else {
                assert!(!request.to_string().contains(&green.data_url));
            }
        }
        main.send(SessionCommand::ShowGraph);
        let turn = timeout(Duration::from_secs(3), async {
            loop {
                if let Some(UiEvent::Graph(graph)) = events.recv().await {
                    break graph
                        .nodes
                        .iter()
                        .find(|n| n.kind == super::super::graph::Kind::Turn)
                        .unwrap()
                        .id
                        .clone();
                }
            }
        })
        .await
        .unwrap();
        main.send(SessionCommand::BranchBeforeTurn(turn));
        timeout(Duration::from_secs(3), async {
            loop {
                if let Some(UiEvent::DraftImages(images)) = events.recv().await {
                    assert_eq!(images, vec![red]);
                    break;
                }
            }
        })
        .await
        .unwrap();
        aside.send(SessionCommand::Shutdown);
        main.send(SessionCommand::Shutdown);
        while events.recv().await.is_some() {}
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn btw_opened_immediately_after_submit_includes_current_main_question() {
        use crate::test_support::{MockLlm, stream};
        let mut mock = MockLlm::respond(|_| stream(serde_json::json!({"content":"answer"}))).await;
        let root = std::env::temp_dir().join(format!("koala-btw-queued-{}", uuid::Uuid::new_v4()));
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.llm.base_url = mock.url.clone();
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.md");
        let (main, mut events) = spawn(Agent::new(&cfg).await.unwrap());
        main.send(SessionCommand::Submit(
            "just submitted main question".into(),
        ));
        let (aside, mut side_events) = main.open_btw();
        aside.send(SessionCommand::Submit("immediate aside".into()));
        receive_until(&mut side_events, |e| matches!(e, UiEvent::Done)).await;
        receive_until(&mut events, |e| matches!(e, UiEvent::Done)).await;
        for _ in 0..2 {
            let request = mock.request().await;
            let messages = request["messages"].as_array().unwrap();
            if messages.last().unwrap()["content"] == "immediate aside" {
                assert!(
                    messages
                        .iter()
                        .any(|m| m["content"] == "just submitted main question")
                );
            }
        }
        main.send(SessionCommand::Shutdown);
        while events.recv().await.is_some() {}
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn btw_inherits_context_remembers_followups_but_never_persists_or_changes_main() {
        use crate::llm::Message;
        use crate::test_support::{MockLlm, stream};
        let mut mock = MockLlm::start(vec![
            stream(serde_json::json!({"content":"private answer"})),
            stream(serde_json::json!({"content":"private followup answer"})),
            stream(serde_json::json!({"content":"fresh aside answer"})),
            stream(serde_json::json!({"content":"main answer"})),
        ])
        .await;
        let root = std::env::temp_dir().join(format!("koala-btw-{}", uuid::Uuid::new_v4()));
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.llm.base_url = mock.url.clone();
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.md");
        let mut agent = Agent::new(&cfg).await.unwrap();
        agent.history = vec![
            Message::user("main context"),
            Message::assistant("prior answer"),
        ];
        let (main, mut events) = spawn(agent);
        let (aside, mut side_events) = main.open_btw();
        aside.send(SessionCommand::Submit("private question".into()));
        receive_until(&mut side_events, |e| matches!(e, UiEvent::Done)).await;
        let request = mock.request().await;
        let text = request["messages"].to_string();
        assert!(
            text.contains("main context")
                && text.contains("prior answer")
                && text.contains("private question")
        );
        assert!(request.get("tools").is_none());
        aside.send(SessionCommand::Submit("private followup".into()));
        receive_until(&mut side_events, |e| matches!(e, UiEvent::Done)).await;
        let request = mock.request().await;
        let text = request["messages"].to_string();
        assert!(
            text.contains("private question")
                && text.contains("private answer")
                && text.contains("private followup")
        );
        assert!(
            !cfg.agent.session_dir.exists()
                || std::fs::read_dir(&cfg.agent.session_dir)
                    .unwrap()
                    .next()
                    .is_none()
        );
        aside.send(SessionCommand::Shutdown);
        timeout(Duration::from_secs(3), async {
            while side_events.recv().await.is_some() {}
        })
        .await
        .unwrap();
        let (fresh, mut fresh_events) = main.open_btw();
        fresh.send(SessionCommand::Submit("fresh aside".into()));
        receive_until(&mut fresh_events, |e| matches!(e, UiEvent::Done)).await;
        let request = mock.request().await;
        let text = request["messages"].to_string();
        assert!(text.contains("main context"));
        assert!(!text.contains("private question") && !text.contains("private answer"));
        fresh.send(SessionCommand::Shutdown);
        main.send(SessionCommand::Submit("continue main".into()));
        receive_until(&mut events, |e| matches!(e, UiEvent::Done)).await;
        let request = mock.request().await;
        let text = request["messages"].to_string();
        assert!(text.contains("main context") && text.contains("continue main"));
        assert!(
            !text.contains("private question")
                && !text.contains("private answer")
                && !text.contains("private followup")
                && !text.contains("fresh aside")
        );
        main.send(SessionCommand::Shutdown);
        while events.recv().await.is_some() {}
        for entry in std::fs::read_dir(&cfg.agent.session_dir).unwrap() {
            let content = std::fs::read_to_string(entry.unwrap().path()).unwrap();
            assert!(
                !content.contains("private question")
                    && !content.contains("private answer")
                    && !content.contains("private followup")
                    && !content.contains("fresh aside")
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn btw_runs_and_cancels_while_main_stream_is_still_active() {
        use crate::llm::Message;
        let root =
            std::env::temp_dir().join(format!("koala-btw-concurrent-{}", uuid::Uuid::new_v4()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.llm.base_url = format!("http://{}", listener.local_addr().unwrap());
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.md");
        let mut agent = Agent::new(&cfg).await.unwrap();
        agent.history = vec![
            Message::user("completed main question"),
            Message::assistant("completed main answer"),
        ];
        let (main, mut events) = spawn(agent);
        main.send(SessionCommand::Submit("active main question".into()));
        let (mut main_socket, _) = timeout(Duration::from_secs(3), listener.accept())
            .await
            .unwrap()
            .unwrap();
        request(&mut main_socket).await;
        main_socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"main partial\"}}]}\n\n").await.unwrap();
        receive_until(
            &mut events,
            |e| matches!(e, UiEvent::Text(t) if t == "main partial"),
        )
        .await;
        let (aside, mut side_events) = main.open_btw();
        aside.send(SessionCommand::Submit("side while busy".into()));
        let (mut side_socket, _) = timeout(Duration::from_secs(3), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let body = request(&mut side_socket).await;
        let text = body["messages"].to_string();
        assert!(text.contains("completed main answer") && text.contains("active main question"));
        side_socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"side partial\"}}]}\n\n").await.unwrap();
        receive_until(
            &mut side_events,
            |e| matches!(e, UiEvent::Text(t) if t == "side partial"),
        )
        .await;
        aside.send(SessionCommand::Cancel);
        receive_until(&mut side_events, |e| matches!(e, UiEvent::Cancelled)).await;
        while let Ok(event) = events.try_recv() {
            assert!(!matches!(
                event,
                UiEvent::Done | UiEvent::Cancelled | UiEvent::Text(_)
            ));
        }
        aside.send(SessionCommand::Submit("retry side".into()));
        let (mut retry_socket, _) = timeout(Duration::from_secs(3), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let body = request(&mut retry_socket).await;
        let text = body["messages"].to_string();
        assert!(!text.contains("side partial") && !text.contains("side while busy"));
        // The main turn completes normally while a second side request is pending.
        main_socket.write_all(b"data: [DONE]\n\n").await.unwrap();
        receive_until(&mut events, |e| matches!(e, UiEvent::Done)).await;
        main.send(SessionCommand::Shutdown);
        timeout(Duration::from_secs(3), async {
            while events.recv().await.is_some() {}
            while side_events.recv().await.is_some() {}
        })
        .await
        .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn restoring_plan_session_updates_frontend_mode() {
        let root = std::env::temp_dir().join(format!("koala-plan-ui-{}", uuid::Uuid::new_v4()));
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.json");
        let mut agent = Agent::new(&cfg).await.unwrap();
        let id = agent.session_id().to_owned();
        agent
            .background
            .journal
            .as_ref()
            .unwrap()
            .context(&[crate::llm::Message::user("plan")], &[])
            .unwrap();
        agent.toggle_plan_mode().unwrap();
        agent.new_session();
        agent.toggle_plan_mode().unwrap();
        let (session, mut events) = spawn(agent);
        session.send(SessionCommand::RestoreSession(id));
        timeout(Duration::from_secs(3), async {
            let mut restored = false;
            while let Some(event) = events.recv().await {
                match event {
                    UiEvent::SessionRestored { .. } => restored = true,
                    UiEvent::PlanMode(on) if restored => {
                        assert!(on);
                        break;
                    }
                    UiEvent::SessionRestoreFailed(error) => panic!("{error}"),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        session.send(SessionCommand::Shutdown);
        drop(session);
        while events.recv().await.is_some() {}
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn legacy_compact_restore_preserves_display_history() {
        let root =
            std::env::temp_dir().join(format!("koala-legacy-compact-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("saved.jsonl"),
            concat!(
                "{\"ts\":\"then\",\"role\":\"user\",\"content\":\"old question\"}\n",
                "{\"ts\":\"then\",\"role\":\"assistant\",\"content\":\"old answer\"}\n"
            ),
        )
        .unwrap();
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.agent.session_dir = root.clone();
        cfg.agent.memory_file = root.join("memory.md");
        let (session, mut events) = spawn(Agent::new(&cfg).await.unwrap());
        session.send(SessionCommand::RestoreSession("saved".into()));
        receive_until(&mut events, |e| {
            matches!(e, UiEvent::SessionRestored { .. })
        })
        .await;
        session.send(SessionCommand::Compact);
        receive_until(&mut events, |e| matches!(e, UiEvent::Done)).await;
        session.send(SessionCommand::NewSession);
        receive_until(&mut events, |e| matches!(e, UiEvent::SessionReset)).await;
        let work_path = root.join("saved.work");
        let saved = super::super::work::Journal::new(work_path.clone())
            .load()
            .unwrap()
            .unwrap();
        assert_eq!(
            saved.trace.len(),
            2,
            "compact must seed the original display records"
        );
        // Also support context-only journals already created by the previous version.
        let context_only = std::fs::read_to_string(&work_path)
            .unwrap()
            .lines()
            .filter(|line| {
                serde_json::from_str::<serde_json::Value>(line)
                    .unwrap()
                    .get("Context")
                    .is_some()
            })
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        std::fs::write(work_path, context_only).unwrap();
        session.send(SessionCommand::RestoreSession("saved".into()));
        let mut visible = Vec::new();
        timeout(Duration::from_secs(3), async {
            loop {
                match events.recv().await.unwrap() {
                    UiEvent::SessionRestored { records, .. } => {
                        visible = records.into_iter().map(|r| r.content).collect()
                    }
                    UiEvent::WorkRestored(trace) => {
                        visible = trace
                            .into_iter()
                            .filter_map(|t| match t {
                                super::super::work::Trace::User(s)
                                | super::super::work::Trace::Text(s) => Some(s),
                                _ => None,
                            })
                            .collect()
                    }
                    UiEvent::PermissionMode(_) => break,
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(visible, ["old question", "old answer"]);
        session.send(SessionCommand::Shutdown);
        while events.recv().await.is_some() {}
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn restored_background_task_delivers_completion_notice() {
        use crate::test_support::{MockLlm, stream};
        let root = std::env::temp_dir().join(format!("koala-notice-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let release = root.join("release");
        let command = format!(
            "while [ ! -f '{}' ]; do sleep 0.01; done; echo FINISHED_AFTER_RESTORE",
            release.display()
        );
        let mock = MockLlm::start(vec![stream(serde_json::json!({"tool_calls":[{"index":0,"id":"bg","function":{"name":"bash","arguments":serde_json::json!({"command":command,"background":true}).to_string()}}]})), stream(serde_json::json!({"content":"started"}))]).await;
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.llm.base_url = mock.url.clone();
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.md");
        cfg.permissions.mode = crate::config::PermissionMode::NeverAsk;
        let agent = Agent::new(&cfg).await.unwrap();
        let id = agent.session_id().to_owned();
        let background = agent.background.clone();
        let (session, mut events) = spawn(agent);
        session.send(SessionCommand::Submit("start".into()));
        receive_until(&mut events, |e| matches!(e, UiEvent::Done)).await;
        session.send(SessionCommand::NewSession);
        receive_until(&mut events, |e| matches!(e, UiEvent::SessionReset)).await;
        session.send(SessionCommand::RestoreSession(id));
        receive_until(&mut events, |e| {
            matches!(e, UiEvent::SessionRestored { .. })
        })
        .await;
        std::fs::write(release, "go").unwrap();
        let result = timeout(Duration::from_secs(3), async {
            while let Some(event) = events.recv().await {
                if matches!(event, UiEvent::Note(ref text) if text.contains("FINISHED_AFTER_RESTORE")) { return; }
            }
            panic!("event stream closed");
        }).await;
        session.send(SessionCommand::Shutdown);
        while events.recv().await.is_some() {}
        for task in background.list() {
            background.stop(task.id).await;
        }
        std::fs::remove_dir_all(root).unwrap();
        assert!(result.is_ok(), "completion notice was lost after restore");
    }

    #[tokio::test]
    async fn language_switch_during_active_turn_updates_backend_without_interrupting() {
        let root = std::env::temp_dir().join(format!("koala-live-lang-{}", uuid::Uuid::new_v4()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.llm.base_url = format!("http://{}", listener.local_addr().unwrap());

        cfg.lang = Lang::En;
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.md");
        let agent = Agent::new(&cfg).await.unwrap();
        let shared = agent.shared.clone();
        let (handle, mut events) = spawn(agent);
        handle.send(SessionCommand::Submit("first prompt".into()));
        let (mut socket, _) = timeout(Duration::from_secs(3), listener.accept())
            .await
            .unwrap()
            .unwrap();
        request(&mut socket).await;
        handle.send(SessionCommand::SetLang(Lang::Zh));
        // FIFO task query is a barrier: SetLang must have been handled first.
        handle.send(SessionCommand::ShowTasks);
        receive_until(&mut events, |event| matches!(event, UiEvent::Tasks(_))).await;
        assert_eq!(shared.lang.get(), Lang::Zh);
        let response =
            "data: {\"choices\":[{\"delta\":{\"content\":\"finished\"}}]}\n\ndata: [DONE]\n\n";
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
        receive_until(
            &mut events,
            |event| matches!(event, UiEvent::Text(text) if text == "finished"),
        )
        .await;
        receive_until(&mut events, |event| matches!(event, UiEvent::Done)).await;
        handle.send(SessionCommand::Shutdown);
        while events.recv().await.is_some() {}
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn restored_session_reaches_model_and_appends_to_original_transcript() {
        use crate::config::PermissionMode;
        use crate::test_support::{MockLlm, stream};
        let mut mock = MockLlm::start(vec![stream(
            serde_json::json!({"content": "continued answer"}),
        )])
        .await;
        let root = std::env::temp_dir().join(format!("koala-resume-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("saved.jsonl");
        let old = concat!(
            "{\"ts\":\"then\",\"role\":\"user\",\"content\":\"OLD_USER_CONTEXT\"}\n",
            "{\"ts\":\"then\",\"role\":\"assistant\",\"content\":\"OLD_ANSWER\"}"
        );
        std::fs::write(&path, old).unwrap();
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.llm.base_url = mock.url.clone();

        cfg.agent.session_dir = root.clone();
        cfg.agent.memory_file = root.join("memory.md");
        cfg.permissions.mode = PermissionMode::NeverAsk;
        let agent = Agent::new(&cfg).await.unwrap();
        let shared = agent.shared.clone();
        let (session, mut events) = spawn(agent);
        session.send(SessionCommand::ShowSessions);
        receive_until(
            &mut events,
            |e| matches!(e, UiEvent::Sessions(items) if items.len() == 1 && items[0].id == "saved"),
        )
        .await;
        session.send(SessionCommand::RestoreSession("saved".into()));
        receive_until(&mut events, |e| matches!(e, UiEvent::SessionRestored { id, records } if id == "saved" && records.len() == 2)).await;
        assert_eq!(shared.permissions.mode(), PermissionMode::Normal);
        // Failure must preserve the restored conversation and its write target.
        session.send(SessionCommand::RestoreSession("missing".into()));
        receive_until(&mut events, |e| {
            matches!(e, UiEvent::SessionRestoreFailed(_))
        })
        .await;
        session.send(SessionCommand::Submit("continue".into()));
        receive_until(&mut events, |e| matches!(e, UiEvent::Done)).await;
        let request = mock.request().await;
        let messages = request["messages"].as_array().unwrap();
        assert!(
            messages
                .iter()
                .any(|m| m["role"] == "user" && m["content"] == "OLD_USER_CONTEXT")
        );
        assert!(
            messages
                .iter()
                .any(|m| m["role"] == "assistant" && m["content"] == "OLD_ANSWER")
        );
        let records =
            crate::agent::transcripts::read(&root, "saved", crate::i18n::Lang::En).unwrap();
        assert_eq!(records.len(), 4);
        assert_eq!(records[3].content, "continued answer");
        assert!(std::fs::read_to_string(path).unwrap().starts_with(old));
        session.send(SessionCommand::Shutdown);
        while events.recv().await.is_some() {}
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn permission_switch_during_active_turn_applies_to_next_tool() {
        use crate::config::PermissionMode;
        use crate::test_support::{MockLlm, stream};
        let root =
            std::env::temp_dir().join(format!("koala-live-permission-{}", uuid::Uuid::new_v4()));
        let path = std::env::current_dir()
            .unwrap()
            .join(format!(".permission-test-{}", uuid::Uuid::new_v4()));
        let mut mock = MockLlm::start(vec![
            stream(serde_json::json!({"tool_calls":[{"index":0,"id":"first","function":{"name":"read","arguments":"{\"path\":\"Cargo.toml\"}"}}]})),
            stream(serde_json::json!({"tool_calls":[{"index":0,"id":"second","function":{"name":"write","arguments":serde_json::json!({"path":path,"content":"edited"}).to_string()}}]})),
            stream(serde_json::json!({"content":"done"})),
        ]).await;
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.llm.base_url = mock.url.clone();
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.md");
        let agent = Agent::new(&cfg).await.unwrap();
        let shared = agent.shared.clone();
        let (session, mut events) = spawn(agent);
        session.send(SessionCommand::Submit("edit a file".into()));
        mock.request().await;
        let respond = timeout(Duration::from_secs(3), async {
            loop {
                if let UiEvent::PermissionRequest { respond, .. } = events.recv().await.unwrap() {
                    break respond;
                }
            }
        })
        .await
        .unwrap();
        session.send(SessionCommand::SetPermissionMode(PermissionMode::AutoEdit));
        receive_until(&mut events, |e| {
            matches!(e, UiEvent::PermissionMode(PermissionMode::AutoEdit))
        })
        .await;
        assert_eq!(shared.permissions.mode(), PermissionMode::AutoEdit);
        respond.send(true).unwrap();
        timeout(Duration::from_secs(3), async {
            loop {
                match events.recv().await.unwrap() {
                    UiEvent::PermissionRequest { .. } => panic!("workspace edit asked again"),
                    UiEvent::Error(e) => panic!("{e}"),
                    UiEvent::Done => break,
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "edited");
        session.send(SessionCommand::Shutdown);
        while events.recv().await.is_some() {}
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn restored_session_inherits_saved_permission_after_restart() {
        use crate::config::PermissionMode;
        use crate::test_support::{MockLlm, stream};
        let mock = MockLlm::start(vec![stream(serde_json::json!({"content": "answer"}))]).await;
        let root = std::env::temp_dir().join(format!("koala-permission-{}", uuid::Uuid::new_v4()));
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.llm.base_url = mock.url.clone();
        cfg.agent.session_dir = root.clone();
        cfg.agent.memory_file = root.join("memory.md");
        cfg.permissions.mode = PermissionMode::AutoEdit;
        let mut agent = Agent::new(&cfg).await.unwrap();
        let id = agent.session_id().to_owned();
        let (tx, _rx) = mpsc::unbounded_channel();
        agent.run_turn("question", tx).await.unwrap();
        drop(agent);
        cfg.permissions.mode = PermissionMode::Normal;
        let agent = Agent::new(&cfg).await.unwrap();
        let shared = agent.shared.clone();
        let (session, mut events) = spawn(agent);
        session.send(SessionCommand::RestoreSession(id.clone()));
        receive_until(&mut events, |e| {
            matches!(e, UiEvent::SessionRestored { .. })
        })
        .await;
        receive_until(&mut events, |e| {
            if let UiEvent::PermissionMode(mode) = e {
                assert_eq!(*mode, PermissionMode::AutoEdit);
                true
            } else {
                false
            }
        })
        .await;
        assert_eq!(shared.permissions.mode(), PermissionMode::AutoEdit);
        for mode in PermissionMode::ALL {
            session.send(SessionCommand::SetPermissionMode(mode));
            receive_until(&mut events, |e| matches!(e, UiEvent::PermissionMode(_))).await;
            let mut reopened = Agent::new(&cfg).await.unwrap();
            reopened.restore_session(&id).unwrap();
            assert_eq!(reopened.shared.permissions.mode(), mode);
            assert!(reopened.restore_session("missing").is_err());
            assert_eq!(reopened.shared.permissions.mode(), mode);
        }
        session.send(SessionCommand::Shutdown);
        while events.recv().await.is_some() {}
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn saved_permission_on_legacy_session_preserves_conversation() {
        use crate::config::PermissionMode;
        let root =
            std::env::temp_dir().join(format!("koala-legacy-permission-{}", uuid::Uuid::new_v4()));
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.agent.session_dir = root.clone();
        cfg.agent.memory_file = root.join("memory.md");
        let agent = Agent::new(&cfg).await.unwrap();
        let id = agent.session_id().to_owned();
        agent
            .transcript
            .append_turn("old question", "old answer")
            .unwrap();
        let (session, mut events) = spawn(agent);
        receive_until(&mut events, |e| matches!(e, UiEvent::PermissionMode(_))).await;
        session.send(SessionCommand::SetPermissionMode(PermissionMode::NeverAsk));
        receive_until(&mut events, |e| matches!(e, UiEvent::PermissionMode(_))).await;
        let mut reopened = Agent::new(&cfg).await.unwrap();
        reopened.restore_session(&id).unwrap();
        assert_eq!(reopened.shared.permissions.mode(), PermissionMode::NeverAsk);
        assert_eq!(reopened.history.len(), 2);
        assert_eq!(reopened.history[0].content.as_deref(), Some("old question"));
        session.send(SessionCommand::Shutdown);
        while events.recv().await.is_some() {}
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn permission_switch_updates_shared_state_and_survives_new_session() {
        use crate::config::PermissionMode;
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();

        cfg.permissions.mode = PermissionMode::AskWhenNeed;
        let agent = Agent::new(&cfg).await.unwrap();
        let shared = agent.shared.clone();
        let (session, mut events) = spawn(agent);
        receive_until(&mut events, |e| {
            matches!(e, UiEvent::PermissionMode(PermissionMode::AskWhenNeed))
        })
        .await;
        for mode in [PermissionMode::NeverAsk, PermissionMode::Normal] {
            session.send(SessionCommand::SetPermissionMode(mode));
            receive_until(
                &mut events,
                |e| matches!(e, UiEvent::PermissionMode(value) if *value == mode),
            )
            .await;
            assert_eq!(shared.permissions.mode(), mode);
        }
        session.send(SessionCommand::NewSession);
        receive_until(&mut events, |e| matches!(e, UiEvent::SessionReset)).await;
        assert_eq!(shared.permissions.mode(), PermissionMode::Normal);
        session.send(SessionCommand::Shutdown);
    }

    #[tokio::test]
    async fn streamed_usage_reaches_frontend_without_accumulating_requests() {
        use crate::test_support::{MockLlm, stream};
        let response = |prompt, completion| {
            (
                200,
                format!(
                    "data: {{\"choices\":[{{\"delta\":{{\"content\":\"ok\"}},\"finish_reason\":\"stop\"}}]}}\n\ndata: {{\"choices\":[],\"usage\":{{\"prompt_tokens\":{prompt},\"completion_tokens\":{completion}}}}}\n\ndata: [DONE]\n\n"
                ),
            )
        };
        let mut mock = MockLlm::start(vec![
            response(110_000, 10_000),
            response(20_000, 500),
            stream(serde_json::json!({"content": "no usage"})),
        ])
        .await;
        let root = std::env::temp_dir().join(format!("koala-usage-{}", uuid::Uuid::new_v4()));
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.llm.base_url = mock.url.clone();

        cfg.agent.memory_file = root.join("memory.md");
        cfg.agent.session_dir = root.join("sessions");
        let (session, mut events) = spawn(Agent::new(&cfg).await.unwrap());
        for (expected, breakdown) in [
            (Some(120_000), Some((110_000, 10_000))),
            (Some(20_500), Some((20_000, 500))),
            (None, None),
        ] {
            session.send(SessionCommand::Submit("hi".into()));
            let usage = timeout(Duration::from_secs(3), async {
                let mut usage = Vec::new();
                let mut token_usage = None;
                loop {
                    match events.recv().await.expect("event stream closed") {
                        UiEvent::ContextUsage(value) => usage.push(value),
                        UiEvent::TokenUsage(value) => {
                            token_usage = Some((value.prompt_tokens, value.completion_tokens));
                        }
                        UiEvent::Done => break,
                        UiEvent::Error(error) => panic!("{error}"),
                        _ => {}
                    }
                }
                assert_eq!(token_usage, breakdown);
                usage
            })
            .await
            .unwrap();
            assert_eq!(usage.first(), Some(&None));
            assert_eq!(usage.last(), Some(&expected));
            assert_eq!(
                mock.request().await["stream_options"]["include_usage"],
                true
            );
        }
        session.send(SessionCommand::Shutdown);
        while events.recv().await.is_some() {}
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn effort_selection_updates_requests_and_rejects_unsupported_values() {
        use crate::test_support::{MockLlm, stream};
        let mut mock = MockLlm::start(vec![stream(serde_json::json!({"content": "ok"})); 2]).await;
        let root = std::env::temp_dir().join(format!("koala-effort-{}", uuid::Uuid::new_v4()));
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.llm.base_url = mock.url.clone();
        cfg.llm.reasoning_efforts = vec!["low".into(), "high".into()];
        cfg.lang = crate::i18n::Lang::Zh;
        cfg.llm.context_window = std::num::NonZeroU64::new(128000);

        cfg.agent.memory_file = root.join("memory.md");
        cfg.agent.session_dir = root.join("sessions");
        let (session, mut events) = spawn(Agent::new(&cfg).await.unwrap());
        receive_until(&mut events, |e| matches!(e, UiEvent::ModelSettings { reasoning_effort: Some(v), context_window: Some(128000), .. } if v == "low")).await;
        session.send(SessionCommand::Submit("first".into()));
        receive_until(&mut events, |e| matches!(e, UiEvent::Done)).await;
        let request = mock.request().await;
        assert_eq!(request["reasoning_effort"], "low");
        assert!(request.get("context_window").is_none());
        session.send(SessionCommand::SetReasoningEffort("high".into()));
        receive_until(&mut events, |e| matches!(e, UiEvent::ModelSettings { reasoning_effort: Some(v), .. } if v == "high")).await;
        session.send(SessionCommand::SetReasoningEffort("invalid".into()));
        receive_until(
            &mut events,
            |e| matches!(e, UiEvent::Note(v) if v.contains("不支持")),
        )
        .await;
        session.send(SessionCommand::NewSession);
        receive_until(&mut events, |e| matches!(e, UiEvent::SessionReset)).await;
        session.send(SessionCommand::Submit("second".into()));
        receive_until(&mut events, |e| matches!(e, UiEvent::Done)).await;
        assert_eq!(mock.request().await["reasoning_effort"], "high");
        session.send(SessionCommand::Shutdown);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn model_switch_updates_requests_capabilities_and_preserves_history() {
        use crate::config::ModelConfig;
        use crate::test_support::{MockLlm, stream};
        let mut mock = MockLlm::start(vec![stream(serde_json::json!({"content": "ok"})); 3]).await;
        let root = std::env::temp_dir().join(format!("koala-model-{}", uuid::Uuid::new_v4()));
        let mut cfg = Config::default();
        cfg.llm.model = "reasoner".into();
        cfg.llm.base_url = mock.url.clone();
        cfg.llm.reasoning_efforts = vec!["low".into(), "high".into()];
        cfg.llm.reasoning_effort = Some("high".into());
        cfg.llm.context_window = std::num::NonZeroU64::new(128000);
        cfg.llm.models = vec![ModelConfig {
            model: "plain".into(),
            ..Default::default()
        }];
        cfg.lang = crate::i18n::Lang::Zh;

        cfg.agent.memory_file = root.join("memory.md");
        cfg.agent.session_dir = root.join("sessions");
        let (session, mut events) = spawn(Agent::new(&cfg).await.unwrap());
        session.send(SessionCommand::Submit("KEEP_THIS_HISTORY".into()));
        receive_until(&mut events, |e| matches!(e, UiEvent::Done)).await;
        assert_eq!(mock.request().await["model"], "reasoner");
        session.send(SessionCommand::SelectModel("plain".into()));
        receive_until(&mut events, |e| matches!(e, UiEvent::ModelSettings { model, reasoning_efforts, reasoning_effort: None, context_window: None, .. } if model == "plain" && reasoning_efforts.is_empty())).await;
        session.send(SessionCommand::SelectModel("unknown".into()));
        receive_until(
            &mut events,
            |e| matches!(e, UiEvent::Note(v) if v.contains("未配置的模型")),
        )
        .await;
        session.send(SessionCommand::SetReasoningEffort("high".into()));
        receive_until(
            &mut events,
            |e| matches!(e, UiEvent::Note(v) if v.contains("未配置思考档位")),
        )
        .await;
        session.send(SessionCommand::Submit("next".into()));
        receive_until(&mut events, |e| matches!(e, UiEvent::Done)).await;
        let request = mock.request().await;
        assert_eq!(request["model"], "plain");
        assert!(request.get("reasoning_effort").is_none());
        assert!(
            request["messages"]
                .to_string()
                .contains("KEEP_THIS_HISTORY")
        );
        session.send(SessionCommand::SelectModel("reasoner".into()));
        receive_until(&mut events, |e| matches!(e, UiEvent::ModelSettings { model, reasoning_effort: Some(v), context_window: Some(128000), .. } if model == "reasoner" && v == "high")).await;
        session.send(SessionCommand::Submit("back".into()));
        receive_until(&mut events, |e| matches!(e, UiEvent::Done)).await;
        let request = mock.request().await;
        assert_eq!(request["model"], "reasoner");
        assert_eq!(request["reasoning_effort"], "high");
        session.send(SessionCommand::Shutdown);
        std::fs::remove_dir_all(root).unwrap();
    }

    async fn request(socket: &mut TcpStream) -> serde_json::Value {
        let mut data = Vec::new();
        loop {
            let mut buf = [0; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            assert!(n > 0);
            data.extend_from_slice(&buf[..n]);
            if let Some(end) = data.windows(4).position(|s| s == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&data[..end]).to_lowercase();
                let len: usize = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap()
                    .trim()
                    .parse()
                    .unwrap();
                if data.len() >= end + 4 + len {
                    return serde_json::from_slice(&data[end + 4..end + 4 + len]).unwrap();
                }
            }
        }
    }

    async fn receive_until(
        rx: &mut mpsc::UnboundedReceiver<UiEvent>,
        predicate: impl Fn(&UiEvent) -> bool,
    ) {
        timeout(Duration::from_secs(3), async {
            loop {
                let ev = rx.recv().await.expect("event stream closed");
                if let UiEvent::Error(e) = &ev {
                    panic!("{e}");
                }
                if predicate(&ev) {
                    break;
                }
            }
        })
        .await
        .expect("session did not respond");
    }

    #[tokio::test]
    async fn cancellation_acknowledges_even_after_turn_completed() {
        use crate::test_support::{MockLlm, stream};
        let _mock = MockLlm::start(vec![stream(serde_json::json!({"content": "done"}))]).await;
        let root = std::env::temp_dir().join(format!("koala-cancel-ack-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.llm.base_url = _mock.url.clone();
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.md");
        let (handle, mut events) = spawn(Agent::new(&cfg).await.unwrap());
        handle.send(SessionCommand::Submit("finish first".into()));
        receive_until(&mut events, |ev| matches!(ev, UiEvent::Done)).await;
        handle.send(SessionCommand::Cancel);
        receive_until(&mut events, |ev| matches!(ev, UiEvent::Cancelled)).await;
        handle.send(SessionCommand::Shutdown);
        std::fs::remove_dir_all(root).unwrap();
    }

    async fn interrupted_stream(reset: bool) {
        let root =
            std::env::temp_dir().join(format!("koala-session-test-{}", uuid::Uuid::new_v4()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.llm.base_url = format!("http://{}", listener.local_addr().unwrap());
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.md");
        let (handle, mut events) = spawn(Agent::new(&cfg).await.unwrap());
        handle.send(SessionCommand::Submit("first prompt".into()));
        let (mut first, _) = timeout(Duration::from_secs(3), listener.accept())
            .await
            .unwrap()
            .unwrap();
        request(&mut first).await;
        first.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"partial progress\"}}]}\n\n").await.unwrap();
        receive_until(&mut events, |ev| matches!(ev, UiEvent::Text(_))).await;
        handle.send(if reset {
            SessionCommand::NewSession
        } else {
            SessionCommand::Cancel
        });
        receive_until(&mut events, |ev| {
            if reset {
                matches!(ev, UiEvent::SessionReset)
            } else {
                matches!(ev, UiEvent::Cancelled)
            }
        })
        .await;
        // A late chunk from the old connection must not enter the next turn.
        let _ = first
            .write_all(b"data: {\"choices\":[{\"delta\":{\"content\":\"STALE\"}}]}\n\n")
            .await;
        handle.send(SessionCommand::Submit("second prompt".into()));
        let (mut second, _) = timeout(Duration::from_secs(3), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let body = request(&mut second).await;
        let text = body["messages"].to_string();
        assert_eq!(text.contains("first prompt"), !reset);
        assert_eq!(text.contains("partial progress"), !reset);
        assert!(!text.contains("STALE"));
        let response =
            "data: {\"choices\":[{\"delta\":{\"content\":\"finished\"}}]}\n\ndata: [DONE]\n\n";
        second.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await.unwrap();
        receive_until(&mut events, |ev| matches!(ev, UiEvent::Done)).await;
        handle.send(SessionCommand::Shutdown);
        timeout(Duration::from_secs(3), async {
            while events.recv().await.is_some() {}
        })
        .await
        .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn cancel_stream_then_continue_preserves_partial_progress() {
        interrupted_stream(false).await;
    }

    #[tokio::test]
    async fn new_session_during_stream_discards_old_context_and_events() {
        interrupted_stream(true).await;
    }
    #[tokio::test]
    async fn task_controls_remain_responsive_while_foreground_owns_agent() {
        let root = std::env::temp_dir().join(format!("koala-controls-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let marker = root.join("started");
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.md");
        cfg.hooks.turn_start = vec![format!("touch '{}'; sleep 30", marker.display())];
        let agent = Agent::new(&cfg).await.unwrap();
        let background = agent.background.clone();
        let id = background.register("task", "pending");
        background.attach(id, tokio::spawn(std::future::pending::<()>()));
        let (handle, mut events) = spawn(agent);
        handle.send(SessionCommand::Submit("busy foreground".into()));
        timeout(Duration::from_secs(3), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        handle.send(SessionCommand::ShowGraph);
        receive_until(&mut events, |ev| matches!(ev, UiEvent::Graph(graph) if graph.nodes.iter().any(|n| n.status == super::super::graph::Status::Running))).await;
        handle.send(SessionCommand::ShowTasks);
        receive_until(
            &mut events,
            |ev| matches!(ev, UiEvent::Tasks(tasks) if tasks.iter().any(|t| t.id == id)),
        )
        .await;
        handle.send(SessionCommand::StopTask(id));
        receive_until(&mut events, |ev| matches!(ev, UiEvent::Tasks(tasks) if tasks.iter().any(|t| t.id == id && t.status == super::super::event::TaskState::Stopped))).await;
        handle.send(SessionCommand::Cancel);
        receive_until(&mut events, |ev| matches!(ev, UiEvent::Cancelled)).await;
        handle.send(SessionCommand::Shutdown);
        timeout(Duration::from_secs(3), async {
            while events.recv().await.is_some() {}
        })
        .await
        .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(test)]
mod ui_tests {
    use super::super::extension_ui::{Snapshot, UiAction};
    use super::*;
    use crate::{config::Config, extensions::*};
    use tokio::time::{Duration, timeout};
    async fn ui(
        rx: &mut mpsc::UnboundedReceiver<UiEvent>,
        check: impl Fn(&Snapshot) -> bool,
    ) -> Snapshot {
        timeout(Duration::from_secs(5), async {
            loop {
                if let Some(UiEvent::ExtensionUi(s)) = rx.recv().await
                    && check(&s)
                {
                    return s;
                }
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn ui_callbacks_work_while_agent_busy_and_remount_on_new_session() {
        let root = std::env::temp_dir().join(format!("koala-session-ui-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let marker = root.join("started");
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.md");
        cfg.hooks.turn_start = vec![format!("touch '{}'; sleep 30", marker.display())];
        cfg.extensions.manifests = vec![
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("examples/extensions/interactive/extension.toml"),
        ];
        let (handle, mut events) = spawn_tui(Agent::new(&cfg).await.unwrap());
        let mounted = ui(&mut events, |s| !s.surfaces.is_empty()).await;
        let surface = &mounted.surfaces[0];
        let old = UiAction {
            extension: surface.extension.clone(),
            event: UiInputEvent {
                kind: UiEventType::Select,
                event_id: String::new(),
                surface: Some(surface.kind),
                surface_id: Some(surface.id.clone()),
                revision: surface.revision,
                control_id: Some("document".into()),
                value: serde_json::json!("develop"),
            },
        };
        handle.send(SessionCommand::Submit("hold the agent mutex".into()));
        timeout(Duration::from_secs(3), async {
            while !marker.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        handle.send(SessionCommand::ExtensionUi(old.clone()));
        ui(&mut events, |s| {
            s.surfaces.iter().any(|s| {
                s.blocks
                    .iter()
                    .any(|b| matches!(b, UiBlock::Markdown { text } if text.contains("JSON")))
            })
        })
        .await;
        handle.send(SessionCommand::NewSession);
        timeout(Duration::from_secs(3), async {
            while !matches!(events.recv().await, Some(UiEvent::SessionReset)) {}
        })
        .await
        .unwrap();
        let fresh = ui(&mut events, |s| !s.surfaces.is_empty()).await;
        assert_ne!(fresh.surfaces[0].revision, old.event.revision);
        handle.send(SessionCommand::ExtensionUi(old));
        timeout(Duration::from_secs(3), async {
            loop {
                if let Some(UiEvent::Note(note)) = events.recv().await
                    && note.contains("stale")
                {
                    break;
                }
            }
        })
        .await
        .unwrap();
        handle.send(SessionCommand::Shutdown);
        timeout(Duration::from_secs(3), async {
            while events.recv().await.is_some() {}
        })
        .await
        .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }
}
