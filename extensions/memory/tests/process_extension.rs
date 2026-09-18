//! Exercise the same install/manifest/process path as a third-party extension.
use koala_extensions::{ExtensionsConfig, Stage, install, load};
use serde_json::{Value, json};
use std::{path::PathBuf, process::Command};

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let dir =
            std::env::temp_dir().join(format!("koala-memory-process-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&dir).unwrap();
        Self(dir)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[tokio::test]
async fn packaged_extension_works_after_source_package_is_removed() {
    let temp = Temp::new();
    let package = temp.0.join("package");
    let workspace = temp.0.join("knowledge");
    let result = Command::new(env!("CARGO_BIN_EXE_koala-memory"))
        .arg("package")
        .arg(&package)
        .arg("--memory-workspace")
        .arg(&workspace)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let manifest = install(&package, &temp.0.join("installed")).unwrap();
    std::fs::remove_dir_all(package).unwrap();
    let cfg = ExtensionsConfig {
        manifests: vec![manifest],
        ..Default::default()
    };
    let extensions = load(&cfg, ["bash".into()]).unwrap();
    assert_eq!(extensions.tools().len(), 3);
    let entries = extensions.tool_entries();
    let call = |name: &str| {
        entries
            .iter()
            .find(|(tool, _)| tool.function.name == name)
            .unwrap()
            .1
            .clone()
    };
    let write = call("memory_write");
    assert!(!write.read_only("memory_write"));
    assert!(call("memory_search").read_only("memory_search"));
    let response = write.execute("memory_write", &json!({"path":"digest/wiki/rust", "name":"Rust", "description":"borrowing", "content":"ownership borrowing"})).await.unwrap();
    assert!(!response.is_error, "{:?}", response.content);
    let context = extensions
        .hook(Stage::TurnStart, json!({"input":"ownership"}))
        .await
        .unwrap()
        .context
        .unwrap();
    assert!(context.contains("ownership borrowing"));
    let read = call("memory_read")
        .execute("memory_read", &json!({"path":"../outside"}))
        .await
        .unwrap();
    assert!(read.is_error);
    std::fs::write(
        workspace.join("digest/wiki/rust.md"),
        "---\nname: Rust\ndescription: updated\n---\nexternalupdate\n",
    )
    .unwrap();
    let search = call("memory_search")
        .execute("memory_search", &json!({"query":"externalupdate"}))
        .await
        .unwrap();
    assert!(search.content.unwrap().contains("externalupdate"));
    drop(extensions);
    let disabled = load(&ExtensionsConfig::default(), Vec::new()).unwrap();
    assert!(disabled.tools().is_empty());
    assert!(
        disabled
            .hook(Stage::TurnStart, json!({"input":"externalupdate"}))
            .await
            .unwrap()
            .context
            .is_none()
    );
    assert!(workspace.join("digest/wiki/rust.md").exists());
}
#[test]
fn unsupported_protocol_version_is_rejected_without_writing() {
    use std::io::Write;
    let temp = Temp::new();
    let workspace = temp.0.join("must-not-exist");
    let mut child = Command::new(env!("CARGO_BIN_EXE_koala-memory"))
        .arg("--workspace")
        .arg(&workspace)
        .arg("serve")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let request: Value =
        json!({"api_version":99,"kind":"tool","name":"memory_write","arguments":{}});
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("unsupported"));
    assert!(!workspace.exists());
}
