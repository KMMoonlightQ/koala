use super::{App, Panel, input, text, theme, transcript};
use crate::agent::event::{TaskState, TodoState};
use crate::i18n::{self, Key};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Paragraph},
};

/// Title and shortcut hint for the modal dialog chrome drawn by `view`.
pub(super) fn dialog_chrome(app: &App) -> Option<(&'static str, &'static str)> {
    let lang = app.lang;
    let t = |key| i18n::text(lang, key);
    match app.panel {
        Some(Panel::Sessions { .. }) => {
            Some((t(Key::PanelSessions), t(Key::HintSelectResumeCancel)))
        }
        Some(Panel::Permissions { .. } | Panel::Model { .. } | Panel::Effort { .. }) => Some((
            t(dialog_list_title(&app.panel)),
            t(Key::HintSelectConfirmCancel),
        )),
        Some(Panel::Todos { .. }) => Some((t(Key::PanelTodos), t(Key::HintScrollBack))),
        Some(Panel::Help { .. }) => Some((t(Key::PanelHelp), t(Key::HintScrollBack))),
        Some(Panel::History { .. }) => Some((t(Key::PanelHistory), t(Key::HintHistorySelect))),
        Some(Panel::Tasks { output: true, .. }) => {
            Some((t(Key::PanelTaskOutput), t(Key::HintTaskOutput)))
        }
        Some(Panel::Tasks { .. }) => Some((t(Key::PanelTasks), t(Key::HintTasksSelect))),
        None => None,
    }
}

fn dialog_list_title(panel: &Option<Panel>) -> Key {
    match panel {
        Some(Panel::Permissions { .. }) => Key::PanelPermissions,
        Some(Panel::Model { .. }) => Key::PanelModel,
        _ => Key::PanelEffort,
    }
}

/// Approximate content height of the open panel, so the modal dialog hugs
/// its content instead of stretching to the full transcript height.
pub(super) fn content_height(app: &App) -> usize {
    match &app.panel {
        Some(Panel::Sessions { .. }) => (app.sessions.len() * 2).clamp(1, 16),
        Some(Panel::Permissions { .. }) => 3,
        Some(Panel::Model { .. }) => app.models.len(),
        Some(Panel::Effort { .. }) => app.reasoning_efforts.len(),
        Some(Panel::Todos { .. }) => todos(app).len().clamp(1, 16),
        Some(Panel::Help { .. }) => {
            // Sectioned help text plus the command list, in the active language.
            i18n::text(app.lang, Key::HelpText).lines().count() + input::COMMANDS.len()
        }
        Some(Panel::History { query, .. }) => 2 + app.history.search(query).len().clamp(1, 10),
        Some(Panel::Tasks { output: false, .. }) => app.tasks.len().clamp(1, 12),
        Some(Panel::Tasks { output: true, .. }) => 14,
        None => 0,
    }
}

