use super::{App, logo, panels, text, theme, transcript};
use crate::i18n::{self, Key};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Margin, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

pub(super) fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area().inner(Margin {
        horizontal: u16::from(f.area().width > 4),
        vertical: 0,
    });
    let input_height = (app.input.lines().len().clamp(1, 5) + 2) as u16;
    let todos = panels::todos(app);
    let todo_height = if todos.is_empty() || app.detailed {
        0
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
            Paragraph::new(i18n::text(app.lang, Key::DetailedLog)).style(theme::heading()),
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
        let max_h = (panels::content_height(app) as u16 + 2).max(3);
        let dialog = centered(region, 88, max_h);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(theme::suggestion())
            .title(Span::styled(format!(" {title} "), theme::suggestion()))
            .title_bottom(
                Line::styled(format!(" {hint} "), theme::key_hint()).alignment(Alignment::Right),
            );
        clear_modal_band(f, region, dialog);
        let inner = block.inner(dialog);
        f.render_widget(block, dialog);
        let content = inner.inner(Margin {
            horizontal: 1,
            vertical: 0,
        });
        panels::draw(f, app, content);
    }
    draw_permission(f, app, rows[1]);
}

/// Clear complete transcript rows so fragments beside a modal cannot read as
/// part of its content or border hints. Keep a blank row above and below it.
fn clear_modal_band(f: &mut Frame, region: Rect, dialog: Rect) {
    let top = dialog.y.saturating_sub(1).max(region.y);
    let bottom = dialog.bottom().saturating_add(1).min(region.bottom());
    let band = Rect::new(region.x, top, region.width, bottom.saturating_sub(top));
    f.render_widget(Clear, band.intersection(region));
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
    if app.rendered.as_ref().is_none_or(|cache| {
        cache.width != area.width || cache.detailed != app.detailed || cache.lang != app.lang
    }) {
        let mut lines = Vec::new();
        if !app.detailed {
            if area.width >= 24 {
                lines.extend(logo::lines());
                lines.push(Line::default());
            }
            lines.push(Line::from(vec![
                Span::styled("koala", theme::heading()),
                Span::styled(format!("  v{}", env!("CARGO_PKG_VERSION")), theme::muted()),
            ]));
            lines.extend(text::wrap(
                Line::styled(text::clean(&app.directory), theme::muted()),
                area.width as usize,
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
                app.lang,
            ));
            lines.push(Line::default());
        }
        app.rendered = Some(transcript::Rendered {
            width: area.width,
            detailed: app.detailed,
            lang: app.lang,
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
        let action = if app.permission.is_some() {
            i18n::text(app.lang, Key::ActionDenyOrInterrupt)
        } else if app.detailed || app.panel.is_some() {
            i18n::text(app.lang, Key::ActionClosePanelToInterrupt)
        } else {
            i18n::text(app.lang, Key::ActionInterrupt)
        };
        let mut line = logo::running(elapsed.as_millis(), app.permission.is_some());
        line.spans.extend([
            Span::styled(
                text::clean(&format!("{} · {}s", app.status, elapsed.as_secs())),
                theme::text(),
            ),
            Span::styled(format!(" · {action}"), theme::key_hint()),
        ]);
        f.render_widget(Paragraph::new(line), area);
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
    let permission_style = match app.permission_mode {
        crate::config::PermissionMode::Normal => theme::text(),
        crate::config::PermissionMode::AskWhenNeed => theme::warning(),
        crate::config::PermissionMode::NeverAsk => theme::error(),
    };
    let mut spans = vec![
        Span::styled(app.permission_mode.label(), permission_style),
        Span::styled(" · ", theme::text()),
    ];
    if app.plan_mode {
        spans.push(Span::styled("Plan · ", theme::plan()));
    }
    if app.background_count > 0 {
        spans.push(Span::styled(
            i18n::fill(
                app.lang,
                Key::StatusBarBackground,
                &[("n", &app.background_count.to_string())],
            ),
            theme::text(),
        ));
    }
    spans.push(Span::styled(text::clean(&app.model), theme::text()));
    if let Some(effort) = &app.reasoning_effort {
        spans.push(Span::styled(
            format!(" · {}", text::clean(effort)),
            theme::text(),
        ));
    }
    if let Some(capacity) = app.context_window {
        let usage = match app.context_used.filter(|_| capacity > 0) {
            Some(tokens) => format!("{:.0}%", tokens as f64 / capacity as f64 * 100.0),
            None => "--%".into(),
        };
        spans.push(Span::styled(format!(" · CTX [{usage}]"), theme::text()));
    }
    let left = Line::from(spans);
    let left_width = left.width() as u16;
    f.render_widget(Paragraph::new(left), area);
    let hint = if app.permission.is_some() || app.panel.is_some() {
        ""
    } else if app.detailed {
        i18n::text(app.lang, Key::StatusBarHintBack)
    } else if let Some(hint) = &app.hint {
        hint.as_str()
    } else if app.unread {
        i18n::text(app.lang, Key::StatusBarHintNewContent)
    } else if !app.follow {
        i18n::text(app.lang, Key::StatusBarHintBottom)
    } else {
        i18n::text(app.lang, Key::StatusBarHintHelp)
    };
    let hint_width = Line::from(hint).width() as u16;
    if hint_width > 0 && area.width > left_width + hint_width + 2 {
        // Paragraph applies its base style to its entire area, even blank cells.
        // Restrict dim styling to the hint so it cannot overwrite status colors.
        let hint_area = Rect::new(area.right() - hint_width, area.y, hint_width, area.height);
        f.render_widget(
            Paragraph::new(hint)
                .style(if app.hint.is_some() || app.unread {
                    theme::text()
                } else {
                    theme::key_hint()
                })
                .alignment(Alignment::Right),
            hint_area,
        );
    }
}

fn draw_permission(f: &mut Frame, app: &mut App, area: Rect) {
    if let Some(prompt) = &mut app.permission {
        let max_h = (area.height / 2).clamp(5, 12);
        let dialog = centered(area, 72, max_h);
        let block = Block::bordered()
            .border_type(BorderType::Rounded)
            .title(Span::styled(
                i18n::text(app.lang, Key::PermissionTitle),
                theme::warning(),
            ))
            .border_style(theme::warning());
        clear_modal_band(f, area, dialog);
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
                Span::styled(
                    i18n::text(app.lang, Key::AllowBadge),
                    theme::selected_badge(),
                )
            } else {
                Span::styled(i18n::text(app.lang, Key::AllowBadge), theme::muted())
            },
            Span::raw("  "),
            if prompt.allow {
                Span::styled(i18n::text(app.lang, Key::DenyBadge), theme::muted())
            } else {
                Span::styled(
                    i18n::text(app.lang, Key::DenyBadge),
                    theme::selected_badge(),
                )
            },
        ]);
        f.render_widget(Paragraph::new(options), parts[1]);
        f.render_widget(
            Paragraph::new(i18n::text(app.lang, Key::PermissionFooter)).style(theme::key_hint()),
            parts[2],
        );
    }
}
