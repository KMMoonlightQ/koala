use super::{logo, markdown, text, theme};
use crate::agent::event::{TodoState, TodoView};
use crate::i18n::{self, Key, Lang};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub(super) enum EntryKind {
    User(String),
    Assistant(String),
    Tool(ToolEntry),
    Todos(Vec<TodoView>),
    Note(String),
    Error(String),
    Info(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ToolState {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

pub(super) struct ToolEntry {
    pub id: String,
    pub name: String,
    pub summary: String,
    pub arguments: String,
    pub output: Option<String>,
    pub state: ToolState,
    pub duration_ms: Option<u64>,
}

struct Rendered {
    width: u16,
    directory: String,
    /// Localised labels are baked into `lines`, so a language switch invalidates
    /// the cache just like a width or detail change does.
    pub lang: Lang,
    pub lines: Vec<Line<'static>>,
}

/// Owns display history and all viewport/cache invariants. Event handlers,
/// keyboard controls and drawing cross this interface instead of editing fields.
pub(super) struct Transcript {
    entries: Vec<EntryKind>,
    assistant_open: bool,
    last_todos: Option<usize>,
    rendered: Option<Rendered>,
    compact_viewport: Option<(usize, bool)>,
    follow: bool,
    unread: bool,
    scroll: usize,
    bottom: usize,
}

pub(super) enum Scroll {
    Up(usize),
    Down(usize),
    Start,
    End,
}

impl Default for Transcript {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            assistant_open: false,
            last_todos: None,
            rendered: None,
            compact_viewport: None,
            follow: true,
            unread: false,
            scroll: 0,
            bottom: 0,
        }
    }
}

impl Transcript {
    pub(super) fn detailed(&self) -> bool {
        self.compact_viewport.is_some()
    }
    pub(super) fn following(&self) -> bool {
        self.follow
    }
    pub(super) fn unread(&self) -> bool {
        self.unread
    }
    #[cfg(test)]
    pub(super) fn entries(&self) -> &[EntryKind] {
        &self.entries
    }
    #[cfg(test)]
    pub(super) fn scroll_offset(&self) -> usize {
        self.scroll
    }

    pub(super) fn todos(&self) -> &[TodoView] {
        let Some(index) = self.last_todos else {
            return &[];
        };
        let EntryKind::Todos(items) = &self.entries[index] else {
            unreachable!("last_todos must point to a Todo entry");
        };
        items
    }

    fn changed(&mut self) {
        self.rendered = None;
        self.unread |= !self.follow;
    }

    pub(super) fn push(&mut self, entry: EntryKind) {
        self.entries.push(entry);
        self.finish();
        self.changed();
    }

    pub(super) fn finish(&mut self) {
        self.assistant_open = false;
    }

    pub(super) fn toggle_details(&mut self) {
        if let Some((scroll, follow)) = self.compact_viewport.take() {
            self.scroll = scroll;
            self.follow = follow;
            if follow {
                self.unread = false;
            }
        } else {
            self.compact_viewport = Some((self.scroll, self.follow));
            self.follow = true;
        }
        self.rendered = None;
    }

    pub(super) fn scroll(&mut self, action: Scroll) {
        match action {
            Scroll::Up(lines) => {
                self.follow = false;
                self.scroll = self.scroll.saturating_sub(lines);
            }
            Scroll::Down(lines) => {
                self.scroll = self.scroll.saturating_add(lines).min(self.bottom);
                if self.scroll == self.bottom {
                    self.follow = true;
                    self.unread = false;
                }
            }
            Scroll::Start => {
                self.follow = false;
                self.scroll = 0;
            }
            Scroll::End => {
                self.follow = true;
                self.unread = false;
            }
        }
    }

    pub(super) fn append_text(&mut self, delta: String) {
        match self.entries.last_mut() {
            Some(EntryKind::Assistant(text)) if self.assistant_open => text.push_str(&delta),
            _ => {
                self.entries.push(EntryKind::Assistant(delta));
                self.assistant_open = true;
            }
        }
        self.changed();
    }

    pub(super) fn start_tool(
        &mut self,
        id: String,
        name: String,
        summary: String,
        arguments: String,
    ) {
        self.push(EntryKind::Tool(ToolEntry {
            id,
            name,
            summary,
            arguments,
            output: None,
            state: ToolState::Running,
            duration_ms: None,
        }));
    }

    pub(super) fn finish_tool(
        &mut self,
        id: &str,
        output: String,
        is_error: bool,
        duration_ms: u64,
    ) {
        if let Some(EntryKind::Tool(tool)) = self
            .entries
            .iter_mut()
            .rev()
            .find(|entry| matches!(entry, EntryKind::Tool(tool) if tool.id == id))
        {
            tool.output = Some(output);
            tool.state = if is_error {
                ToolState::Failed
            } else {
                ToolState::Succeeded
            };
            tool.duration_ms = Some(duration_ms);
        }
        self.changed();
    }

    pub(super) fn set_todos(&mut self, items: Vec<TodoView>) {
        match self.last_todos {
            Some(index) => {
                self.entries[index] = EntryKind::Todos(items);
                self.changed();
            }
            None => {
                self.last_todos = Some(self.entries.len());
                self.push(EntryKind::Todos(items));
            }
        }
    }

    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(super) fn restore(&mut self, records: Vec<crate::agent::transcripts::Record>) {
        self.reset();
        for record in records {
            self.push(if record.role == "user" {
                EntryKind::User(record.content)
            } else {
                EntryKind::Assistant(record.content)
            });
        }
    }

    pub(super) fn stop_tools(&mut self, state: ToolState, output: &str) {
        for entry in &mut self.entries {
            if let EntryKind::Tool(tool) = entry
                && tool.state == ToolState::Running
            {
                tool.state = state;
                tool.output = Some(output.into());
            }
        }
        self.changed();
    }

    /// Produces the visible lines while keeping wrapping, cache invalidation and
    /// scroll clamping together. Drawing never changes transcript state itself.
    pub(super) fn visible_lines(
        &mut self,
        width: u16,
        height: u16,
        lang: Lang,
        directory: &str,
    ) -> Vec<Line<'static>> {
        if self.rendered.as_ref().is_none_or(|cache| {
            cache.width != width || cache.lang != lang || cache.directory != directory
        }) {
            let mut lines = Vec::new();
            if !self.detailed() {
                if width >= 24 {
                    lines.extend(logo::lines());
                    lines.push(Line::default());
                }
                lines.push(Line::from(vec![
                    Span::styled("koala", theme::heading()),
                    Span::styled(format!("  v{}", env!("CARGO_PKG_VERSION")), theme::muted()),
                ]));
                lines.extend(text::wrap(
                    Line::styled(text::clean(directory), theme::muted()),
                    width as usize,
                ));
                lines.push(Line::default());
            }
            for entry in &self.entries {
                if !self.detailed() && matches!(entry, EntryKind::Todos(_)) {
                    continue;
                }
                lines.extend(render_entry(entry, width as usize, self.detailed(), lang));
                lines.push(Line::default());
            }
            self.rendered = Some(Rendered {
                width,
                directory: directory.into(),
                lang,
                lines,
            });
        }
        let lines = &self.rendered.as_ref().unwrap().lines;
        self.bottom = lines.len().saturating_sub(height as usize);
        self.scroll = if self.follow {
            self.bottom
        } else {
            self.scroll.min(self.bottom)
        };
        lines
            .iter()
            .skip(self.scroll)
            .take(height as usize)
            .cloned()
            .collect()
    }
}

