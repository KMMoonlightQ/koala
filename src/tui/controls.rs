use super::{App, Panel, input};
use crate::agent::event::{SessionCommand, TaskState};
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
        app.hint = Some("当前轮正在执行，请先按 Esc 中断再切换模式".into());
    } else {
        app.session.send(SessionCommand::TogglePlanMode);
    }
}

/// Higher priority than composer shortcuts, lower priority than permissions.
pub(super) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if app.permission.is_some() {
        return false;
    }
    if app.panel.is_some() {
        return panel_key(app, key);
    }
    if app.detailed {
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
            app.todos_expanded = !app.todos_expanded;
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
                app.input = input::editor(&format!("/{}", input::COMMANDS[selected].0));
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
            app.history.previous(&mut app.input);
            app.menu_dismissed = true;
            true
        }
        KeyCode::Down if app.input.cursor().0 + 1 == app.input.lines().len() => {
            app.history.next(&mut app.input);
            app.menu_dismissed = true;
            true
        }
        _ => false,
    }
}

fn panel_key(app: &mut App, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
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
        Panel::Help { scroll } => match key.code {
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
                        app.input = input::editor(&app.history.entries[*index]);
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
