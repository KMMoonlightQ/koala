use super::Agent;
use super::event::{EventSender, SessionCommand, UiEvent};
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
    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = mpsc::unbounded_channel();
    let (mut work_tx, mut work_rx) = mpsc::unbounded_channel();
    let background = agent.background.clone();
    let mut background_count = background.subscribe_count();
    let _ = ev_tx.send(UiEvent::PlanMode(agent.plan_mode()));
    let _ = ev_tx.send(UiEvent::BackgroundCount(*background_count.borrow()));
    let agent = Arc::new(Mutex::new(agent));
    tokio::spawn(async move {
        let mut active: Option<JoinHandle<()>> = None;
        let mut progress = String::new();
        let mut watch_tasks = false;
        loop {
            tokio::select! {
                // Complete and drain an old operation before accepting another.
                biased;
                result = async { active.as_mut().unwrap().await }, if active.is_some() => {
                    active = None;
                    drain(&mut work_rx, &ev_tx, &mut progress);
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
                        stop(&mut active, &agent, &mut work_rx, &ev_tx, &mut progress).await;
                        break;
                    };
                    match cmd {
                        SessionCommand::Cancel => {
                            if stop(&mut active, &agent, &mut work_rx, &ev_tx, &mut progress).await {
                                let _ = ev_tx.send(UiEvent::Cancelled);
                            }
                        }
                        SessionCommand::Shutdown => {
                            stop(&mut active, &agent, &mut work_rx, &ev_tx, &mut progress).await;
                            break;
                        }
                        SessionCommand::NewSession => {
                            stop(&mut active, &agent, &mut work_rx, &ev_tx, &mut progress).await;
                            agent.lock().await.new_session();
                            (work_tx, work_rx) = mpsc::unbounded_channel();
                            progress.clear();
                            let _ = ev_tx.send(UiEvent::SessionReset);
                            let _ = ev_tx.send(UiEvent::PlanMode(agent.lock().await.plan_mode()));
                        }
                        SessionCommand::Submit(text) if active.is_none() => {
                            progress.clear();
                            let agent = agent.clone();
                            let events = work_tx.clone();
                            active = Some(tokio::spawn(async move {
                                let mut guard = agent.lock().await;
                                if let Err(e) = guard.run_turn(&text, events.clone()).await {
                                    let _ = events.send(UiEvent::Error(e.to_string()));
                                }
                            }));
                        }
                        SessionCommand::Compact if active.is_none() => {
                            progress.clear();
                            let agent = agent.clone();
                            let events = work_tx.clone();
                            active = Some(tokio::spawn(async move {
                                let _ = events.send(UiEvent::Status("压缩上下文中".into()));
                                let ev = match agent.lock().await.compact_now().await {
                                    Ok(true) => UiEvent::Note("上下文已压缩".into()),
                                    Ok(false) => UiEvent::Note("暂无需要压缩的内容".into()),
                                    Err(e) => UiEvent::Error(e.to_string()),
                                };
                                let _ = events.send(ev);
                            }));
                        }
                        SessionCommand::ShowTasks => {
                            watch_tasks = true;
                            let _ = ev_tx.send(UiEvent::Tasks(background.list()));
                        }
                        SessionCommand::HideTasks => watch_tasks = false,
                        SessionCommand::StopTask(id) => {
                            let stopped = background.stop(id).await;
                            if !stopped { let _ = ev_tx.send(UiEvent::Note(format!("任务 #{id} 已结束或不可停止"))); }
                            let _ = ev_tx.send(UiEvent::Tasks(background.list()));
                        }
                        _ if active.is_some() => {
                            let _ = ev_tx.send(UiEvent::Note("正在执行，请先按 Esc 中断".into()));
                        }
                        SessionCommand::TogglePlanMode => {
                            let on = agent.lock().await.toggle_plan_mode();
                            let _ = ev_tx.send(UiEvent::PlanMode(on));
                        }
                        SessionCommand::ShowSkills => {
                            let text = agent.lock().await.skills().listing();
                            let _ = ev_tx.send(UiEvent::Info(text));
                        }
                        _ => {}
                    }
                }
                Ok(()) = background_count.changed() => {
                    let _ = ev_tx.send(UiEvent::BackgroundCount(*background_count.borrow_and_update()));
                    if watch_tasks { let _ = ev_tx.send(UiEvent::Tasks(background.list())); }
                }
                Some(ev) = work_rx.recv() => forward(ev, &ev_tx, &mut progress),
            }
        }
    });
    (SessionHandle { tx: cmd_tx }, ev_rx)
}

fn forward(ev: UiEvent, events: &EventSender, progress: &mut String) {
    match &ev {
        UiEvent::Text(text) => progress.push_str(text),
        UiEvent::ToolStart { name, summary, .. } => {
            progress.push_str(&format!("\n工具 {name}({summary})\n"))
        }
        UiEvent::ToolEnd { output, .. } => progress.push_str(&format!(
            "\n结果：{}\n",
            super::tools::result_preview(output)
        )),
        _ => {}
    }
    let _ = events.send(ev);
}

fn drain(rx: &mut mpsc::UnboundedReceiver<UiEvent>, events: &EventSender, progress: &mut String) {
    while let Ok(ev) = rx.try_recv() {
        forward(ev, events, progress);
    }
}

async fn stop(
    active: &mut Option<JoinHandle<()>>,
    agent: &Arc<Mutex<Agent>>,
    rx: &mut mpsc::UnboundedReceiver<UiEvent>,
    events: &EventSender,
    progress: &mut String,
) -> bool {
    let Some(task) = active.take() else {
        return false;
    };
    task.abort();
    let _ = task.await;
    drain(rx, events, progress);
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

    async fn interrupted_stream(reset: bool) {
        let root = std::env::temp_dir().join(format!("kb-session-test-{}", uuid::Uuid::new_v4()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.llm.base_url = format!("http://{}", listener.local_addr().unwrap());
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.md");
        let (handle, mut events) = spawn(Agent::new(&cfg).unwrap());
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
        let root = std::env::temp_dir().join(format!("kb-controls-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let marker = root.join("started");
        let mut cfg = Config::default();
        cfg.llm.model = "test".into();
        cfg.agent.session_dir = root.join("sessions");
        cfg.agent.memory_file = root.join("memory.md");
        cfg.hooks.turn_start = vec![format!("touch '{}'; sleep 30", marker.display())];
        let agent = Agent::new(&cfg).unwrap();
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
