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
    pub fn check(&self, tool: &str, automatically_allowed: bool) -> Policy {
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
        if self.allow.contains(tool) || automatically_allowed {
            Policy::Allow
        } else {
            Policy::Ask
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_apply_to_safe_and_unknown_tools() {
        let p = Permissions::new(&PermissionsConfig::default());
        for safe in [true, false] {
            assert_eq!(p.check("tool", safe), Policy::Ask);
        }
        p.set_mode(PermissionMode::AskWhenNeed);
        assert_eq!(p.check("tool", true), Policy::Allow);
        assert_eq!(p.check("tool", false), Policy::Ask);
        p.set_mode(PermissionMode::NeverAsk);
        assert_eq!(p.check("unknown", false), Policy::Allow);
        p.set_mode(PermissionMode::Normal);
        assert_eq!(p.check("tool", true), Policy::Ask);
    }

    #[test]
    fn normal_ignores_allow_and_never_ask_overrides_all_rules() {
        let p = Permissions::new(&PermissionsConfig {
            allow: vec!["bash".into()],
            deny: vec!["bash".into()],
            ..Default::default()
        });
        assert_eq!(p.check("bash", false), Policy::Deny);
        p.set_mode(PermissionMode::AskWhenNeed);
        assert_eq!(p.check("bash", false), Policy::Deny);
        p.set_mode(PermissionMode::NeverAsk);
        assert_eq!(p.check("bash", false), Policy::Allow);
        let trusted = Permissions::new(&PermissionsConfig {
            allow: vec!["bash".into()],
            ..Default::default()
        });
        assert_eq!(trusted.check("bash", false), Policy::Ask);
        trusted.set_mode(PermissionMode::AskWhenNeed);
        assert_eq!(trusted.check("bash", false), Policy::Allow);
    }
}
