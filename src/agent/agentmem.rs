use std::path::PathBuf;

/// Agent-private memory file. Injected into the system prompt every turn;
/// not part of the knowledge base.
pub struct AgentMemory {
    path: PathBuf,
}

const MAX_PROMPT_CHARS: usize = 4000;

impl AgentMemory {
    pub fn load(path: PathBuf) -> Self {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        Self { path }
    }

    pub fn content(&self) -> String {
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        if text.len() <= MAX_PROMPT_CHARS {
            return text;
        }
        let mut end = MAX_PROMPT_CHARS;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text[..end].to_string()
    }

    /// Append an entry under today's `## YYYY-MM-DD` section, creating the
    /// section (or file) when needed.
    pub fn remember(&self, text: &str) -> std::io::Result<()> {
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let heading = format!("## {today}");
        let existing = std::fs::read_to_string(&self.path).unwrap_or_default();
        let mut lines: Vec<String> = existing.lines().map(str::to_string).collect();
        let entry = format!("- {}", text.trim());
        match lines.iter().rposition(|l| l.trim() == heading) {
            Some(pos) => {
                let insert_at = lines[pos + 1..]
                    .iter()
                    .position(|l| l.starts_with("## "))
                    .map(|i| pos + 1 + i)
                    .unwrap_or(lines.len());
                lines.insert(insert_at, entry);
            }
            None => {
                if !lines.is_empty() {
                    lines.push(String::new());
                }
                lines.push(heading);
                lines.push(String::new());
                lines.push(entry);
            }
        }
        let mut out = lines.join("\n");
        out.push('\n');
        std::fs::write(&self.path, out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remember_groups_by_day_and_content_caps() {
        let dir = std::env::temp_dir().join(format!("kb-agent-mem-{}", uuid::Uuid::new_v4()));
        let mem = AgentMemory::load(dir.join("memory.md"));
        mem.remember("喜欢简洁回复").unwrap();
        mem.remember("用 Rust").unwrap();
        let content = mem.content();
        assert_eq!(content.matches("## ").count(), 1);
        assert!(content.contains("- 喜欢简洁回复"));
        assert!(content.contains("- 用 Rust"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn content_truncates_long_memory() {
        let dir = std::env::temp_dir().join(format!("kb-agent-mem-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("memory.md");
        std::fs::write(&file, "x".repeat(5000)).unwrap();
        let mem = AgentMemory::load(file);
        assert_eq!(mem.content().len(), MAX_PROMPT_CHARS);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
