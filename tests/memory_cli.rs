use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Output};

struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("koala-memory-cli-{}", uuid::Uuid::new_v4()));
        for project in ["a", "b"] {
            std::fs::create_dir_all(root.join(project)).unwrap();
            std::fs::write(
                root.join(project).join("config.toml"),
                format!(
                    "[agent]\nmemory_file = {}\n",
                    serde_json::to_string(&root.join("memory.json")).unwrap()
                ),
            )
            .unwrap();
        }
        Self(root)
    }
    fn run(&self, project: &str, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_koala"))
            .current_dir(self.0.join(project))
            .arg("memory")
            .args(args)
            .output()
            .unwrap()
    }
    fn json(&self, project: &str, args: &[&str]) -> Value {
        let output = self.run(project, args);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn cli_curates_without_an_llm_and_preserves_legacy_notes() {
    let w = Workspace::new();
    std::fs::write(w.0.join("memory.md"), "# old work\n163 tests passed").unwrap();
    let show = w.run("a", &["show"]);
    assert!(show.status.success());
    let text = String::from_utf8(show.stdout).unwrap();
    assert!(text.contains("legacy_not_imported"));
    assert!(!text.contains("163 tests"));
    assert!(!w.0.join("memory.json").exists());
    w.json(
        "a",
        &[
            "set",
            "language",
            "--kind",
            "preference",
            "--summary",
            "中文回答",
            "--details",
            "用户明确要求",
        ],
    );
    assert_eq!(w.json("a", &["get", "language"])["details"], "用户明确要求");
    assert_eq!(w.json("b", &["list"]), serde_json::json!([]));
    w.json(
        "a",
        &[
            "set",
            "language",
            "--kind",
            "preference",
            "--summary",
            "English replies",
        ],
    );
    let items = w.json("a", &["list"]);
    assert_eq!(items.as_array().unwrap().len(), 1);
    assert_eq!(items[0]["summary"], "English replies");
    assert!(
        items[0]["source"]
            .as_str()
            .unwrap()
            .starts_with("user:koala memory")
    );
    assert_eq!(
        w.json("a", &["search", "English"])
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(w.json("a", &["forget", "language"])["deleted"], true);
    assert!(!w.run("a", &["get", "language"]).status.success());
    assert_eq!(
        std::fs::read_to_string(w.0.join("memory.md")).unwrap(),
        "# old work\n163 tests passed"
    );
}