pub(super) fn render_entry(
    entry: &EntryKind,
    width: usize,
    detailed: bool,
    lang: Lang,
) -> Vec<Line<'static>> {
    let body_width = width.saturating_sub(2).max(1);
    match entry {
        EntryKind::User(source) => {
            let style = theme::text().add_modifier(Modifier::BOLD);
            text::prefixed_styled(
                literal(source, body_width, style),
                Span::styled("› ", theme::user()),
                Span::raw("  "),
            )
        }
        EntryKind::Assistant(source) => text::prefixed_styled(
            markdown::render(source, body_width, lang),
            Span::styled("⏺ ", theme::accent()),
            Span::raw("  "),
        ),
        EntryKind::Tool(tool) => render_tool(tool, width, detailed, lang),
        EntryKind::Todos(items) => items
            .iter()
            .flat_map(|item| {
                let (mark, style) = match item.status {
                    TodoState::Done => ("☒", theme::muted()),
                    TodoState::InProgress => ("◐", theme::accent()),
                    TodoState::Pending => ("☐", theme::text()),
                };
                text::prefixed(
                    literal(&format!("{mark} {}", item.content), body_width, style),
                    "  ",
                    "  ",
                )
            })
            .collect(),
        EntryKind::Note(s) | EntryKind::Info(s) => text::prefixed_styled(
            literal(s, body_width, theme::muted()),
            Span::styled("· ", theme::subtle()),
            Span::raw("  "),
        ),
        EntryKind::Error(s) => text::prefixed_styled(
            literal(s, body_width, theme::error()),
            Span::styled("✗ ", theme::error()),
            Span::raw("  "),
        ),
    }
}

pub(super) fn literal(source: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    text::clean(source)
        .split('\n')
        .flat_map(|s| text::wrap(Line::styled(s.to_owned(), style), width))
        .collect()
}

