//! Project-relative file references, completed one directory at a time.
use super::{App, text, theme};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    text::Line,
    widgets::{Block, BorderType, Paragraph},
};

struct Query {
    start: usize,
    prefix: String,
}

fn query(app: &App) -> Option<Query> {
    let (row, column) = app.input.cursor();
    let before: String = app.input.lines()[row].chars().take(column).collect();
    let (byte, _) = before.char_indices().rev().find(|(_, ch)| *ch == '@')?;
    if byte > 0 && !before[..byte].ends_with(char::is_whitespace) {
        return None;
    }
    let raw = &before[byte + 1..];
    let prefix = if let Some(quoted) = raw.strip_prefix('"') {
        // Completed quoted references no longer trigger a menu.
        if quoted.contains('"') {
            return None;
        }
        quoted
    } else {
        if raw.chars().any(char::is_whitespace) {
            return None;
        }
        raw
    };
    Some(Query {
        start: before[..byte].chars().count(),
        prefix: prefix.to_owned(),
    })
}

pub(super) fn candidates(app: &App) -> Vec<String> {
    if app.menu_dismissed
        || app.panel.is_some()
        || app.permission.is_some()
        || app.transcript.detailed()
    {
        return Vec::new();
    }
    let Some(query) = query(app) else {
        return Vec::new();
    };
    let (parent, name) = query
        .prefix
        .rsplit_once('/')
        .map_or(("", query.prefix.as_str()), |(p, n)| {
            (&query.prefix[..p.len() + 1], n)
        });
    let root = std::path::Path::new(&app.directory).join(parent);
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut paths = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let Some(file_name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !file_name.to_lowercase().starts_with(&name.to_lowercase())
            || file_name.chars().any(char::is_control)
            || file_name.contains(['"', '\\'])
        {
            continue;
        }
        if name.is_empty() && file_name.starts_with('.') {
            continue;
        }
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        // Do not follow symlinks when navigating project references.
        if !kind.is_file() && !kind.is_dir() {
            continue;
        }
        paths.push(format!(
            "{parent}{file_name}{}",
            if kind.is_dir() { "/" } else { "" }
        ));
    }
    paths.sort_by(|a, b| b.ends_with('/').cmp(&a.ends_with('/')).then(a.cmp(b)));
    paths
}

pub(super) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    let paths = candidates(app);
    if paths.is_empty() {
        return false;
    }
    match key.code {
        KeyCode::Up => app.menu_selected = app.menu_selected.saturating_sub(1),
        KeyCode::Down => app.menu_selected = (app.menu_selected + 1).min(paths.len() - 1),
        KeyCode::Esc => app.menu_dismissed = true,
        KeyCode::Tab | KeyCode::Enter
            if !key.modifiers.intersects(
                KeyModifiers::SHIFT
                    | KeyModifiers::CONTROL
                    | KeyModifiers::SUPER
                    | KeyModifiers::ALT,
            ) =>
        {
            let path = &paths[app.menu_selected.min(paths.len() - 1)];
            let query = query(app).unwrap();
            let (_, column) = app.input.cursor();
            for _ in query.start..column {
                app.input.delete_char();
            }
            let directory = path.ends_with('/');
            let quoted = path.chars().any(char::is_whitespace);
            let reference = match (quoted, directory) {
                (true, true) => format!("@\"{path}"),
                (true, false) => format!("@\"{path}\" "),
                (false, true) => format!("@{path}"),
                (false, false) => format!("@{path} "),
            };
            app.input.insert_str(reference);
            app.menu_selected = 0;
            app.history.reset_navigation();
        }
        _ => return false,
    }
    true
}

pub(super) fn draw(f: &mut Frame, app: &App, area: Rect, paths: &[String]) {
    let selected = app.menu_selected.min(paths.len().saturating_sub(1));
    let capacity = area.height.saturating_sub(2) as usize;
    let start = selected.saturating_sub(capacity.saturating_sub(1));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border())
        .title(" @ · Tab / Enter ");
    let inner = block.inner(area);
    f.render_widget(block, area);
    let lines: Vec<_> = paths
        .iter()
        .enumerate()
        .skip(start)
        .take(capacity)
        .map(|(index, path)| {
            Line::styled(
                format!(
                    "{}{}",
                    if index == selected { "❯ " } else { "  " },
                    text::clean(path)
                ),
                if index == selected {
                    theme::selected()
                } else {
                    theme::muted()
                },
            )
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::session::SessionHandle;

    #[test]
    fn navigate_quoted_directory_then_insert_file_and_dismiss_menu() {
        let root = std::env::temp_dir().join(format!("koala-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("my docs")).unwrap();
        std::fs::write(root.join("my docs/说明.md"), "content").unwrap();
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = App::new(session);
        app.directory = root.display().to_string();
        app.input.insert_str("review @my");
        assert!(handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
        ));
        assert_eq!(app.input.lines(), ["review @\"my docs/"]);
        assert_eq!(candidates(&app), ["my docs/说明.md"]);
        assert!(handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)
        ));
        assert_eq!(app.input.lines(), ["review @\"my docs/说明.md\" "]);
        assert!(candidates(&app).is_empty());
        assert!(commands.try_recv().is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn email_and_finished_mentions_do_not_open_menu() {
        let (session, _) = SessionHandle::test_channel();
        let mut app = App::new(session);
        for value in ["name@example.com", "@\"my docs/file.md\"", "@src/file.rs "] {
            app.input = super::super::input::editor(value, app.lang);
            assert!(query(&app).is_none(), "{value}");
        }
    }
}
