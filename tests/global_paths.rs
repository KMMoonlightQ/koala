use koala::{agent::skills::Skills, config::Config};
use std::process::Command;

#[test]
fn global_resources_are_loaded_from_home() {
    const CHILD: &str = "KOALA_GLOBAL_PATHS_TEST_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
        let cfg = Config::load().unwrap();
        assert_eq!(cfg.agent.memory_file, home.join(".koala/memory.json"));
        assert!(cfg.mcp.servers.contains_key("example"));
        assert!(Skills::load().get("global-test").is_some());
        return;
    }
    let root = std::env::temp_dir().join(format!("koala-global-{}", uuid::Uuid::new_v4()));
    let home = root.join("home");
    std::fs::create_dir_all(home.join(".koala/skills/global-test")).unwrap();
    std::fs::create_dir_all(root.join("project")).unwrap();
    std::fs::write(
        home.join(".koala/config.toml"),
        "[mcp.servers.example]\ncommand = 'unused'\nenabled = false\n",
    )
    .unwrap();
    std::fs::write(
        home.join(".koala/skills/global-test/SKILL.md"),
        "A global test skill",
    )
    .unwrap();
    let result = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "global_resources_are_loaded_from_home",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .current_dir(root.join("project"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let extension = Command::new(env!("CARGO_BIN_EXE_koala"))
        .arg("extension-install")
        .arg(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/extensions/context"))
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .current_dir(root.join("project"))
        .output()
        .unwrap();
    assert!(
        extension.status.success(),
        "{}",
        String::from_utf8_lossy(&extension.stderr)
    );
    assert!(
        home.join(".koala/extensions/context-example/extension.toml")
            .is_file()
    );
    let memory = Command::new(env!("CARGO_BIN_EXE_koala"))
        .args([
            "memory",
            "set",
            "language",
            "--kind",
            "preference",
            "--summary",
            "Chinese",
        ])
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .current_dir(root.join("project"))
        .output()
        .unwrap();
    assert!(
        memory.status.success(),
        "{}",
        String::from_utf8_lossy(&memory.stderr)
    );
    assert!(home.join(".koala/memory.json").is_file());
    assert!(!root.join("project/.koala").exists());
    std::fs::remove_dir_all(root).unwrap();
}