fn render_tool(tool: &ToolEntry, width: usize, detailed: bool, lang: Lang) -> Vec<Line<'static>> {
    let (mark, key, style) = match tool.state {
        ToolState::Running => ("◐", Key::ToolRunning, theme::warning()),
        ToolState::Succeeded => ("✓", Key::ToolSucceeded, theme::success()),
        ToolState::Failed => ("✗", Key::ToolFailed, theme::error()),
        ToolState::Cancelled => ("■", Key::ToolCancelled, theme::muted()),
    };
    let label = i18n::text(lang, key);
    let elapsed = tool
        .duration_ms
        .map(|ms| format!(" · {:.1}s", ms as f64 / 1000.0))
        .unwrap_or_default();
    let mut header = Line::from(vec![
        Span::styled(format!("{mark} "), style),
        Span::styled(
            text::clean(&tool.name),
            theme::text().add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" · {label}{elapsed}"), style),
    ]);
    if !tool.summary.is_empty() {
        header.spans.push(Span::styled(
            format!("  {}", text::clean(&tool.summary).replace('\n', " ")),
            theme::suggestion(),
        ));
    }
    let mut lines = text::wrap(header, width);
    if !detailed && lines.len() > 2 {
        lines.truncate(1);
        lines.push(Line::styled(
            i18n::text(lang, Key::ViewFullArgs),
            theme::key_hint(),
        ));
    }
    if detailed {
        let arguments = serde_json::from_str::<serde_json::Value>(&tool.arguments)
            .and_then(|value| serde_json::to_string_pretty(&value))
            .unwrap_or_else(|_| tool.arguments.clone());
        lines.push(Line::styled(
            i18n::text(lang, Key::ArgsLabel),
            theme::muted(),
        ));
        lines.extend(text::prefixed(
            literal(&arguments, width.saturating_sub(4), theme::muted()),
            "    ",
            "    ",
        ));
        lines.push(Line::styled(
            i18n::text(lang, Key::OutputLabel),
            theme::muted(),
        ));
    }
    if let Some(output) = &tool.output {
        let output = if output.is_empty() {
            i18n::text(lang, Key::NoOutput)
        } else {
            output
        };
        let output_style = if tool.state == ToolState::Failed {
            theme::error()
        } else {
            theme::muted()
        };
        let mut content = literal(
            output.trim_end_matches('\n'),
            width.saturating_sub(4),
            output_style,
        );
        let hidden = if detailed {
            0
        } else {
            content.len().saturating_sub(3)
        };
        if !detailed {
            content.truncate(3);
        }
        lines.extend(text::prefixed(content, "  ⎿ ", "    "));
        if hidden > 0 {
            lines.push(Line::styled(
                i18n::fill(lang, Key::MoreLines, &[("n", &hidden.to_string())]),
                theme::key_hint(),
            ));
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render(transcript: &mut Transcript, lang: Lang) -> String {
        transcript
            .visible_lines(60, 40, lang, "workspace")
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn events_invalidate_cached_output_and_reset_clears_tool_and_todo_state() {
        let mut transcript = Transcript::default();
        transcript.append_text("first ".into());
        assert!(render(&mut transcript, Lang::En).contains("first"));
        transcript.append_text("answer".into());
        assert!(render(&mut transcript, Lang::En).contains("first answer"));
        transcript.start_tool("call".into(), "bash".into(), "ls".into(), "{}".into());
        render(&mut transcript, Lang::En);
        transcript.finish_tool("call", "visible result".into(), false, 10);
        assert!(render(&mut transcript, Lang::En).contains("visible result"));
        transcript.set_todos(vec![TodoView {
            content: "old todo".into(),
            status: TodoState::Pending,
        }]);
        assert_eq!(transcript.todos()[0].content, "old todo");
        transcript.toggle_details();
        transcript.scroll(Scroll::Start);
        transcript.reset();
        assert!(!transcript.detailed());
        assert!(transcript.following());
        assert!(!transcript.unread());
        assert!(transcript.todos().is_empty());
        let output = render(&mut transcript, Lang::En);
        assert!(!output.contains("visible result"));
        assert!(!output.contains("first answer"));
        transcript.set_todos(vec![TodoView {
            content: "new todo".into(),
            status: TodoState::InProgress,
        }]);
        assert_eq!(transcript.todos()[0].content, "new todo");
    }

    #[test]
    fn stopping_tools_only_changes_running_invocations() {
        let mut transcript = Transcript::default();
        for id in ["done", "active"] {
            transcript.start_tool(id.into(), "bash".into(), String::new(), "{}".into());
        }
        transcript.finish_tool("done", "completed output".into(), false, 1);
        render(&mut transcript, Lang::En);
        transcript.stop_tools(
            ToolState::Cancelled,
            i18n::text(Lang::En, Key::NoteToolInterrupted),
        );
        let output = render(&mut transcript, Lang::En);
        assert!(output.contains("completed output"));
        assert!(output.contains(i18n::text(Lang::En, Key::NoteToolInterrupted)));
        transcript.stop_tools(ToolState::Failed, "later failure");
        assert!(!render(&mut transcript, Lang::En).contains("later failure"));
    }
}
