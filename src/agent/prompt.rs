use super::plan::TodoList;
use super::skills::Skills;

const BASE_PROMPT: &str = "你是一个带长期记忆和执行能力的个人助手 Agent，运行在用户的终端里。\n\
工作方式：\n\
- remember 是你的私人记忆文件，用来记值得保留的用户偏好、事实和工作状态。\n\
- 多步骤任务先用 todo_write 拆解并持续更新进度。\n\
- 需要操作本地环境时用 bash；独立的子任务用 task 派给子 Agent。\n\
- 回答简洁直接，用用户的语言。";

const PLAN_MODE_PROMPT: &str = "当前处于 plan mode：只做调查和规划（todo_write / skill / task），\
不要修改任何文件或状态。把计划写进 todo 列表，等用户退出 plan mode 后再执行。";

/// Assemble the system prompt from sections; rebuilt before every LLM call.
pub fn build_system(
    agent_memory: &str,
    skills: &Skills,
    todos: &TodoList,
    plan_mode: bool,
) -> String {
    let mut parts = vec![BASE_PROMPT.to_string()];
    if plan_mode {
        parts.push(PLAN_MODE_PROMPT.to_string());
    }
    if !agent_memory.trim().is_empty() {
        parts.push(format!("[agent memory]\n{}", agent_memory.trim()));
    }
    if !skills.list.is_empty() {
        parts.push(format!(
            "[skills]\n可用 skill，需要时用 skill 工具加载全文：\n{}",
            skills.listing()
        ));
    }
    if !todos.is_empty() {
        parts.push(format!("[todos]\n{}", todos.render_prompt()));
    }
    parts.join("\n\n")
}

pub fn subagent_system() -> String {
    "你是子 Agent，独立完成交给你的子任务。可以用 bash 操作环境。\
     完成后直接给出结果，不要解释过程。"
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_appear_only_when_present() {
        let skills = Skills::default();
        let todos = TodoList::default();
        let p = build_system("", &skills, &todos, false);
        assert!(!p.contains("[agent memory]"));
        assert!(!p.contains("plan mode"));

        let p = build_system("私有笔记", &skills, &todos, true);
        assert!(p.contains("plan mode"));
        assert!(p.contains("[agent memory]\n私有笔记"));
    }
}
