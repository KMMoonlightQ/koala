use koala::{
    agent::{Agent, background::BackgroundManager, event, work::Journal},
    config::{Config, PermissionMode},
    llm::Message,
};
use koala_test_support::{MockLlm, reply, stream};
use serde_json::json;
struct Temp(std::path::PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("koala-audit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn config(root: &Temp, url: &str) -> Config {
    let mut cfg = Config::default();
    cfg.llm.model = "test".into();
    cfg.llm.base_url = url.into();
    cfg.permissions.mode = PermissionMode::NeverAsk;
    cfg.agent.memory_file = root.0.join("memory.json");
    cfg.agent.session_dir = root.0.join("sessions");
    cfg.agent.memory_read = false;
    cfg.agent.memory_write = false;
    cfg
}
fn tool_call(name: &str, id: &str, args: serde_json::Value) -> (u16, String) {
    stream(json!({"tool_calls": [{"index": 0, "id": id,
        "function": {"name": name, "arguments": args.to_string()}}]}))
}

#[tokio::test]
async fn failed_pre_hook_blocks_the_write_and_reports_why() {
    for code in [1, 2, 3] {
        let root = Temp::new();
        let path = root.0.join("SHOULD_BE_BLOCKED");
        let mut mock = MockLlm::start(vec![
            tool_call("write", "c", json!({"path":path,"content":"probe"})),
            stream(json!({"content":"done"})),
        ])
        .await;
        let mut cfg = config(&root, &mock.url);
        cfg.hooks.pre_tool_use = vec![format!("echo veto >&2; exit {code}"), "exit 2".into()];
        let mut agent = Agent::new(&cfg).await.unwrap();
        agent.run_turn("probe", event::null_events()).await.unwrap();
        assert!(!path.exists());
        mock.request().await;
        let request = mock.request().await;
        assert!(request["messages"].to_string().contains("veto"));
    }
}

#[tokio::test]
async fn long_tool_loop_compacts_before_sending_and_keeps_protocol_valid() {
    for child in [false, true] {
        let root = Temp::new();
        let path = root.0.join("data.txt");
        std::fs::write(&path, "x".repeat(7000)).unwrap();
        let mut rounds = 0;
        let mut root_started = false;
        let mut mock = MockLlm::respond(move |request| {
            if request["stream"] != true {
                return reply(Message::assistant("compact summary"));
            }
            let is_root = request["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["function"]["name"] == "task");
            if child && is_root {
                if root_started {
                    return stream(json!({"content":"done"}));
                }
                root_started = true;
                return tool_call(
                    "task",
                    "child",
                    json!({"prompt":"read repeatedly", "description":"child budget test"}),
                );
            }
            if rounds == 8 {
                return stream(json!({"content":"done"}));
            }
            rounds += 1;
            tool_call("read", &format!("c{rounds}"), json!({"path":path}))
        })
        .await;
        let cfg = config(&root, &mock.url);
        let mut agent = Agent::new(&cfg).await.unwrap();
        assert_eq!(
            agent
                .run_turn("read repeatedly", event::null_events())
                .await
                .unwrap(),
            "done"
        );
        let (mut streams, mut summaries) = (0, 0);
        while streams < if child { 11 } else { 9 } {
            let r = mock.request().await;
            if r["stream"] != true {
                summaries += 1;
                assert!(r["messages"].to_string().len() + 4096 <= cfg.agent.compact_threshold);
                continue;
            }
            streams += 1;
            let bytes = r["messages"].to_string().len() + r["tools"].to_string().len() + 4096;
            assert!(bytes <= cfg.agent.compact_threshold, "{bytes}");
            let mut pending = std::collections::HashSet::new();
            for m in r["messages"].as_array().unwrap() {
                if let Some(calls) = m["tool_calls"].as_array() {
                    for call in calls {
                        pending.insert(call["id"].as_str().unwrap());
                    }
                }
                if let Some(id) = m["tool_call_id"].as_str() {
                    assert!(pending.remove(id));
                }
            }
            assert!(pending.is_empty());
        }
        assert!(summaries > 0);
    }
}

#[tokio::test]
async fn oversized_input_is_rejected_without_a_model_request() {
    let root = Temp::new();
    let mock = MockLlm::start(vec![]).await;
    let mut agent = Agent::new(&config(&root, &mock.url)).await.unwrap();
    let result = agent
        .run_turn(&"x".repeat(50000), event::null_events())
        .await;
    assert!(matches!(
        result,
        Err(koala::agent::AgentError::ContextBudget { .. })
    ));
}

#[test]
fn background_index_is_bounded_and_all_results_are_readable_by_page() {
    let bg = BackgroundManager::default();
    let mut ids = vec![];
    for _ in 0..100 {
        let id = bg.register("task", &"中\n".repeat(100));
        bg.finish(id, true, "中".repeat(5000));
        ids.push(id);
    }
    assert!(bg.result_context().len() < 8192);
    assert!(!bg.result_context().contains(&"中".repeat(100)));
    let mut offset = 0;
    let mut listed = vec![];
    loop {
        let page = bg.task_page(offset, 12);
        listed.extend(
            page["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["id"].as_u64().unwrap() as usize),
        );
        match page["next_offset"].as_u64() {
            Some(next) => offset = next as usize,
            None => break,
        }
    }
    ids.reverse();
    assert_eq!(listed, ids);
    let mut offset = 0;
    let mut output = String::new();
    loop {
        let page = bg.read_output(ids[0], offset).unwrap();
        output.push_str(page["output"].as_str().unwrap());
        match page["next_offset"].as_u64() {
            Some(next) => offset = next as usize,
            None => break,
        }
    }
    assert_eq!(output, "中".repeat(5000));
    assert!(bg.read_output(ids[0], 1).is_err());
    let root = Temp::new();
    let other = bg.for_session(Journal::new(root.0.join("other.work")), vec![]);
    assert!(other.read_output(ids[0], 0).is_err());
}

#[test]
fn journal_context_growth_is_linear_and_round_trips_after_restart() {
    let root = Temp::new();
    let journal = Journal::new(root.0.join("state.work"));
    let mut messages = vec![];
    for _ in 0..100 {
        messages.push(Message::user("x".repeat(1000)));
        journal.context(&messages, &[]).unwrap();
    }
    let file = std::fs::metadata(&journal.path).unwrap().len();
    let live = serde_json::to_vec(&messages).unwrap().len();
    assert!(file < live as u64 * 2, "disk={file}, live={live}");
    let reopened = Journal::new(journal.path.clone());
    assert_eq!(
        serde_json::to_value(reopened.load().unwrap().unwrap().messages).unwrap(),
        serde_json::to_value(&messages).unwrap()
    );
    let compacted = vec![Message::user("summary"), Message::assistant("ok")];
    reopened.context(&compacted, &[]).unwrap();
    messages.push(Message::assistant("continued by another writer"));
    journal.context(&messages, &[]).unwrap();
    assert_eq!(
        serde_json::to_value(reopened.load().unwrap().unwrap().messages).unwrap(),
        serde_json::to_value(&messages).unwrap()
    );
}

#[test]
fn streamed_text_is_batched_and_flushed_at_tool_boundary() {
    use koala::agent::work::Trace;
    let root = Temp::new();
    let journal = Journal::new(root.0.join("state.work"));
    journal.trace(Trace::User("hello".into())).unwrap();
    for _ in 0..100 {
        journal.trace(Trace::Text("x".into())).unwrap();
    }
    journal.trace(Trace::Note("boundary".into())).unwrap();
    let disk = std::fs::read_to_string(&journal.path).unwrap();
    assert!(disk.lines().count() < 10);
    let reopened = Journal::new(journal.path.clone());
    let restored = reopened.load().unwrap().unwrap();
    assert_eq!(
        restored.messages[1].content.as_deref(),
        Some("x".repeat(100).as_str())
    );
}

#[tokio::test]
async fn failed_automatic_summary_stops_requests_and_preserves_tool_progress() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let root = Temp::new();
    let path = root.0.join("data.txt");
    std::fs::write(&path, "x".repeat(7000)).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mock = MockLlm::respond(move |r| {
        if r["stream"] != true {
            return (500, "summary unavailable".into());
        }
        let round = seen.fetch_add(1, Ordering::SeqCst);
        tool_call("read", &format!("c{round}"), json!({"path":path}))
    })
    .await;
    let cfg = config(&root, &mock.url);
    let mut agent = Agent::new(&cfg).await.unwrap();
    let result = agent
        .run_turn("read repeatedly", event::null_events())
        .await;
    assert!(matches!(result, Err(koala::agent::AgentError::Llm(_))));
    let path = std::fs::read_dir(&cfg.agent.session_dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|p| p.extension().is_some_and(|ext| ext == "work"))
        .unwrap();
    let saved = Journal::new(path).load().unwrap().unwrap();
    assert_eq!(
        saved.messages.iter().filter(|m| m.role == "tool").count(),
        calls.load(Ordering::SeqCst)
    );
    assert!(!saved.messages.iter().any(|m| {
        m.content
            .as_deref()
            .unwrap_or_default()
            .contains("[早前对话摘要]")
    }));
}
