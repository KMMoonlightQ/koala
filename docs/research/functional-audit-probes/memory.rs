use koala_memory::{FileStore, distill::distillation_text, dream, llm::Message};
use koala_test_support::{MockLlm, reply};
use serde_json::json;
struct Temp(std::path::PathBuf);
impl Temp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("koala-audit-ext-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[test]
fn distill_discards_final_correction_after_prefix_budget() {
    let root = Temp::new();
    let path = root.0.join("session.jsonl");
    std::fs::write(
        &path,
        format!(
            "{}\n{}\n",
            json!({"role":"assistant","content":"old assumption ".repeat(1000)}),
            json!({"role":"user","content":"FINAL_CORRECTION: the previous assumption was wrong"})
        ),
    )
    .unwrap();
    let projected = distillation_text(&path, 12000).unwrap();
    assert_eq!(projected.len(), 12000);
    assert!(!projected.contains("FINAL_CORRECTION"));
    println!("DISTILL: final user correction absent from 12000-byte projection");
}
#[tokio::test]
async fn dream_marks_entire_source_done_after_capped_extraction() {
    let root = Temp::new();
    let mut store = FileStore::open(&root.0).unwrap();
    store
        .write_file(
            "daily/a.md",
            "Topic A: preference A\nTopic B: independent preference B",
        )
        .unwrap();
    let write:Message=serde_json::from_value(json!({"role":"assistant","tool_calls":[{"id":"c","type":"function","function":{"name":"memory_write","arguments":json!({"path":"digest/wiki/a","name":"a","content":"Topic A only"}).to_string()}}]})).unwrap();
    let mock=MockLlm::start(vec![reply(Message::assistant(json!([{"name":"a","bucket":"wiki","summary":"Topic A only","paths":["daily/a.md"]},{"name":"b","bucket":"wiki","summary":"Topic B","paths":["daily/a.md"]}]).to_string())),reply(write),reply(Message::assistant("done"))]).await;
    let first = dream(&mock.client, &mut store, 1).await.unwrap();
    assert_eq!(first.integrated.len(), 1);
    let second = dream(&mock.client, &mut store, 10).await.unwrap();
    assert_eq!(second.changed, 0);
    assert!(!root.0.join("digest/wiki/b.md").exists());
    println!(
        "DREAM: max_units=1 dropped unit B; later max_units=10 => changed={} (source already checkpointed)",
        second.changed
    );
}
