use super::{App, Panel, input, text, theme, transcript};
use crate::agent::event::{TaskState, TodoState};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Paragraph},
};

/// Title and shortcut hint for the modal dialog chrome drawn by `view`.
pub(super) fn dialog_chrome(app: &App) -> Option<(&'static str, &'static str)> {
    match app.panel {
        Some(Panel::Help { .. }) => Some(("帮助", "↑↓ / PgUp/PgDn 滚动 · Esc 返回")),
        Some(Panel::History { .. }) => {
            Some(("搜索输入历史", "↑↓ 选择 · Enter/Tab 回填 · Esc 取消"))
        }
        Some(Panel::Tasks { output: true, .. }) => {
            Some(("任务输出", "PgUp/PgDn 滚动 · x 停止 · Esc 返回列表"))
        }
        Some(Panel::Tasks { .. }) => {
            Some(("后台任务", "↑↓ 选择 · Enter 查看输出 · x 停止 · Esc 返回"))
        }
        None => None,
    }
}

/// Approximate content height of the open panel, so the modal dialog hugs
/// its content instead of stretching to the full transcript height.
pub(super) fn content_height(app: &App) -> usize {
    match &app.panel {
        Some(Panel::Help { .. }) => {
            // Sectioned help text plus the command list.
            22 + input::COMMANDS.len()
        }
        Some(Panel::History { query, .. }) => 2 + app.history.search(query).len().clamp(1, 10),
        Some(Panel::Tasks { output: false, .. }) => app.tasks.len().clamp(1, 12),
        Some(Panel::Tasks { output: true, .. }) => 14,
        None => 0,
    }
}

pub(super) fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    match app.panel.as_mut() {
        Some(Panel::Help { scroll }) => {
            let mut lines = Vec::new();
            let help = "输入与导航\nEnter 发送 · Shift+Enter / Ctrl+J / \\+Enter 换行\n↑↓ 在多行中移动，到首尾后召回历史\nCtrl+R 搜索历史，Enter/Tab 回填，Esc 保留原草稿\n粘贴多行作为一段草稿，不会自动发送\n\n命令与面板\n/ 筛选命令 · ↑↓ 选择 · Tab 补全 · Enter 执行\nShift+Tab 切换 Normal / Plan（空闲时）\nCtrl+T 展开/收起 Todo · Ctrl+O 详细记录\n/tasks 查看后台任务：↑↓ 选择 · Enter 输出 · x 停止\n?（空输入）或 /help 打开帮助\n\n运行与退出\nEsc 关闭面板；无面板时中断前台工作\nCtrl+C 中断前台工作；空闲时清空输入\n权限确认：方向键选择 · Enter 确认 · Esc 拒绝\nCtrl+D（空输入）或 /quit 退出\nPgUp/PgDn 历史 · Ctrl+End 回到底部\n\n命令列表";
            lines.extend(transcript::literal(
                help,
                area.width as usize,
                ratatui::style::Style::default(),
            ));
            for (name, description) in input::COMMANDS {
                lines.extend(transcript::literal(
                    &format!("/{name}  {description}"),
                    area.width as usize,
                    theme::suggestion(),
                ));
            }
            render_scrolled(f, lines, scroll, area);
        }
        Some(Panel::History { query, selected }) => {
            let matches = app.history.search(query);
            *selected = (*selected).min(matches.len().saturating_sub(1));
            let mut lines = vec![
                Line::from(vec![
                    Span::styled("⌕ ", theme::suggestion()),
                    Span::styled(format!("{}▏", text::clean(query)), theme::text()),
                    Span::styled(format!("  · {} 条", matches.len()), theme::subtle()),
                ]),
                Line::default(),
            ];
            if matches.is_empty() {
                lines.push(Line::styled("没有匹配的输入历史", theme::muted()));
            }
            let capacity = area.height.saturating_sub(2) as usize;
            let start = selected.saturating_sub(capacity.saturating_sub(1));
            for (row, index) in matches.iter().enumerate().skip(start).take(capacity) {
                let value = text::clean(&app.history.entries[*index]).replace('\n', " ↵ ");
                let focused = row == *selected;
                lines.push(Line::from(vec![
                    Span::styled(
                        if focused { "❯ " } else { "  " },
                        if focused {
                            theme::suggestion()
                        } else {
                            theme::subtle()
                        },
                    ),
                    Span::styled(
                        value,
                        if focused {
                            theme::suggestion()
                        } else {
                            theme::muted()
                        },
                    ),
                ]));
            }
            f.render_widget(Paragraph::new(Text::from(lines)), area);
        }
        Some(Panel::Tasks {
            selected,
            output,
            scroll,
        }) => {
            let mut lines = Vec::new();
            if app.tasks.is_empty() {
                lines.push(Line::styled("暂无后台任务", theme::muted()));
            } else if *output {
                if let Some(task) = app.tasks.iter().find(|t| Some(t.id) == *selected) {
                    lines.extend(transcript::literal(
                        &format!(
                            "#{} [{}] {} · {:.1}s\n{}",
                            task.id,
                            task.kind,
                            task.status.label(),
                            elapsed(task.elapsed_ms, task.status, app.tasks_received),
                            task.description
                        ),
                        area.width as usize,
                        theme::heading(),
                    ));
                    lines.push(Line::default());
                    let output = if task.output.is_empty() {
                        if task.status == TaskState::Running {
                            "任务执行中，完成后可查看完整输出。"
                        } else if task.status == TaskState::Stopped {
                            "任务已停止，没有已收集的输出。"
                        } else {
                            "（无输出）"
                        }
                    } else {
                        &task.output
                    };
                    lines.extend(transcript::literal(
                        output,
                        area.width as usize,
                        ratatui::style::Style::default(),
                    ));
                }
                render_scrolled(f, lines, scroll, area);
                return;
            } else {
                let index = app
                    .tasks
                    .iter()
                    .position(|t| Some(t.id) == *selected)
                    .unwrap_or(0);
                let capacity = area.height as usize;
                let start = index.saturating_sub(capacity.saturating_sub(1));
                for (i, task) in app.tasks.iter().enumerate().skip(start).take(capacity) {
                    let style = if task.status == TaskState::Failed {
                        theme::error()
                    } else if i == index {
                        theme::suggestion()
                    } else {
                        theme::muted()
                    };
                    lines.push(Line::from(vec![
                        Span::styled(
                            if i == index { "❯ " } else { "  " },
                            if i == index {
                                theme::suggestion()
                            } else {
                                theme::subtle()
                            },
                        ),
                        Span::styled(
                            format!(
                                "#{} [{}] {} · {:.1}s  {}",
                                task.id,
                                task.kind,
                                task.status.label(),
                                elapsed(task.elapsed_ms, task.status, app.tasks_received),
                                text::clean(&task.description).replace('\n', " ")
                            ),
                            style,
                        ),
                    ]));
                }
            }
            f.render_widget(Paragraph::new(Text::from(lines)), area);
        }
        None => {}
    }
}

