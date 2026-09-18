use super::plan::TodoList;
use super::skills::Skills;
use crate::i18n::{self, Key, Lang};

/// Assemble the system prompt from sections; rebuilt before every LLM call.
/// `lang` is the frontend's language, so the prompt itself matches the
/// interface the user is reading instead of being hardcoded to one language.
pub fn build_system(
    agent_memory: &str,
    skills: &Skills,
    todos: &TodoList,
    plan_mode: bool,
    lang: Lang,
) -> String {
    let mut parts = vec![i18n::text(lang, Key::SystemBase).to_string()];
    if plan_mode {
        parts.push(i18n::text(lang, Key::SystemPlan).to_string());
    }
    if !agent_memory.trim().is_empty() {
        parts.push(format!("[agent memory]\n{}", agent_memory.trim()));
    }
    if !skills.list.is_empty() {
        parts.push(format!(
            "[skills]\n{}",
            i18n::fill(lang, Key::SkillsSection, &[("listing", &skills.listing())])
        ));
    }
    if !todos.is_empty() {
        parts.push(format!("[todos]\n{}", todos.render_prompt()));
    }
    parts.join("\n\n")
}

pub fn subagent_system(lang: Lang) -> String {
    i18n::text(lang, Key::SubagentSystem).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_appear_only_when_present() {
        let skills = Skills::default();
        let todos = TodoList::default();
        let p = build_system("", &skills, &todos, false, Lang::En);
        assert!(!p.contains("[agent memory]"));
        assert!(!p.contains("plan mode"));

        let p = build_system("私有笔记", &skills, &todos, true, Lang::Zh);
        assert!(p.contains("plan mode"));
        assert!(p.contains("[agent memory]\n私有笔记"));
    }
}
