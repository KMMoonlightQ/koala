use crate::config::PermissionsConfig;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Policy {
    Allow,
    Ask,
    Deny,
}

#[derive(Debug, Clone)]
pub struct Permissions {
    default: Policy,
    allow: HashSet<String>,
    deny: HashSet<String>,
}

impl Permissions {
    pub fn new(cfg: &PermissionsConfig) -> Self {
        Self {
            default: match cfg.default.as_str() {
                "allow" => Policy::Allow,
                "deny" => Policy::Deny,
                _ => Policy::Ask,
            },
            allow: cfg.allow.iter().cloned().collect(),
            deny: cfg.deny.iter().cloned().collect(),
        }
    }

    /// allow beats deny beats default.
    pub fn check(&self, tool: &str) -> Policy {
        if self.allow.contains(tool) {
            Policy::Allow
        } else if self.deny.contains(tool) {
            Policy::Deny
        } else {
            self.default
        }
    }
}

/// Tools that mutate state; refused outright while plan mode is on.
pub fn is_write_tool(name: &str) -> bool {
    matches!(name, "bash" | "remember")
}

/// Tools usable in plan mode.
pub fn is_plan_mode_tool(name: &str) -> bool {
    matches!(name, "todo_write" | "skill" | "task")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn perms(default: &str, allow: &[&str], deny: &[&str]) -> Permissions {
        Permissions::new(&PermissionsConfig {
            default: default.into(),
            allow: allow.iter().map(|s| s.to_string()).collect(),
            deny: deny.iter().map(|s| s.to_string()).collect(),
        })
    }

    #[test]
    fn allow_beats_deny_beats_default() {
        let p = perms("ask", &["bash"], &["bash", "task"]);
        assert_eq!(p.check("bash"), Policy::Allow);
        assert_eq!(p.check("task"), Policy::Deny);
        assert_eq!(p.check("other"), Policy::Ask);
    }

    #[test]
    fn default_policy_parsing() {
        assert_eq!(perms("allow", &[], &[]).check("x"), Policy::Allow);
        assert_eq!(perms("deny", &[], &[]).check("x"), Policy::Deny);
        assert_eq!(perms("garbage", &[], &[]).check("x"), Policy::Ask);
    }

    #[test]
    fn write_tools_blocked_in_plan_mode() {
        assert!(is_write_tool("bash"));
        assert!(is_plan_mode_tool("todo_write"));
        assert!(!is_plan_mode_tool("memory_write"));
    }
}
