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
#[tokio::test]
async fn failed_pre_hook_skips_following_blocker_and_still_writes() {
    let root = Temp::new();
    let path = root.0.join("SHOULD_BE_BLOCKED");
    let mock=MockLlm::start(vec![stream(json!({"tool_calls":[{"index":0,"id":"c","function":{"name":"write","arguments":json!({"path":path,"content":"probe"}).to_string()}}]})),stream(json!({"content":"done"}))]).await;
    let mut cfg = config(&root, &mock.url);
    cfg.hooks.pre_tool_use = vec!["exit 1".into(), "exit 2".into()];
    let mut agent = Agent::new(&cfg).await.unwrap();
    agent.run_turn("probe", event::null_events()).await.unwrap();
    assert!(path.exists());
    println!(
        "HOOK: exit 1 followed by exit 2 => write executed={}",
        path.exists()
    );
}
#[tokio::test]
async fn tool_loop_exceeds_threshold_before_any_compaction() {
    let root = Temp::new();
    let path = root.0.join("data.txt");
    std::fs::write(&path, "x".repeat(7000)).unwrap();
    let mut responses = vec![];
    for i in 0..8 {
        responses.push(stream(json!({"tool_calls":[{"index":0,"id":format!("c{i}"),"function":{"name":"read","arguments":json!({"path":path}).to_string()}}]})));
    }
    responses.push(stream(json!({"content":"done"})));
    responses.extend((0..10).map(|_| reply(Message::assistant("compact summary"))));
    let mut mock = MockLlm::start(responses).await;
    let mut cfg = config(&root, &mock.url);
    cfg.agent.compact_threshold = 40000;
    let mut agent = Agent::new(&cfg).await.unwrap();
    agent
        .run_turn("read repeatedly", event::null_events())
        .await
        .unwrap();
    let mut max = 0;
    for _ in 0..9 {
        let r = mock.request().await;
        assert_eq!(r["stream"], true);
        max = max.max(r["messages"].to_string().len());
    }
    assert!(max > 50000);
    println!("COMPACT: threshold=40000; largest request before compaction={max} bytes");
}
#[test]
fn completed_background_results_are_reinjected_without_total_budget() {
    let bg = BackgroundManager::default();
    for i in 0..10 {
        let id = bg.register("task", &format!("job-{i}"));
        bg.finish(id, true, "x".repeat(8000));
    }
    let first = bg.result_context();
    let second = bg.result_context();
    assert_eq!(first, second);
    assert!(second.len() > 80000);
    println!(
        "BACKGROUND: 10 completed tasks => {} bytes on every request",
        second.len()
    );
}
#[test]
fn journal_checkpoints_duplicate_full_history() {
    let root = Temp::new();
    let journal = Journal::new(root.0.join("state.work"));
    let mut messages = vec![];
    for _ in 0..100 {
        messages.push(Message::user("x".repeat(1000)));
        journal.context(&messages, &[]).unwrap();
    }
    let file = std::fs::metadata(&journal.path).unwrap().len();
    let live = serde_json::to_vec(&messages).unwrap().len();
    assert!(file > live as u64 * 40);
    println!("JOURNAL: 100 snapshots => disk={file} bytes, current context={live} bytes");
}
