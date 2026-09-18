use super::{App, panels, text, theme, transcript};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

/// Pixel-style airliner, dsh-TUI splash fashion. Shown when the welcome
/// header fits (wide enough terminals only).
const LOGO: [&str; 7] = [
    " ▄▄",
    " █▀▙▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄",
    "█▀                              ▀▀█▄",
    "█   ▄  ▄  ▄  ▄  ▄  ▄  ▄          ▀█",
    "█▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄█",
    "       ▀▀▀▀▀██████▀▀▀",
    "             ▀▀▀▀",
];

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area().inner(Margin {
        horizontal: u16::from(f.area().width > 4),
        vertical: 0,
    });
    let input_height = (app.input.lines().len().clamp(1, 5) + 2) as u16;
    let todos = panels::todos(app);
    let todo_height = if todos.is_empty() || app.detailed {
        0
    } else if !app.todos_expanded {
        1
    } else {
        (todos.len() + 1)
            .min(5)
            .min((area.height / 4).max(1) as usize) as u16
    };
    let menu_height = panels::menu_height(app);
    let rows = Layout::vertical([
        Constraint::Length(u16::from(app.detailed)),
        Constraint::Min(1),
        Constraint::Length(todo_height),
        Constraint::Length(menu_height),
        Constraint::Length(1),
        Constraint::Length(if app.detailed { 0 } else { input_height }),
        Constraint::Length(1),
    ])
    .split(area);
    if app.detailed {
        f.render_widget(
            Paragraph::new("详细记录 · 完整工具参数与返回内容 · Esc / Ctrl+O 返回")
                .style(theme::heading()),
            rows[0],
        );
    }
    draw_transcript(f, app, rows[1]);
    panels::draw_todos(f, app, rows[2]);
    panels::draw_menu(f, app, rows[3]);
    draw_status(f, app, rows[4]);
    if !app.detailed {
        draw_input(f, app, rows[5]);
    }
    draw_statusbar(f, app, rows[6]);
    // Modal overlays float over the transcript, dsh-TUI hosted-dialog style;
    // the composer and status bar stay visible underneath.
    if let Some((title, hint)) = panels::dialog_chrome(app) {
        let region = rows[1];
        let max_h = (panels::content_height(app) as u16 + 3).max(4);
        let dialog = centered(region, 88, max_h);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme::suggestion())
            .title(Span::styled(format!(" {title} "), theme::suggestion()));
        f.render_widget(Clear, dialog);
        let inner = block.inner(dialog);
        f.render_widget(block, dialog);
        let parts = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
        let content = parts[0].inner(Margin {
            horizontal: 1,
            vertical: 0,
        });
        panels::draw(f, app, content);
        f.render_widget(
            Paragraph::new(format!(" {hint}")).style(theme::subtle()),
            parts[1],
        );
    }
    draw_permission(f, app, rows[1]);
}

/// Center a dialog of at most `max_w` x `max_h` inside `area`, clamped so
/// tiny terminals never produce out-of-bounds or inverted rects.
fn centered(area: Rect, max_w: u16, max_h: u16) -> Rect {
    let width = max_w.min(area.width).max(1);
    let height = max_h.min(area.height).max(1);
    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn draw_transcript(f: &mut Frame, app: &mut App, area: Rect) {
    if app
        .rendered
        .as_ref()
        .is_none_or(|cache| cache.width != area.width || cache.detailed != app.detailed)
    {
        let mut lines = Vec::new();
        if !app.detailed {
            if area.width >= 40 {
                for row in LOGO {
                    lines.push(Line::styled(row, theme::heading()));
                }
                lines.push(Line::default());
            }
            lines.push(Line::from(vec![
                Span::styled("Airplane", theme::heading()),
                Span::styled(format!("  v{}", env!("CARGO_PKG_VERSION")), theme::muted()),
            ]));
            for (label, value) in [("模型", &app.model), ("目录", &app.directory)] {
                let line = Line::from(vec![
                    Span::styled(format!("{label}  "), theme::subtle()),
                    Span::styled(text::clean(value), theme::muted()),
                ]);
                lines.extend(text::wrap(line, area.width as usize));
            }
            lines.push(Line::default());
            lines.extend(transcript::literal(
                "直接输入开始对话 · /new 新会话 · /plan 规划 · /tasks 后台任务",
                area.width as usize,
                theme::subtle(),
            ));
            lines.push(Line::default());
        }
        for entry in &app.entries {
            if !app.detailed && matches!(entry, transcript::EntryKind::Todos(_)) {
                continue;
            }
            lines.extend(transcript::render_entry(
                entry,
                area.width as usize,
                app.detailed,
            ));
            lines.push(Line::default());
        }
        app.rendered = Some(transcript::Rendered {
            width: area.width,
            detailed: app.detailed,
            lines,
        });
    }
    let lines = &app.rendered.as_ref().unwrap().lines;
    // Markdown and tool output are already wrapped with their continuation
    // prefixes. Paragraph only provides scrolling and terminal clipping.
    app.bottom = lines.len().saturating_sub(area.height as usize);
    app.scroll = if app.follow {
        app.bottom
    } else {
        app.scroll.min(app.bottom)
    };
    f.render_widget(
        Paragraph::new(Text::from(
            lines
                .iter()
                .skip(app.scroll)
                .take(area.height as usize)
                .cloned()
                .collect::<Vec<_>>(),
        )),
        area,
    );
}

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    if app.busy {
        let elapsed = app.started.map(|t| t.elapsed()).unwrap_or_default();
        let spinner = ["⠋", "⠙", "⠹", "⠸"][((elapsed.as_millis() / 100) % 4) as usize];
        let action = if app.permission.is_some() {
            "Esc 拒绝 · Ctrl+C 中断"
        } else if app.detailed || app.panel.is_some() {
            "关闭面板后可中断"
        } else {
            "Esc 中断"
        };
        let line = Line::from(vec![
            Span::styled(format!("{spinner} "), theme::accent()),
            Span::styled(
                text::clean(&format!("{} · {}s", app.status, elapsed.as_secs())),
                theme::text(),
            ),
            Span::styled(format!(" · {action}"), theme::subtle()),
        ]);
        f.render_widget(Paragraph::new(line), area);
    } else {
        let (text_str, style) = if app.unread {
            ("● 就绪 · 有新内容", theme::accent())
        } else {
            ("● 就绪", theme::subtle())
        };
        f.render_widget(Paragraph::new(text_str).style(style), area);
    }
}

