use super::{markdown, text, theme};
use crate::agent::event::{TodoState, TodoView};
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

pub(super) struct Rendered {
    pub width: u16,
    pub detailed: bool,
    pub lines: Vec<Line<'static>>,
}

pub(super) fn render_entry(entry: &EntryKind, width: usize, detailed: bool) -> Vec<Line<'static>> {
    let body_width = width.saturating_sub(2).max(1);
    match entry {
        EntryKind::User(source) => {
            let style = Style::default().add_modifier(Modifier::BOLD);
            text::prefixed(literal(source, body_width, style), "❯ ", "  ")
        }
        EntryKind::Assistant(source) => {
            text::prefixed(markdown::render(source, body_width), "⏺ ", "  ")
        }
        EntryKind::Tool(tool) => render_tool(tool, width, detailed),
        EntryKind::Todos(items) => items
            .iter()
            .flat_map(|item| {
                let (mark, style) = match item.status {
                    TodoState::Done => ("☒", theme::muted()),
                    TodoState::InProgress => ("◐", theme::accent()),
                    TodoState::Pending => ("☐", Style::default()),
                };
                text::prefixed(
                    literal(&format!("{mark} {}", item.content), body_width, style),
                    "  ",
                    "  ",
                )
            })
            .collect(),
        EntryKind::Note(s) | EntryKind::Info(s) => {
            text::prefixed(literal(s, body_width, theme::muted()), "  ", "  ")
        }
        EntryKind::Error(s) => text::prefixed(literal(s, body_width, theme::error()), "✗ ", "  "),
    }
}

pub(super) fn literal(source: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    text::clean(source)
        .split('\n')
        .flat_map(|s| text::wrap(Line::styled(s.to_owned(), style), width))
        .collect()
}

fn render_tool(tool: &ToolEntry, width: usize, detailed: bool) -> Vec<Line<'static>> {
    let (mark, label, style) = match tool.state {
        ToolState::Running => ("◐", "进行中", theme::warning()),
        ToolState::Succeeded => ("✓", "成功", theme::success()),
        ToolState::Failed => ("✗", "失败", theme::error()),
        ToolState::Cancelled => ("■", "已中断", theme::muted()),
    };
    let elapsed = tool
        .duration_ms
        .map(|ms| format!(" · {:.1}s", ms as f64 / 1000.0))
        .unwrap_or_default();
    let mut header = Line::from(vec![
        Span::styled(
            format!("{mark} {}", text::clean(&tool.name)),
            style.add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" · {label}{elapsed}"), theme::muted()),
    ]);
    if !tool.summary.is_empty() {
        header.spans.push(Span::styled(
            format!("  {}", text::clean(&tool.summary).replace('\n', " ")),
            theme::accent(),
        ));
    }
    let mut lines = text::wrap(header, width);
    if !detailed && lines.len() > 2 {
        lines.truncate(1);
        lines.push(Line::styled("  … Ctrl+O 查看完整参数", theme::muted()));
    }
    if detailed {
        let arguments = serde_json::from_str::<serde_json::Value>(&tool.arguments)
            .and_then(|value| serde_json::to_string_pretty(&value))
            .unwrap_or_else(|_| tool.arguments.clone());
        lines.push(Line::styled("  参数", theme::muted()));
        lines.extend(text::prefixed(
            literal(&arguments, width.saturating_sub(4), theme::muted()),
            "    ",
            "    ",
        ));
        lines.push(Line::styled("  输出", theme::muted()));
    }
    if let Some(output) = &tool.output {
        let output = if output.is_empty() {
            "（无输出）"
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
                format!("    … 另 {hidden} 行 · Ctrl+O 展开"),
                theme::muted(),
            ));
        }
    }
    lines
}
