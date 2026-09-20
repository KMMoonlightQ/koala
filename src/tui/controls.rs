use super::{App, Panel, input};
use crate::agent::event::{SessionCommand, TaskState};
use crate::i18n::{self, Key};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

pub(super) fn close_panel(app: &mut App) {
    if matches!(app.panel, Some(Panel::Tasks { .. })) {
        app.session.send(SessionCommand::HideTasks);
    }
    app.panel = None;
}

pub(super) fn open_tasks(app: &mut App) {
    close_panel(app);
    app.panel = Some(Panel::Tasks {
        selected: app.tasks.first().map(|t| t.id),
        output: false,
        scroll: 0,
    });
    app.session.send(SessionCommand::ShowTasks);
}

pub(super) fn toggle_mode(app: &mut App) {
    if app.busy {
        app.hint = Some(i18n::text(app.lang, Key::NoteBusyInterruptFirst).into());
    } else {
        app.session.send(SessionCommand::TogglePlanMode);
    }
}

/// Higher priority than composer shortcuts, lower priority than permissions.
pub(super) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.temporary {
        if key.code == KeyCode::Char('j') && key.modifiers.contains(KeyModifiers::CONTROL) {
            app.input.insert_newline();
            return true;
        }
        return false;
    }
    if app.permission.is_some() {
        return false;
    }
    if app.panel.is_some() {
        return panel_key(app, key);
    }
    if app.transcript.detailed() {
        return false;
    }
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('r') if ctrl => {
            app.panel = Some(Panel::History {
                query: String::new(),
                selected: 0,
            });
            return true;
        }
        KeyCode::Char('t') if ctrl => {
            app.panel = Some(Panel::Todos { scroll: 0 });
            return true;
        }
        KeyCode::Char('j') if ctrl => {
            app.input.insert_newline();
            app.menu_dismissed = true;
            return true;
        }
        KeyCode::BackTab => {
            toggle_mode(app);
            return true;
        }
        KeyCode::Char('?') if app.input.is_empty() => {
            app.panel = Some(Panel::Help { scroll: 0 });
            return true;
        }
        _ => {}
    }
    let matches = input::matches(&app.input);
    if !app.menu_dismissed && !matches.is_empty() {
        match key.code {
            KeyCode::Up => {
                app.menu_selected = app.menu_selected.saturating_sub(1);
                return true;
            }
            KeyCode::Down => {
                app.menu_selected = (app.menu_selected + 1).min(matches.len() - 1);
                return true;
            }
            KeyCode::Tab | KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                let selected = matches[app.menu_selected.min(matches.len() - 1)];
                app.input = input::editor(&format!("/{}", input::COMMANDS[selected].0), app.lang);
                app.menu_dismissed = true;
                if key.code == KeyCode::Enter {
                    super::submit(app);
                }
                return true;
            }
            KeyCode::Esc => {
                app.menu_dismissed = true;
                return true;
            }
            _ => {}
        }
    }
    match key.code {
        KeyCode::Up if app.input.cursor().0 == 0 => {
            app.history.previous(&mut app.input, app.lang);
            app.menu_dismissed = true;
            true
        }
        KeyCode::Down if app.input.cursor().0 + 1 == app.input.lines().len() => {
            app.history.next(&mut app.input, app.lang);
            app.menu_dismissed = true;
            true
        }
        _ => false,
    }
}

