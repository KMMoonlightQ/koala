use crate::config::{PermissionMode, PermissionsConfig};
use std::collections::HashSet;
use std::sync::RwLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug)]
pub struct Permissions {
    mode: RwLock<PermissionMode>,
    allow: HashSet<String>,
    deny: HashSet<String>,
}

impl Permissions {
    pub fn new(cfg: &PermissionsConfig) -> Self {
        Self {
            mode: RwLock::new(cfg.mode),
            allow: cfg.allow.iter().cloned().collect(),
            deny: cfg.deny.iter().cloned().collect(),
        }
    }

    pub fn mode(&self) -> PermissionMode {
        *self.mode.read().unwrap()
    }

    pub fn set_mode(&self, mode: PermissionMode) {
        *self.mode.write().unwrap() = mode;
    }

    /// Called after argument-rewriting hooks. Unknown tools require approval.
    pub fn check(&self, tool: &str, args: &serde_json::Value, read_only: bool) -> Policy {
        let mode = self.mode();
        if mode == PermissionMode::NeverAsk {
            return Policy::Allow;
        }
        if self.deny.contains(tool) {
            return Policy::Deny;
        }
        if mode == PermissionMode::Normal {
            return Policy::Ask;
        }
        if self.allow.contains(tool)
            || read_only
            || matches!(tool, "todo_write" | "remember" | "skill" | "task")
            || (tool == "bash"
                && args
                    .get("command")
                    .and_then(|v| v.as_str())
                    .is_some_and(read_only_shell))
        {
            Policy::Allow
        } else {
            Policy::Ask
        }
    }
}

/// Deliberately narrow: no shell operators, expansion, quoting, scripts or
/// extensible commands. Anything outside this subset goes through approval.
fn read_only_shell(command: &str) -> bool {
    if command.is_empty()
        || !command
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " /._-".contains(c))
    {
        return false;
    }
    let mut words = command.split_whitespace();
    matches!(
        words.next(),
        Some("pwd" | "ls" | "cat" | "head" | "tail" | "wc")
    )
}

/// Tools usable in plan mode.
pub fn is_plan_mode_tool(name: &str) -> bool {
    matches!(name, "todo_write" | "skill" | "task")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn levels_apply_to_safe_dangerous_and_unknown_tools() {
        let p = Permissions::new(&PermissionsConfig::default());
        let safe = json!({"command": "ls -la src"});
        let dangerous = json!({"command": "rm -rf src"});
        for (tool, args, read_only) in [
            ("bash", &safe, false),
            ("bash", &dangerous, false),
            ("todo_write", &safe, false),
            ("extension", &safe, true),
            ("unknown", &safe, false),
        ] {
            assert_eq!(p.check(tool, args, read_only), Policy::Ask);
        }
        p.set_mode(PermissionMode::AskWhenNeed);
        assert_eq!(p.check("bash", &safe, false), Policy::Allow);
        assert_eq!(p.check("todo_write", &safe, false), Policy::Allow);
        assert_eq!(p.check("extension", &safe, true), Policy::Allow);
        assert_eq!(p.check("bash", &dangerous, false), Policy::Ask);
        assert_eq!(p.check("unknown", &safe, false), Policy::Ask);
        p.set_mode(PermissionMode::NeverAsk);
        assert_eq!(p.check("bash", &dangerous, false), Policy::Allow);
        assert_eq!(p.check("unknown", &safe, false), Policy::Allow);
        p.set_mode(PermissionMode::Normal);
        assert_eq!(p.check("bash", &safe, false), Policy::Ask);
    }

    #[test]
    fn shell_approval_cannot_be_bypassed_with_composition_or_expansion() {
        for command in [
            "ls; rm x",
            "ls\nrm x",
            "cat $(touch x)",
            "ls `touch x`",
            "ls > x",
            "cat a | sh",
            "ls && rm x",
            "ls || rm x",
            "ls &",
            "ls${IFS}x",
            "env ls",
            "bash -c ls",
            "python script.py",
            "curl example.com",
            "sudo ls",
            "",
        ] {
            assert!(!read_only_shell(command), "{command:?}");
        }
        for command in [
            "pwd",
            "ls -la",
            "cat src/main.rs",
            "head -n 10 README.md",
            "wc -l README.md",
        ] {
            assert!(read_only_shell(command), "{command}");
        }
    }

    #[test]
    fn normal_ignores_allow_and_never_ask_overrides_all_rules() {
        let p = Permissions::new(&PermissionsConfig {
            allow: vec!["bash".into()],
            deny: vec!["bash".into()],
            ..Default::default()
        });
        assert_eq!(p.check("bash", &json!({}), false), Policy::Deny);
        p.set_mode(PermissionMode::AskWhenNeed);
        assert_eq!(p.check("bash", &json!({}), false), Policy::Deny);
        p.set_mode(PermissionMode::NeverAsk);
        assert_eq!(p.check("bash", &json!({}), false), Policy::Allow);
        let trusted = Permissions::new(&PermissionsConfig {
            allow: vec!["bash".into()],
            ..Default::default()
        });
        assert_eq!(trusted.check("bash", &json!({}), false), Policy::Ask);
        trusted.set_mode(PermissionMode::AskWhenNeed);
        assert_eq!(trusted.check("bash", &json!({}), false), Policy::Allow);
    }

    #[test]
    fn write_tools_blocked_in_plan_mode() {
        assert!(!is_plan_mode_tool("bash"));
        assert!(!is_plan_mode_tool("remember"));
        assert!(is_plan_mode_tool("todo_write"));
        assert!(!is_plan_mode_tool("memory_write"));
    }
}
