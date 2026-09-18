use super::AgentError;
use super::plan::TodoList;
use super::skills::Skills;
use super::tools::ToolCatalog;
use crate::i18n::{self, Key, Lang};
use std::path::{Path, PathBuf};

/// File-backed instructions are loaded once per agent run. Live memory and todos
/// are rendered again before each model request.
#[derive(Default)]
pub struct PromptResources {
    pub cwd: PathBuf,
    pub custom: Option<String>,
    pub append: Option<String>,
    pub project_context: Vec<(PathBuf, String)>,
}

impl PromptResources {
    pub fn load(cwd: PathBuf, global: Option<&Path>) -> Result<Self, AgentError> {
        let project = cwd.join(".koala");
        let mut resources = Self {
            custom: preferred_file(&project, global, "SYSTEM.md")?,
            append: preferred_file(&project, global, "APPEND_SYSTEM.md")?,
            cwd,
            ..Default::default()
        };
        let mut directories: Vec<_> = resources.cwd.ancestors().map(Path::to_path_buf).collect();
        directories.reverse();
        if let Some(global) = global {
            directories.retain(|directory| directory != global);
            directories.insert(0, global.to_path_buf());
        }
        for directory in directories {
            // At each level take the first instruction file, rather than
            // duplicating the same rules in AGENTS.md and CLAUDE.md.
            for name in ["AGENTS.override.md", "AGENTS.md", "CLAUDE.md"] {
                let path = directory.join(name);
                if let Some(content) = optional_file(&path)? {
                    resources.project_context.push((path, content));
                    break;
                }
            }
        }
        Ok(resources)
    }
}

fn optional_file(path: &Path) -> Result<Option<String>, AgentError> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(AgentError::Io {
            path: path.display().to_string(),
            source,
        }),
    }
}

fn preferred_file(
    project: &Path,
    global: Option<&Path>,
    name: &str,
) -> Result<Option<String>, AgentError> {
    if let Some(content) = optional_file(&project.join(name))? {
        return Ok(Some(content));
    }
    match global {
        Some(global) => optional_file(&global.join(name)),
        None => Ok(None),
    }
}

pub struct PromptOptions<'a> {
    pub resources: &'a PromptResources,
    pub catalog: &'a ToolCatalog,
    pub agent_memory: &'a str,
    pub skills: &'a Skills,
    pub todos: &'a TodoList,
    pub plan_mode: bool,
    pub depth: usize,
    pub lang: Lang,
}

