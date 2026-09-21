use crate::i18n::{self, Key, Lang};
use serde::{Deserialize, Serialize};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use tui_textarea::TextArea;

/// Slash commands in completion order, each with the key of its description.
/// Names are never translated: they are the typed identifiers themselves.
pub(super) const COMMANDS: &[(&str, Key)] = &[
    ("help", Key::CmdHelp),
    ("sessions", Key::CmdSessions),
    ("new", Key::CmdNew),
    ("plan", Key::CmdPlan),
    ("model", Key::CmdModel),
    ("permissions", Key::CmdPermissions),
    ("effort", Key::CmdEffort),
    ("tasks", Key::CmdTasks),
    ("todos", Key::CmdTodos),
    ("skills", Key::CmdSkills),
    ("compact", Key::CmdCompact),
    ("lang", Key::CmdLang),
    ("theme", Key::CmdTheme),
    ("quit", Key::CmdQuit),
    ("memory", Key::CmdMemory),
    ("tree", Key::CmdGraph),
    ("btw", Key::CmdBtw),
    ("extensions", Key::CmdExtensions),
];

/// Name and translated description of the command at `index` in [`COMMANDS`].
pub(super) fn command(index: usize, lang: Lang) -> (&'static str, &'static str) {
    let (name, key) = COMMANDS[index];
    (name, i18n::text(lang, key))
}

/// The whole command list, described in `lang`; used by the help panel.
pub(super) fn commands(lang: Lang) -> Vec<(&'static str, &'static str)> {
    (0..COMMANDS.len()).map(|i| command(i, lang)).collect()
}

/// Slash-command names matching the current prefix; descriptions live in the
/// command table and are resolved per language at render time.
pub(super) fn matches(input: &TextArea<'_>) -> Vec<usize> {
    if input.lines().len() != 1 {
        return Vec::new();
    }
    let Some(prefix) = input.lines()[0].strip_prefix('/') else {
        return Vec::new();
    };
    if prefix.chars().any(char::is_whitespace) {
        return Vec::new();
    }
    COMMANDS
        .iter()
        .enumerate()
        .filter_map(|(i, (name, _))| name.starts_with(prefix).then_some(i))
        .collect()
}

#[derive(Default)]
pub(super) struct History {
    pub entries: Vec<String>,
    position: Option<usize>,
    draft: Option<TextArea<'static>>,
    file: Option<PathBuf>,
    project: String,
}

#[derive(Serialize, Deserialize)]
struct Record {
    project: String,
    text: String,
}

impl History {
    pub fn load(file: PathBuf, project: String) -> io::Result<Self> {
        let mut history = Self {
            file: Some(file.clone()),
            project,
            ..Self::default()
        };
        match std::fs::File::open(file) {
            Ok(file) => {
                for line in io::BufReader::new(file).lines() {
                    if let Ok(record) = serde_json::from_str::<Record>(&line?)
                        && record.project == history.project
                    {
                        history.remember(record.text);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        Ok(history)
    }

    fn remember(&mut self, text: String) {
        if self.entries.last() != Some(&text) {
            self.entries.push(text);
            if self.entries.len() > 1000 {
                self.entries.remove(0);
            }
        }
    }

    pub fn record(&mut self, text: &str) -> io::Result<()> {
        self.reset_navigation();
        if self.entries.last().is_some_and(|last| last == text) {
            return Ok(());
        }
        self.remember(text.to_owned());
        if let Some(path) = &self.file {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let mut options = std::fs::OpenOptions::new();
            options.create(true).append(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(path)?;
            let mut record = serde_json::to_vec(&Record {
                project: self.project.clone(),
                text: text.into(),
            })?;
            record.push(b'\n');
            file.write_all(&record)?;
        }
        Ok(())
    }

    pub fn reset_navigation(&mut self) {
        self.position = None;
        self.draft = None;
    }

    pub fn previous(&mut self, input: &mut TextArea<'static>, lang: Lang) {
        if self.entries.is_empty() {
            return;
        }
        let index = match self.position {
            Some(index) => index.saturating_sub(1),
            None => {
                self.draft = Some(input.clone());
                self.entries.len() - 1
            }
        };
        self.position = Some(index);
        *input = editor(&self.entries[index], lang);
        input.move_cursor(tui_textarea::CursorMove::Top);
    }

    pub fn next(&mut self, input: &mut TextArea<'static>, lang: Lang) {
        let Some(index) = self.position else { return };
        if index + 1 < self.entries.len() {
            self.position = Some(index + 1);
            *input = editor(&self.entries[index + 1], lang);
        } else {
            if let Some(draft) = self.draft.take() {
                *input = draft;
            }
            self.position = None;
        }
    }

    pub fn search(&self, query: &str) -> Vec<usize> {
        let query = query.to_lowercase();
        self.entries
            .iter()
            .enumerate()
            .rev()
            .filter_map(|(i, text)| text.to_lowercase().contains(&query).then_some(i))
            .collect()
    }
}

pub(super) fn editor(text: &str, lang: Lang) -> TextArea<'static> {
    let mut input = super::new_input(lang);
    input.insert_str(text);
    input
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_roundtrip_is_project_scoped_and_preserves_multiline() {
        let root = std::env::temp_dir().join(format!("koala-history-{}", uuid::Uuid::new_v4()));
        let path = root.join("history");
        let mut first = History::load(path.clone(), "project-a".into()).unwrap();
        first.record("第一行\n第二行").unwrap();
        first.record("第一行\n第二行").unwrap();
        let mut other = History::load(path.clone(), "project-b".into()).unwrap();
        assert!(other.entries.is_empty());
        other.record("other project").unwrap();
        let restored = History::load(path, "project-a".into()).unwrap();
        assert_eq!(restored.entries, ["第一行\n第二行"]);
        assert_eq!(restored.search("第二行"), [0]);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn history_navigation_restores_draft_and_cursor() {
        let mut history = History::default();
        history.record("older").unwrap();
        history.record("newer").unwrap();
        let mut draft = editor("未发送\n草稿", Lang::Zh);
        draft.move_cursor(tui_textarea::CursorMove::Back);
        let cursor = draft.cursor();
        history.previous(&mut draft, Lang::Zh);
        assert_eq!(draft.lines(), ["newer"]);
        history.previous(&mut draft, Lang::Zh);
        assert_eq!(draft.lines(), ["older"]);
        history.next(&mut draft, Lang::Zh);
        history.next(&mut draft, Lang::Zh);
        assert_eq!(draft.lines(), ["未发送", "草稿"]);
        assert_eq!(draft.cursor(), cursor);
    }
}
