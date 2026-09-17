use super::{App, Panel, panels, text, theme, transcript};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area().inner(Margin {
        horizontal: u16::from(f.area().width > 4),
        vertical: 0,
    });
    let input_height = (app.input.lines().len().clamp(1, 5) + 2) as u16;
    let permission_height = if app.permission.is_some() {
        (area.height / 2).clamp(5, 12)
    } else {
        0
    };
    let panel_open = app.panel.is_some();
    let todos = panels::todos(app);
    let todo_height = if todos.is_empty() || app.detailed || panel_open {
        0
    } else if !app.todos_expanded {
        1
    } else {
        (todos.len() + 1)
            .min(5)
            .min((area.height / 4).max(1) as usize) as u16
    };
    let menu_height = panels::menu_matches(app).len().min(5) as u16;
    let rows = Layout::vertical([
        Constraint::Length(u16::from(app.detailed || panel_open)),
        Constraint::Min(1),
        Constraint::Length(permission_height),
        Constraint::Length(todo_height),
        Constraint::Length(if menu_height > 0 { menu_height + 1 } else { 0 }),
        Constraint::Length(1),
        Constraint::Length(if app.detailed || panel_open {
            0
        } else {
            input_height
        }),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(area);
    let title = match app.panel {
        Some(Panel::Help { .. }) => "帮助 · ↑↓ / PgUp/PgDn 滚动 · Esc 返回",
        Some(Panel::History { .. }) => "搜索输入历史 · Enter/Tab 回填 · Esc 取消",
        Some(Panel::Tasks { output: true, .. }) => {
            "任务输出 · PgUp/PgDn 滚动 · x 停止 · Esc 返回列表"
        }
        Some(Panel::Tasks { .. }) => "后台任务 · ↑↓ 选择 · Enter 查看输出 · x 停止 · Esc 返回",
        None => "详细记录 · 完整工具参数与返回内容 · Esc / Ctrl+O 返回",
    };
    if app.detailed || panel_open {
        f.render_widget(Paragraph::new(title).style(theme::heading()), rows[0]);
    }
    if panel_open {
        panels::draw(f, app, rows[1]);
    } else {
        draw_transcript(f, app, rows[1]);
    }
    panels::draw_todos(f, app, rows[3]);
    panels::draw_menu(f, app, rows[4]);
    draw_permission(f, app, rows[2]);
    draw_status(f, app, rows[5]);
    if !app.detailed && !panel_open {
        let border = Block::default()
            .borders(Borders::TOP | Borders::BOTTOM)
            .border_style(theme::muted());
        let inner = border.inner(rows[6]);
        f.render_widget(border, rows[6]);
        let input = Layout::horizontal([Constraint::Length(2), Constraint::Min(1)]).split(inner);
        f.render_widget(Paragraph::new("❯").style(theme::accent()), input[0]);
        f.render_widget(&app.input, input[1]);
    }
    draw_metadata(f, app, rows[7]);
    let footer = if app.permission.is_some() {
        "方向键选择 · Enter 确认 · Esc 拒绝 · Ctrl+C 中断"
    } else if panel_open {
        "Esc 返回 · 面板打开期间保留输入草稿"
    } else if app.detailed {
        "↑↓ / PgUp/PgDn 滚动 · Home/End 首尾 · Esc 返回（保留草稿）"
    } else if let Some(hint) = &app.hint {
        hint.as_str()
    } else if app.unread {
        "有新内容 · Ctrl+End 回到底部 · Ctrl+O 详细记录"
    } else if !app.follow {
        "正在查看历史 · Ctrl+End 回到底部 · Ctrl+O 详细记录"
    } else if area.width < 70 {
        "/ 命令 · ? 帮助 · Shift+Tab 模式"
    } else {
        "Enter 发送 · Ctrl+J 换行 · / 命令 · ? 帮助 · Shift+Tab 模式 · Ctrl+R 历史"
    };
    f.render_widget(Paragraph::new(footer).style(theme::muted()), rows[8]);
}

fn draw_transcript(f: &mut Frame, app: &mut App, area: Rect) {
    if app
        .rendered
        .as_ref()
        .is_none_or(|cache| cache.width != area.width || cache.detailed != app.detailed)
    {
        let mut lines = Vec::new();
        if !app.detailed {
            lines.push(Line::from(vec![
                Span::styled("kb-agent", theme::heading()),
                Span::styled(format!("  v{}", env!("CARGO_PKG_VERSION")), theme::muted()),
            ]));
            for line in [
                format!("模型  {}", app.model),
                format!("目录  {}", app.directory),
            ] {
                lines.extend(transcript::literal(
                    &line,
                    area.width as usize,
                    theme::muted(),
                ));
            }
            lines.push(Line::default());
            lines.extend(transcript::literal(
                "直接输入开始对话 · /new 新会话 · /plan 规划 · /tasks 后台任务",
                area.width as usize,
                theme::muted(),
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
    let status = if app.busy {
        let elapsed = app.started.map(|t| t.elapsed()).unwrap_or_default();
        let spinner = ["⠋", "⠙", "⠹", "⠸"][((elapsed.as_millis() / 100) % 4) as usize];
        let action = if app.permission.is_some() {
            "Esc 拒绝 · Ctrl+C 中断"
        } else if app.detailed || app.panel.is_some() {
            "关闭面板后可中断"
        } else {
            "Esc 中断"
        };
        format!(
            "{spinner} {} · {}s · {action}",
            app.status,
            elapsed.as_secs()
        )
    } else if app.unread {
        "就绪 · 有新内容".into()
    } else {
        "就绪".into()
    };
    f.render_widget(
        Paragraph::new(text::clean(&status)).style(if app.busy {
            theme::warning()
        } else {
            theme::muted()
        }),
        area,
    );
}

fn draw_metadata(f: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled(
            if app.plan_mode { "Plan" } else { "Normal" },
            if app.plan_mode {
                theme::warning()
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
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_permission(f: &mut Frame, app: &mut App, area: Rect) {
    if let Some(prompt) = &mut app.permission {
        let block = Block::bordered()
            .title(" 权限确认 · 仅本次 ")
            .border_style(theme::warning());
        let inner = block.inner(area);
        f.render_widget(block, area);
        let parts = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);
        let details = Paragraph::new(text::clean(&prompt.text)).wrap(Wrap { trim: false });
        let bottom = details
            .line_count(parts[0].width)
            .saturating_sub(parts[0].height as usize)
            .min(u16::MAX as usize) as u16;
        prompt.scroll = prompt.scroll.min(bottom);
        f.render_widget(details.scroll((prompt.scroll, 0)), parts[0]);
        f.render_widget(
            Paragraph::new(if prompt.allow {
                "❯ 允许    拒绝"
            } else {
                "  允许  ❯ 拒绝"
            })
            .style(theme::warning()),
            parts[1],
        );
        f.render_widget(
            Paragraph::new("方向键选择 · Enter 确认 · Esc 拒绝 · PgUp/PgDn 查看参数")
                .style(theme::muted()),
            parts[2],
        );
    }
}