/// Default instructions describe only available tools. Custom instructions replace
/// that prefix, while project instructions, live state and mode constraints remain.
pub fn build_system(options: PromptOptions<'_>) -> String {
    let PromptOptions {
        resources,
        catalog,
        agent_memory,
        skills,
        todos,
        plan_mode,
        depth,
        lang,
    } = options;
    let custom = resources.custom.as_deref().filter(|s| !s.trim().is_empty());
    let mut prompt = custom
        .unwrap_or(i18n::text(lang, Key::SystemBase))
        .to_owned();
    if custom.is_none() {
        let (tools, mut rules) = catalog.prompt_sections(lang, plan_mode);
        append_section(&mut prompt, "tools", &tools);
        for rule in i18n::text(lang, Key::SystemRules).lines() {
            if !rules.iter().any(|existing| existing == rule) {
                rules.push(rule.to_owned());
            }
        }
        append_section(
            &mut prompt,
            "rules",
            &rules
                .iter()
                .map(|s| format!("- {s}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
    if let Some(append) = &resources.append {
        append_section(&mut prompt, "addendum", append);
    }
    let context = resources
        .project_context
        .iter()
        .map(|(path, text)| {
            format!(
                "<project_instructions path=\"{}\">\n{text}\n</project_instructions>",
                escape_xml(&path.to_string_lossy())
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    append_section(&mut prompt, "project_context", &context);
    if !skills.list.is_empty() {
        append_section(
            &mut prompt,
            "skills",
            &i18n::fill(
                lang,
                Key::SkillsSection,
                &[("listing", &skills.prompt_listing())],
            ),
        );
    }
    append_section(&mut prompt, "agent_memory", agent_memory);
    if !todos.is_empty() {
        append_section(&mut prompt, "todos", &todos.render_prompt());
    }
    if depth > 0 {
        append_section(
            &mut prompt,
            "subagent",
            i18n::text(lang, Key::SubagentSystem),
        );
    }
    if plan_mode {
        append_section(&mut prompt, "plan_mode", i18n::text(lang, Key::SystemPlan));
    }
    append_section(
        &mut prompt,
        "cwd",
        &escape_xml(&resources.cwd.to_string_lossy()),
    );
    prompt
}

pub fn append_section(prompt: &mut String, name: &str, content: &str) {
    if !content.trim().is_empty() {
        prompt.push_str(&format!("\n\n<{name}>\n{}\n</{name}>", content.trim()));
    }
}

pub(crate) fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::skills::Skill;

    #[test]
    fn tools_and_rules_follow_plan_mode_and_subagent_visibility() {
        let extensions = crate::extensions::Extensions::default();
        let skills = Skills::default();
        let todos = TodoList::default();
        let resources = PromptResources {
            cwd: "/workspace".into(),
            ..Default::default()
        };
        for lang in Lang::ALL {
            for (depth, plan_mode) in [(0, false), (0, true), (1, false), (1, true)] {
                let catalog = ToolCatalog::build(depth, &extensions);
                let prompt = build_system(PromptOptions {
                    resources: &resources,
                    catalog: &catalog,
                    agent_memory: "",
                    skills: &skills,
                    todos: &todos,
                    plan_mode,
                    depth,
                    lang,
                });
                let advertised = catalog.definitions_for_mode(plan_mode);
                for tool in catalog.definitions() {
                    let name = &tool.function.name;
                    assert_eq!(
                        prompt.contains(&format!("- {name}: ")),
                        advertised.iter().any(|tool| &tool.function.name == name)
                    );
                }
                assert_eq!(prompt.contains("- task: "), depth == 0);
                assert_eq!(
                    prompt.contains(i18n::text(lang, Key::ToolTaskRules)),
                    depth == 0
                );
                assert_eq!(
                    prompt.contains(i18n::text(lang, Key::ToolEditRules).lines().next().unwrap()),
                    !plan_mode
                );
                assert_eq!(prompt.contains("<plan_mode>"), plan_mode);
                assert_eq!(prompt.contains("<subagent>"), depth > 0);
                assert!(!prompt.contains("<agent_memory>"));
                assert!(!prompt.contains("<todos>"));
            }
        }
    }

    #[test]
    fn custom_prefix_preserves_context_and_skills_are_metadata_only() {
        let catalog = ToolCatalog::build(0, &crate::extensions::Extensions::default());
        let skills = Skills {
            list: vec![Skill {
                name: "review".into(),
                description: "A & B".into(),
                path: "/skills/review/SKILL.md".into(),
                body: "SECRET_SKILL_BODY".into(),
            }],
        };
        let resources = PromptResources {
            cwd: "/workspace".into(),
            custom: Some("Custom role".into()),
            append: Some("Extra rules".into()),
            project_context: vec![("/workspace/AGENTS.md".into(), "Project rules".into())],
        };
        let prompt = build_system(PromptOptions {
            resources: &resources,
            catalog: &catalog,
            agent_memory: "Private note",
            skills: &skills,
            todos: &TodoList::default(),
            plan_mode: true,
            depth: 1,
            lang: Lang::En,
        });
        assert!(prompt.starts_with("Custom role\n"));
        assert!(!prompt.contains("<tools>"));
        assert!(!prompt.contains("<rules>"));
        for text in [
            "<addendum>",
            "Extra rules",
            "Project rules",
            "<agent_memory>",
            "Private note",
            "<skills>",
            "A &amp; B",
            "<location>/skills/review/SKILL.md</location>",
            "<plan_mode>",
            "<subagent>",
            "<cwd>",
        ] {
            assert!(prompt.contains(text), "{text}");
        }
        assert!(!prompt.contains("SECRET_SKILL_BODY"));
    }

    #[test]
    fn resource_files_use_project_precedence_and_order_ancestor_instructions() {
        let root = std::env::temp_dir().join(format!("koala-prompt-{}", uuid::Uuid::new_v4()));
        let cwd = root.join("project/child");
        let global = root.join("global");
        std::fs::create_dir_all(cwd.join(".koala")).unwrap();
        std::fs::create_dir_all(&global).unwrap();
        for (path, content) in [
            (global.join("SYSTEM.md"), "global role"),
            (global.join("APPEND_SYSTEM.md"), "global extra"),
            (global.join("AGENTS.md"), "global rules"),
            (root.join("project/AGENTS.md"), "parent rules"),
            (cwd.join("AGENTS.override.md"), "child override"),
            (cwd.join("AGENTS.md"), "ignored"),
            (cwd.join(".koala/SYSTEM.md"), "project role"),
            (cwd.join(".koala/APPEND_SYSTEM.md"), "project extra"),
        ] {
            std::fs::write(path, content).unwrap();
        }
        let resources = PromptResources::load(cwd.clone(), Some(&global)).unwrap();
        assert_eq!(resources.custom.as_deref(), Some("project role"));
        assert_eq!(resources.append.as_deref(), Some("project extra"));
        let contexts: Vec<_> = resources
            .project_context
            .iter()
            .filter(|(path, _)| path.starts_with(&root))
            .map(|(_, content)| content.as_str())
            .collect();
        assert_eq!(contexts, ["global rules", "parent rules", "child override"]);
        std::fs::remove_file(cwd.join(".koala/SYSTEM.md")).unwrap();
        std::fs::remove_file(cwd.join(".koala/APPEND_SYSTEM.md")).unwrap();
        let resources = PromptResources::load(cwd.clone(), Some(&global)).unwrap();
        assert_eq!(resources.custom.as_deref(), Some("global role"));
        assert_eq!(resources.append.as_deref(), Some("global extra"));
        std::fs::write(cwd.join(".koala/SYSTEM.md"), [0xff]).unwrap();
        assert!(PromptResources::load(cwd, Some(&global)).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