fn draw_input(f: &mut Frame, app: &App, area: Rect) {
    let border_style = if app.busy {
        theme::accent()
    } else {
        theme::border()
    };
    let block = Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let input = Layout::horizontal([Constraint::Length(2), Constraint::Min(1)]).split(inner);
    f.render_widget(Paragraph::new("❯").style(theme::accent()), input[0]);
    f.render_widget(&app.input, input[1]);
}

/// Bottom bar: session metadata on the left, contextual key hints flushed
/// right — the dsh-TUI status-row arrangement.
fn draw_statusbar(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled(
            if app.plan_mode { "Plan" } else { "Normal" },
            if app.plan_mode {
                theme::plan()
            } else {
                theme::accent()
            },
        ),
        Span::styled(format!(" · 后台 {}", app.background_count), theme::muted()),
    ];
    // Keep mode/task count visible even for unusually long model identifiers.
    if area.width > 24 {
        spans.push(Span::styled(
            format!(" · {}", text::clean(&app.model)),
            theme::muted(),
        ));
    }
    let left = Line::from(spans);
    let left_width = left.width() as u16;
    f.render_widget(Paragraph::new(left), area);
    let (hint, fallback) = if app.permission.is_some() {
        ("Enter 确认 · Esc 拒绝 · Ctrl+C 中断", None)
    } else if app.panel.is_some() {
        ("Esc 返回 · 面板打开期间保留输入草稿", None)
    } else if app.detailed {
        (
            "↑↓ / PgUp/PgDn 滚动 · Home/End 首尾 · Esc 返回（保留草稿）",
            None,
        )
    } else if let Some(hint) = &app.hint {
        (hint.as_str(), None)
    } else if app.unread {
        ("有新内容 · Ctrl+End 回到底部 · Ctrl+O 详细记录", None)
    } else if !app.follow {
        ("正在查看历史 · Ctrl+End 回到底部 · Ctrl+O 详细记录", None)
    } else if area.width < 70 {
        ("/ 命令 · ? 帮助 · Shift+Tab 模式", None)
    } else {
        (
            "Enter 发送 · Ctrl+J 换行 · / 命令 · ? 帮助 · Shift+Tab 模式 · Ctrl+R 历史",
            Some("/ 命令 · ? 帮助 · Shift+Tab 模式"),
        )
    };
    // Long hints don't fit beside the metadata on narrower terminals: try the
    // short form, then drop the hint entirely.
    let mut chosen = Some(hint);
    if area.width <= left_width + Line::from(hint).width() as u16 + 2 {
        chosen = fallback.filter(|s| area.width > left_width + Line::from(*s).width() as u16 + 2);
    }
    if let Some(hint) = chosen {
        f.render_widget(
            Paragraph::new(hint)
                .style(theme::subtle())
                .alignment(Alignment::Right),
            area,
        );
    }
}

fn draw_permission(f: &mut Frame, app: &mut App, area: Rect) {
    if let Some(prompt) = &mut app.permission {
        let max_h = (area.height / 2).clamp(5, 12);
        let dialog = centered(area, 72, max_h);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .title(Span::styled(" 权限确认 · 仅本次 ", theme::warning()))
            .border_style(theme::warning());
        f.render_widget(Clear, dialog);
        let inner = block.inner(dialog);
        f.render_widget(block, dialog);
        let parts = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner.inner(Margin {
            horizontal: 1,
            vertical: 0,
        }));
        let details = Paragraph::new(text::clean(&prompt.text)).wrap(Wrap { trim: false });
        let bottom = details
            .line_count(parts[0].width)
            .saturating_sub(parts[0].height as usize)
            .min(u16::MAX as usize) as u16;
        prompt.scroll = prompt.scroll.min(bottom);
        f.render_widget(details.scroll((prompt.scroll, 0)), parts[0]);
        let options = Line::from(vec![
            Span::raw(" "),
            if prompt.allow {
                Span::styled(" 允许 ", theme::selected_badge())
            } else {
                Span::styled(" 允许 ", theme::muted())
            },
            Span::raw("  "),
            if prompt.allow {
                Span::styled(" 拒绝 ", theme::muted())
            } else {
                Span::styled(" 拒绝 ", theme::selected_badge())
            },
        ]);
        f.render_widget(Paragraph::new(options), parts[1]);
        f.render_widget(
            Paragraph::new("方向键选择 · Enter 确认 · Esc 拒绝 · PgUp/PgDn 查看参数")
                .style(theme::subtle()),
            parts[2],
        );
    }
}
