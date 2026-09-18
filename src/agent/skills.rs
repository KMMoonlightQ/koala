use crate::markdown;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub path: PathBuf,
    pub body: String,
}

#[derive(Debug, Default)]
pub struct Skills {
    pub list: Vec<Skill>,
}

impl Skills {
    /// Scan `./skills/*/SKILL.md` and `<config dir>/koala/skills/*/SKILL.md`.
    pub fn load() -> Self {
        let mut roots = vec![PathBuf::from("skills")];
        if let Some(dir) = dirs::config_dir() {
            roots.push(dir.join("koala").join("skills"));
        }
        Self::load_from(&roots)
    }

    pub fn load_from(roots: &[PathBuf]) -> Self {
        let mut list = Vec::new();
        for root in roots {
            scan_dir(root, &mut list);
        }
        list.sort_by(|a, b| a.name.cmp(&b.name));
        list.dedup_by(|a, b| a.name == b.name);
        Self { list }
    }

    pub fn get(&self, name: &str) -> Option<&Skill> {
        self.list.iter().find(|s| s.name == name)
    }

    /// name — description, one per line, for the UI.
    pub fn listing(&self) -> String {
        self.list
            .iter()
            .map(|s| format!("- {}: {}", s.name, s.description))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn prompt_listing(&self) -> String {
        use super::prompt::escape_xml;
        let skills = self.list.iter().map(|skill| format!(
            "<skill>\n<name>{}</name>\n<description>{}</description>\n<location>{}</location>\n</skill>",
            escape_xml(&skill.name), escape_xml(&skill.description), escape_xml(&skill.path.to_string_lossy())
        )).collect::<Vec<_>>().join("\n");
        format!("<available_skills>\n{skills}\n</available_skills>")
    }
}

fn scan_dir(root: &Path, out: &mut Vec<Skill>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let file = entry.path().join("SKILL.md");
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let parsed =
            markdown::parse(&text).unwrap_or_else(|_| markdown::ParsedMarkdown::plain(&text));
        let fallback = entry.file_name().to_string_lossy().to_string();
        let (name, description) = parsed
            .frontmatter
            .map(|f| {
                (
                    f.name.unwrap_or(fallback.clone()),
                    f.description.unwrap_or_default(),
                )
            })
            .unwrap_or((fallback, String::new()));
        out.push(Skill {
            name,
            description,
            path: std::fs::canonicalize(&file).unwrap_or(file),
            body: parsed.body,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_skill_files_with_frontmatter() {
        let dir = std::env::temp_dir().join(format!("koala-skills-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("skills/review")).unwrap();
        std::fs::write(
            dir.join("skills/review/SKILL.md"),
            "---\nname: review\ndescription: 代码审查\n---\n\n按清单审查。\n",
        )
        .unwrap();
        std::fs::create_dir_all(dir.join("skills/plain")).unwrap();
        std::fs::write(dir.join("skills/plain/SKILL.md"), "没有 frontmatter").unwrap();
        let skills = Skills::load_from(&[dir.join("skills")]);
        let review = skills.get("review").unwrap();
        assert_eq!(review.description, "代码审查");
        assert!(review.body.contains("按清单审查"));
        assert!(skills.get("plain").is_some());
        assert_eq!(skills.listing().lines().count(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_dirs_are_fine() {
        let dir = std::env::temp_dir().join(format!("koala-skills-{}", uuid::Uuid::new_v4()));
        let skills = Skills::load_from(&[dir.join("skills")]);
        assert!(skills.list.is_empty());
    }
}