pub(super) fn draw(f: &mut Frame, app: &mut App, area: Rect) {
    if let Some(Panel::Todos { scroll }) = app.panel {
        let mut lines = Vec::new();
        for item in ordered_todos(app) {
            let (mark, style) = todo_marker(item.status);
            lines.extend(transcript::literal(
                &format!("{mark} {}", text::clean(&item.content)),
                area.width as usize,
                style,
            ));
        }
        let mut scroll = scroll;
        render_scrolled(f, lines, &mut scroll, area);
        app.panel = Some(Panel::Todos { scroll });
        return;
    }
    match app.panel.as_mut() {
        Some(Panel::Sessions { selected, loading }) => {
            if *loading || app.sessions.is_empty() {
                f.render_widget(
                    Paragraph::new(if *loading {
                        i18n::text(app.lang, Key::LoadingSessions)
                    } else {
                        i18n::text(app.lang, Key::NoSessions)
                    })
                    .style(theme::muted()),
                    area,
                );
                return;
            }
            let capacity = (area.height as usize / 2).max(1);
            let start = selected.saturating_sub(capacity.saturating_sub(1));
            let mut lines = Vec::new();
            for (index, item) in app.sessions.iter().enumerate().skip(start).take(capacity) {
                lines.push(Line::styled(
                    format!(
                        "{}{}{}",
                        if index == *selected { "❯ " } else { "  " },
                        text::clean(&item.title),
                        if item.current {
                            i18n::text(app.lang, Key::Current)
                        } else {
                            ""
                        }
                    ),
                    if index == *selected {
                        theme::suggestion()
                    } else {
                        theme::text()
                    },
                ));
                lines.push(Line::styled(
                    format!("  {} · {}", item.updated, item.id),
                    theme::muted(),
                ));
            }
            f.render_widget(Paragraph::new(Text::from(lines)), area);
        }
        Some(Panel::Permissions { selected }) => {
            let capacity = area.height as usize;
            let start = selected.saturating_sub(capacity.saturating_sub(1));
            let lines: Vec<_> = super::PermissionMode::ALL
                .iter()
                .enumerate()
                .skip(start)
                .take(capacity)
                .map(|(index, mode)| {
                    Line::styled(
                        format!(
                            "{}{}{} · {}",
                            if index == *selected { "❯ " } else { "  " },
                            mode.label(),
                            if *mode == app.permission_mode {
                                i18n::text(app.lang, Key::Current)
                            } else {
                                ""
                            },
                            mode.description(app.lang)
                        ),
                        if index == *selected {
                            theme::suggestion()
                        } else {
                            theme::muted()
                        },
                    )
                })
                .collect();
            f.render_widget(Paragraph::new(Text::from(lines)), area);
        }
        Some(Panel::Model { selected }) => {
            let capacity = area.height as usize;
            let start = selected.saturating_sub(capacity.saturating_sub(1));
            let lines: Vec<_> = app
                .models
                .iter()
                .enumerate()
                .skip(start)
                .take(capacity)
                .map(|(index, effort)| {
                    let current = effort == &app.model;
                    Line::styled(
                        format!(
                            "{}{}{}",
                            if index == *selected { "❯ " } else { "  " },
                            text::clean(effort),
                            if current {
                                i18n::text(app.lang, Key::Current)
                            } else {
                                ""
                            }
                        ),
                        if index == *selected {
                            theme::suggestion()
                        } else {
                            theme::muted()
                        },
                    )
                })
                .collect();
            f.render_widget(Paragraph::new(Text::from(lines)), area);
        }
        Some(Panel::Effort { selected }) => {
            let capacity = area.height as usize;
            let start = selected.saturating_sub(capacity.saturating_sub(1));
            let lines: Vec<_> = app
                .reasoning_efforts
                .iter()
                .enumerate()
                .skip(start)
                .take(capacity)
                .map(|(index, effort)| {
                    let current = Some(effort) == app.reasoning_effort.as_ref();
                    Line::styled(
                        format!(
                            "{}{}{}",
                            if index == *selected { "❯ " } else { "  " },
                            text::clean(effort),
                            if current {
                                i18n::text(app.lang, Key::Current)
                            } else {
                                ""
                            }
                        ),
                        if index == *selected {
                            theme::suggestion()
                        } else {
                            theme::muted()
                        },
                    )
                })
                .collect();
            f.render_widget(Paragraph::new(Text::from(lines)), area);
        }
        Some(Panel::Help { scroll }) => {
            let mut lines = Vec::new();
            let help = i18n::text(app.lang, Key::HelpText);
            lines.extend(transcript::literal(
                help,
                area.width as usize,
                ratatui::style::Style::default(),
            ));
            for (name, description) in input::commands(app.lang) {
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
                    Span::styled(
                        format!(
                            "  · {}",
                            i18n::fill(
                                app.lang,
                                Key::HistoryCount,
                                &[("n", &matches.len().to_string())]
                            )
                        ),
                        theme::subtle(),
                    ),
                ]),
                Line::default(),
            ];
            if matches.is_empty() {
                lines.push(Line::styled(
                    i18n::text(app.lang, Key::NoHistoryMatches),
                    theme::muted(),
                ));
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
                lines.push(Line::styled(
                    i18n::text(app.lang, Key::NoTasks),
                    theme::muted(),
                ));
            } else if *output {
                if let Some(task) = app.tasks.iter().find(|t| Some(t.id) == *selected) {
                    lines.extend(transcript::literal(
                        &format!(
                            "#{} [{}] {} · {:.1}s\n{}",
                            task.id,
                            task.kind,
                            task.status.label(app.lang),
                            elapsed(task.elapsed_ms, task.status, app.tasks_received),
                            task.description
                        ),
                        area.width as usize,
                        theme::heading(),
                    ));
                    lines.push(Line::default());
                    let output = if task.output.is_empty() {
                        if task.status == TaskState::Running {
                            i18n::text(app.lang, Key::TaskOutputRunning)
                        } else if task.status == TaskState::Stopped {
                            i18n::text(app.lang, Key::TaskOutputStopped)
                        } else {
                            i18n::text(app.lang, Key::NoOutput)
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
                                task.status.label(app.lang),
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
        Some(Panel::Todos { .. }) | None => {}
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
            format!(" {} ", i18n::text(app.lang, Key::PanelCommands)),
            theme::subtle(),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines = Vec::new();
    for (row, index) in matches.iter().enumerate().skip(start).take(capacity) {
        let (name, description) = input::command(*index, app.lang);
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
        format!("▾ {done}/{}", items.len()),
        theme::subtle(),
    )];
    let ordered = ordered_todos(app);
    let capacity = area.height.saturating_sub(1) as usize;
    let shown = if ordered.len() > capacity {
        capacity.saturating_sub(1)
    } else {
        capacity
    };
    for item in ordered.iter().take(shown) {
        let (mark, style) = todo_marker(item.status);
        lines.push(Line::from(vec![Span::styled(
            format!("  {mark} {}", text::clean(&item.content).replace('\n', " ")),
            style,
        )]));
    }
    if ordered.len() > shown {
        lines.push(Line::styled(
            i18n::fill(
                app.lang,
                Key::MoreTodos,
                &[("n", &(ordered.len() - shown).to_string())],
            ),
            theme::key_hint(),
        ));
    }
    f.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn ordered_todos(app: &App) -> Vec<&crate::agent::event::TodoView> {
    let mut items: Vec<_> = todos(app).iter().collect();
    items.sort_by_key(|t| match t.status {
        TodoState::InProgress => 0,
        TodoState::Pending => 1,
        TodoState::Done => 2,
    });
    items
}

fn todo_marker(status: TodoState) -> (&'static str, ratatui::style::Style) {
    match status {
        TodoState::InProgress => ("◐", theme::heading()),
        TodoState::Pending => ("☐", theme::muted()),
        TodoState::Done => ("☒", theme::muted()),
    }
}