fn panel_key(app: &mut App, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && key.code == KeyCode::Char('t') && matches!(app.panel, Some(Panel::Todos { .. })) {
        close_panel(app);
        return true;
    }
    if let Some(Panel::Graph(view)) = &mut app.panel {
        if matches!(key.code, KeyCode::Char('r') | KeyCode::Enter)
            && !ctrl
            && super::graph::browsing(view)
            && super::graph::retry_turn(view, &app.graph).is_some()
        {
            if app.busy {
                app.hint = Some(i18n::text(app.lang, Key::NoteBusyInterruptFirst).into());
            } else if let Some(id) = super::graph::retry_turn(view, &app.graph) {
                if app.input.is_empty() {
                    app.session.send(if key.code == KeyCode::Enter {
                        SessionCommand::ContinueAfterTurn(id)
                    } else {
                        SessionCommand::BranchBeforeTurn(id)
                    });
                } else {
                    app.hint = Some(
                        if app.lang == crate::i18n::Lang::Zh {
                            "请先处理输入框中的草稿"
                        } else {
                            "Please finish or clear the current draft first"
                        }
                        .into(),
                    );
                }
            }
            return true;
        }
        if super::graph::handle_key(view, &app.graph, key) {
            close_panel(app);
        }
        return true;
    }
    if key.code == KeyCode::Esc || (ctrl && key.code == KeyCode::Char('c')) {
        if let Some(Panel::Tasks { output, scroll, .. }) = &mut app.panel
            && *output
        {
            *output = false;
            *scroll = 0;
        } else {
            close_panel(app);
        }
        return true;
    }
    match app.panel.as_mut().unwrap() {
        Panel::Graph(_) => unreachable!(),
        Panel::Sessions { selected, loading } => match key.code {
            KeyCode::Up => *selected = selected.saturating_sub(1),
            KeyCode::Down => *selected = (*selected + 1).min(app.sessions.len().saturating_sub(1)),
            KeyCode::Enter if !*loading && !app.busy => {
                if let Some(item) = app.sessions.get(*selected) {
                    app.session
                        .send(SessionCommand::RestoreSession(item.id.clone()));
                    close_panel(app);
                    app.restarting = true;
                    app.start(i18n::text(app.lang, Key::StatusRestoringSession));
                }
            }
            _ => {}
        },
        Panel::Theme { selected } => match key.code {
            KeyCode::Up => *selected = selected.saturating_sub(1),
            KeyCode::Down => *selected = (*selected + 1).min(super::Theme::ALL.len() - 1),
            KeyCode::Enter => {
                let theme = super::Theme::ALL[*selected];
                close_panel(app);
                super::handle_command(app, &format!("theme {}", theme.code()));
            }
            _ => {}
        },
        Panel::Permissions { selected } => match key.code {
            KeyCode::Up => *selected = selected.saturating_sub(1),
            KeyCode::Down => *selected = (*selected + 1).min(super::PermissionMode::ALL.len() - 1),
            KeyCode::Enter => {
                app.session.send(SessionCommand::SetPermissionMode(
                    super::PermissionMode::ALL[*selected],
                ));
                close_panel(app);
            }
            _ => {}
        },
        Panel::Model { selected } => match key.code {
            KeyCode::Up => *selected = selected.saturating_sub(1),
            KeyCode::Down => *selected = (*selected + 1).min(app.models.len().saturating_sub(1)),
            KeyCode::Enter => {
                if let Some(name) = app.models.get(*selected) {
                    app.session.send(SessionCommand::SelectModel(name.clone()));
                }
                close_panel(app);
            }
            _ => {}
        },
        Panel::Effort { selected } => match key.code {
            KeyCode::Up => *selected = selected.saturating_sub(1),
            KeyCode::Down => {
                *selected = (*selected + 1).min(app.reasoning_efforts.len().saturating_sub(1))
            }
            KeyCode::Enter => {
                if let Some(value) = app.reasoning_efforts.get(*selected) {
                    app.session
                        .send(SessionCommand::SetReasoningEffort(value.clone()));
                }
                close_panel(app);
            }
            _ => {}
        },
        Panel::Help { scroll } | Panel::Todos { scroll } => match key.code {
            KeyCode::Down | KeyCode::PageDown => {
                *scroll = scroll.saturating_add(if key.code == KeyCode::Down { 1 } else { 8 })
            }
            KeyCode::Up | KeyCode::PageUp => {
                *scroll = scroll.saturating_sub(if key.code == KeyCode::Up { 1 } else { 8 })
            }
            KeyCode::Char('q' | '?') => close_panel(app),
            _ => {}
        },
        Panel::History { query, selected } => {
            let matches = app.history.search(query);
            match key.code {
                KeyCode::Up => *selected = selected.saturating_sub(1),
                KeyCode::Down => *selected = (*selected + 1).min(matches.len().saturating_sub(1)),
                KeyCode::Char('r') if ctrl => {
                    if !matches.is_empty() {
                        *selected = (*selected + 1) % matches.len();
                    }
                }
                KeyCode::Enter | KeyCode::Tab => {
                    if let Some(index) = matches.get(*selected) {
                        app.input = input::editor(&app.history.entries[*index], app.lang);
                        app.history.reset_navigation();
                        app.menu_dismissed = true;
                        close_panel(app);
                    }
                }
                KeyCode::Char('u') if ctrl => {
                    query.clear();
                    *selected = 0;
                }
                KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                    query.push(c);
                    *selected = 0;
                }
                KeyCode::Backspace => {
                    query.pop();
                    *selected = 0;
                }
                _ => {}
            }
        }
        Panel::Tasks {
            selected,
            output,
            scroll,
        } => {
            let index = app
                .tasks
                .iter()
                .position(|t| Some(t.id) == *selected)
                .unwrap_or(0);
            match key.code {
                KeyCode::Char('q') => close_panel(app),
                KeyCode::Enter => {
                    *output = !*output;
                    *scroll = 0;
                }
                KeyCode::Char('x') => {
                    if let Some(task) = app.tasks.get(index)
                        && task.status == TaskState::Running
                    {
                        app.session.send(SessionCommand::StopTask(task.id));
                    }
                }
                KeyCode::Up | KeyCode::PageUp if *output => {
                    *scroll = scroll.saturating_sub(if key.code == KeyCode::Up { 1 } else { 8 })
                }
                KeyCode::Down | KeyCode::PageDown if *output => {
                    *scroll = scroll.saturating_add(if key.code == KeyCode::Down { 1 } else { 8 })
                }
                KeyCode::Home if *output => *scroll = 0,
                KeyCode::End if *output => *scroll = usize::MAX,
                KeyCode::Up => {
                    *selected = app.tasks.get(index.saturating_sub(1)).map(|t| t.id);
                }
                KeyCode::Down => {
                    *selected = app
                        .tasks
                        .get((index + 1).min(app.tasks.len().saturating_sub(1)))
                        .map(|t| t.id);
                }
                _ => {}
            }
        }
    }
    true
}