fn elapsed(ms: u64, state: TaskState, received: std::time::Instant) -> f64 {
    ms as f64 / 1000.0
        + if matches!(state, TaskState::Running | TaskState::Stopping) {
            received.elapsed().as_secs_f64()
        } else {
            0.0
        }
}

fn render_scrolled(f: &mut Frame, lines: Vec<Line<'static>>, scroll: &mut usize, area: Rect) {
    *scroll = (*scroll).min(lines.len().saturating_sub(area.height as usize));
    f.render_widget(
        Paragraph::new(Text::from(
            lines
                .into_iter()
                .skip(*scroll)
                .take(area.height as usize)
                .collect::<Vec<_>>(),
        )),
        area,
    );
}

pub(super) fn menu_matches(app: &App) -> Vec<usize> {
    if app.menu_dismissed || app.panel.is_some() || app.detailed || app.permission.is_some() {
        Vec::new()
    } else {
        input::matches(&app.input)
    }
}

/// Rows the command menu occupies: at most five entries inside a rounded box.
pub(super) fn menu_height(app: &App) -> u16 {
    let matches = menu_matches(app).len().min(5) as u16;
    if matches == 0 { 0 } else { matches + 2 }
}

pub(super) fn draw_menu(f: &mut Frame, app: &App, area: Rect) {
    let matches = menu_matches(app);
    if area.height == 0 || matches.is_empty() {
        return;
    }
    let selected = app.menu_selected.min(matches.len() - 1);
    let capacity = area.height.saturating_sub(2) as usize;
    let start = selected.saturating_sub(capacity.saturating_sub(1));
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(theme::border())
        .title(Span::styled(
            " 命令 · ↑↓ 选择 · Tab 补全 · Enter 执行 ",
            theme::subtle(),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = Vec::new();
    for (row, index) in matches.iter().enumerate().skip(start).take(capacity) {
        let (name, description) = input::COMMANDS[*index];
        let focused = row == selected;
        lines.push(Line::from(vec![
            Span::styled(
                if focused { "❯ " } else { "  " },
                if focused {
                    theme::suggestion()
                } else {
                    theme::subtle()
                },
            ),
            Span::styled(
                format!("/{name}"),
                if focused {
                    theme::suggestion()
                } else {
                    theme::text()
                },
            ),
            Span::styled(format!("  {description}"), theme::subtle()),
        ]));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), inner);
}

pub(super) fn todos(app: &App) -> &[crate::agent::event::TodoView] {
    app.last_todos
        .and_then(|i| app.entries.get(i))
        .and_then(|entry| match entry {
            transcript::EntryKind::Todos(items) => Some(items.as_slice()),
            _ => None,
        })
        .unwrap_or_default()
}

pub(super) fn draw_todos(f: &mut Frame, app: &App, area: Rect) {
    let items = todos(app);
    if area.height == 0 || items.is_empty() {
        return;
    }
    let done = items.iter().filter(|t| t.status == TodoState::Done).count();
    let mut lines = vec![Line::styled(
        format!(
            "{} Todo {done}/{} · Ctrl+T {}",
            if app.todos_expanded { "▾" } else { "▸" },
            items.len(),
            if app.todos_expanded {
                "收起"
            } else {
                "展开"
            }
        ),
        theme::subtle(),
    )];
    if app.todos_expanded {
        let mut ordered: Vec<_> = items.iter().collect();
        ordered.sort_by_key(|t| match t.status {
            TodoState::InProgress => 0,
            TodoState::Pending => 1,
            TodoState::Done => 2,
        });
        let capacity = area.height.saturating_sub(1) as usize;
        let shown = if ordered.len() > capacity {
            capacity.saturating_sub(1)
        } else {
            capacity
        };
        for item in ordered.iter().take(shown) {
            let (mark, style) = match item.status {
                TodoState::InProgress => ("◐", theme::heading()),
                TodoState::Pending => ("☐", theme::muted()),
                TodoState::Done => ("☒", theme::muted()),
            };
            lines.push(Line::from(vec![Span::styled(
                format!("  {mark} {}", text::clean(&item.content).replace('\n', " ")),
                style,
            )]));
        }
        if ordered.len() > shown {
            lines.push(Line::styled(
                format!("  … 另 {} 项 · Ctrl+O 查看完整列表", ordered.len() - shown),
                theme::muted(),
            ));
        }
    }
    f.render_widget(Paragraph::new(Text::from(lines)), area);
}
