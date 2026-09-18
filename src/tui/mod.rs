use crate::config::{PermissionMode, Theme};
mod controls;
mod input;
mod logo;
mod markdown;
mod panels;
mod text;
mod theme;
mod transcript;
mod view;

use crate::agent::Agent;
use crate::agent::event::{SessionCommand, TaskView, UiEvent};
use crate::agent::session::{self, SessionHandle};
use crate::config::Config;
use crate::i18n::{self, Key, Lang};
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use ratatui::style::Style;
#[cfg(test)]
use ratatui::text::Line;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
#[cfg(test)]
use transcript::ToolEntry;
use transcript::{EntryKind, Scroll, ToolState, Transcript};
use tui_textarea::TextArea;
use view::draw;

enum Panel {
    Theme {
        selected: usize,
    },
    Sessions {
        selected: usize,
        loading: bool,
    },
    Permissions {
        selected: usize,
    },
    Model {
        selected: usize,
    },
    Effort {
        selected: usize,
    },
    Todos {
        scroll: usize,
    },
    Help {
        scroll: usize,
    },
    History {
        query: String,
        selected: usize,
    },
    Tasks {
        selected: Option<usize>,
        output: bool,
        scroll: usize,
    },
}

struct PermissionPrompt {
    text: String,
    respond: oneshot::Sender<bool>,
    allow: bool,
    scroll: u16,
}

/// UI state only; all agent operations go through SessionCommand / UiEvent.
struct App {
    session: SessionHandle,
    history: input::History,
    menu_selected: usize,
    menu_dismissed: bool,
    panel: Option<Panel>,
    tasks: Vec<TaskView>,
    sessions: Vec<crate::agent::transcripts::SessionView>,
    tasks_received: Instant,
    model: String,
    models: Vec<String>,
    reasoning_efforts: Vec<String>,
    reasoning_effort: Option<String>,
    context_window: Option<u64>,
    context_used: Option<u64>,
    directory: String,
    plan_mode: bool,
    permission_mode: PermissionMode,
    /// Interface language; drives every label and the system prompt.
    lang: Lang,
    language_path: Option<std::path::PathBuf>,
    theme: Theme,
    theme_path: Option<std::path::PathBuf>,
    background_count: usize,
    transcript: Transcript,
    input: TextArea<'static>,
    permission: Option<PermissionPrompt>,
    busy: bool,
    restarting: bool,
    status: String,
    started: Option<Instant>,
    hint: Option<String>,
    quit: bool,
}

fn new_input(lang: Lang) -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_placeholder_text(i18n::text(lang, Key::InputPlaceholder));
    input.set_placeholder_style(theme::subtle());
    input.set_cursor_line_style(Style::default());
    input
}

impl App {
    fn new(session: SessionHandle) -> Self {
        Self {
            session,
            history: input::History::default(),
            menu_selected: 0,
            menu_dismissed: false,
            panel: None,
            tasks: Vec::new(),
            sessions: Vec::new(),
            tasks_received: Instant::now(),
            model: String::new(),
            models: Vec::new(),
            reasoning_efforts: Vec::new(),
            reasoning_effort: None,
            context_window: None,
            context_used: None,
            directory: String::new(),
            plan_mode: false,
            permission_mode: PermissionMode::Normal,
            lang: Lang::default(),
            language_path: None,
            theme: Theme::default(),
            theme_path: None,
            background_count: 0,
            transcript: Transcript::default(),
            input: new_input(Lang::default()),
            permission: None,
            busy: false,
            restarting: false,
            status: String::new(),
            started: None,
            hint: None,
            quit: false,
        }
    }

    fn configure_appearance(&mut self, cfg: &Config) {
        self.theme = cfg.theme;
        self.theme_path = cfg.theme_path.clone();
        self.set_lang(cfg.lang);
        self.language_path = cfg.language_path.clone();
    }

    fn push(&mut self, entry: EntryKind) {
        self.transcript.push(entry);
    }

    fn toggle_details(&mut self) {
        self.transcript.toggle_details();
        self.hint = None;
    }

    fn start(&mut self, status: &str) {
        self.busy = true;
        self.started = Some(Instant::now());
        self.status = status.into();
        self.hint = None;
    }

    fn finish(&mut self) {
        self.permission = None;
        self.transcript.finish();
        if !self.restarting {
            self.busy = false;
            self.started = None;
            self.status.clear();
        }
    }

    /// Switch the interface language. The transcript cache keys on language;
    /// draft, cursor and scroll stay untouched.
    fn set_lang(&mut self, lang: Lang) {
        if self.lang == lang {
            return;
        }
        self.lang = lang;
        self.input
            .set_placeholder_text(i18n::text(lang, Key::InputPlaceholder));
        self.hint = None;
    }

    fn cancel(&mut self) {
        if self.busy && !self.restarting {
            self.session.send(SessionCommand::Cancel);
            self.status = i18n::text(self.lang, Key::StatusInterrupting).into();
            self.permission = None;
        }
    }
}

pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let (handle, events) = session::spawn(Agent::new(cfg).await?);
    let mut app = App::new(handle);
    app.configure_appearance(cfg);
    app.model = cfg.llm.model.clone();
    app.directory = std::env::current_dir()?.display().to_string();
    match input::History::load(
        cfg.agent.session_dir.join(".input-history"),
        app.directory.clone(),
    ) {
        Ok(history) => app.history = history,
        Err(e) => app.push(EntryKind::Note(i18n::fill(
            app.lang,
            Key::NoteHistoryLoadFailed,
            &[("e", &e.to_string())],
        ))),
    }
    let mut terminal = ratatui::init();
    let result =
        match crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste) {
            Ok(()) => event_loop(&mut terminal, &mut app, events).await,
            Err(e) => Err(e.into()),
        };
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
    app.session.send(SessionCommand::Shutdown);
    ratatui::restore();
    result
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    mut events: mpsc::UnboundedReceiver<UiEvent>,
) -> anyhow::Result<()> {
    let mut keys = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        terminal.draw(|f| draw(f, app))?;
        tokio::select! {
            key = keys.next() => match key {
                Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => handle_key(app, key),
                Some(Ok(Event::Paste(text))) => handle_paste(app, &text),
                Some(Err(e)) => return Err(e.into()),
                None => app.quit = true,
                _ => {}
            },
            ev = events.recv() => match ev {
                Some(ev) => handle_ui_event(app, ev),
                None => return Ok(()),
            },
            _ = tick.tick(), if app.busy || matches!(app.panel, Some(Panel::Tasks { .. })) => {}
        }
        if app.quit {
            app.session.send(SessionCommand::Shutdown);
            // Wait for foreground cancellation and process cleanup before exit.
            while events.recv().await.is_some() {}
            return Ok(());
        }
    }
}

fn handle_ui_event(app: &mut App, ev: UiEvent) {
    match ev {
        UiEvent::ContextUsage(tokens) => app.context_used = tokens,
        UiEvent::Tasks(tasks) => {
            app.tasks = tasks;
            app.tasks_received = Instant::now();
            if let Some(Panel::Tasks { selected, .. }) = &mut app.panel
                && !app.tasks.iter().any(|t| Some(t.id) == *selected)
            {
                *selected = app.tasks.first().map(|t| t.id);
            }
        }
        UiEvent::PermissionMode(mode) => app.permission_mode = mode,
        UiEvent::PlanMode(on) => app.plan_mode = on,
        UiEvent::ModelSettings {
            model,
            models,
            reasoning_efforts,
            reasoning_effort,
            context_window,
        } => {
            if app.model != model {
                app.context_used = None;
            }
            app.model = model;
            app.models = models;
            app.reasoning_efforts = reasoning_efforts;
            app.reasoning_effort = reasoning_effort;
            app.context_window = context_window;
        }
        UiEvent::BackgroundCount(n) => app.background_count = n,
        UiEvent::Status(status) => app.status = status,
        UiEvent::Text(delta) => app.transcript.append_text(delta),
        UiEvent::Todos(items) => app.transcript.set_todos(items),
        UiEvent::ToolStart {
            id,
            name,
            summary,
            arguments,
        } => {
            app.status = i18n::fill(app.lang, Key::StatusRunningTool, &[("name", &name)]);
            app.transcript.start_tool(id, name, summary, arguments);
        }
        UiEvent::ToolEnd {
            id,
            output,
            is_error,
            duration_ms,
        } => {
            app.transcript
                .finish_tool(&id, output, is_error, duration_ms);
            app.status = i18n::text(app.lang, Key::StatusProcessing).into();
        }
        UiEvent::Note(text) => app.push(EntryKind::Note(text)),
        UiEvent::Info(text) => app.push(EntryKind::Info(text)),
        UiEvent::PermissionRequest { text, respond } => {
            app.status = i18n::text(app.lang, Key::StatusAwaitingApproval).into();
            app.permission = Some(PermissionPrompt {
                text,
                respond,
                allow: false,
                scroll: 0,
            });
        }
        UiEvent::Done => app.finish(),
        UiEvent::Cancelled => {
            app.transcript.stop_tools(
                ToolState::Cancelled,
                i18n::text(app.lang, Key::NoteToolInterrupted),
            );
            app.finish();
            app.push(EntryKind::Note(
                i18n::text(app.lang, Key::InfoCancelled).into(),
            ));
        }
        UiEvent::Sessions(items) => {
            app.sessions = items;
            if let Some(Panel::Sessions { selected, loading }) = &mut app.panel {
                *selected = 0;
                *loading = false;
            }
        }
        UiEvent::SessionRestored { id, records } => {
            app.transcript.restore(records);
            app.context_used = None;
            controls::close_panel(app);
            app.restarting = false;
            app.finish();
            app.hint = None;
            app.push(EntryKind::Info(i18n::fill(
                app.lang,
                Key::InfoSessionRestored,
                &[("id", &id)],
            )));
        }
        UiEvent::WorkRestored(trace) => {
            use crate::agent::work::Trace;
            app.transcript.reset();
            for item in trace {
                match item {
                    Trace::User(text) => app.push(EntryKind::User(text)),
                    Trace::Text(text) => app.transcript.append_text(text),
                    Trace::ToolStart {
                        id,
                        name,
                        summary,
                        arguments,
                    } => app.transcript.start_tool(id, name, summary, arguments),
                    Trace::ToolEnd {
                        id,
                        output,
                        is_error,
                        duration_ms,
                    } => app
                        .transcript
                        .finish_tool(&id, output, is_error, duration_ms),
                    Trace::Todos(items) => app.transcript.set_todos(
                        items
                            .iter()
                            .map(crate::agent::event::TodoView::from)
                            .collect(),
                    ),
                    Trace::Note(text) => app.push(EntryKind::Note(text)),
                }
            }
            app.transcript.stop_tools(
                ToolState::Cancelled,
                i18n::text(app.lang, Key::NoteToolInterrupted),
            );
            app.finish();
        }
        UiEvent::SessionRestoreFailed(error) => {
            app.restarting = false;
            app.finish();
            controls::close_panel(app);
            app.push(EntryKind::Error(error));
        }
        UiEvent::SessionReset => {
            app.transcript.reset();
            app.context_used = None;
            controls::close_panel(app);
            app.restarting = false;
            app.finish();
            app.hint = None;
            app.push(EntryKind::Info(
                i18n::text(app.lang, Key::InfoNewSession).into(),
            ));
        }
        UiEvent::Error(err) => {
            app.transcript.stop_tools(ToolState::Failed, &err);
            app.permission = None;
            app.push(EntryKind::Error(err));
        }
    }
}

fn handle_paste(app: &mut App, pasted: &str) {
    if app.permission.is_some() || app.transcript.detailed() {
        return;
    }
    let pasted = text::clean(&pasted.replace("\r\n", "\n").replace('\r', "\n"));
    match &mut app.panel {
        Some(Panel::History { query, selected }) => {
            query.push_str(&pasted.replace('\n', " "));
            *selected = 0;
        }
        None => {
            app.input.insert_str(pasted);
            app.history.reset_navigation();
            app.menu_dismissed = true;
            app.hint = None;
        }
        _ => {}
    }
}

fn handle_key(app: &mut App, key: KeyEvent) {
    if controls::handle_key(app, key) {
        return;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        match key.code {
            KeyCode::Char('o') if app.permission.is_none() => {
                app.toggle_details();
                return;
            }
            KeyCode::Char('c') => {
                if app.busy {
                    app.cancel();
                } else {
                    app.input = new_input(app.lang);
                    app.history.reset_navigation();
                }
                return;
            }
            KeyCode::Char('d') if app.input.is_empty() && app.permission.is_none() => {
                app.quit = true;
                return;
            }
            _ => {}
        }
    }
    if let Some(prompt) = app.permission.as_mut() {
        let decision = match key.code {
            KeyCode::Left | KeyCode::Right | KeyCode::Up | KeyCode::Down | KeyCode::Tab => {
                prompt.allow = !prompt.allow;
                None
            }
            KeyCode::PageUp => {
                prompt.scroll = prompt.scroll.saturating_sub(5);
                None
            }
            KeyCode::PageDown => {
                prompt.scroll = prompt.scroll.saturating_add(5);
                None
            }
            KeyCode::Enter => Some(prompt.allow),
            KeyCode::Char('y' | 'Y') => Some(true),
            KeyCode::Char('n' | 'N') | KeyCode::Esc => Some(false),
            _ => None,
        };
        if let Some(allow) = decision {
            let prompt = app.permission.take().unwrap();
            let _ = prompt.respond.send(allow);
            app.status = i18n::text(app.lang, Key::StatusContinuing).into();
            app.push(EntryKind::Note(
                i18n::text(
                    app.lang,
                    if allow {
                        Key::InfoAllowedOnce
                    } else {
                        Key::InfoDeniedOnce
                    },
                )
                .into(),
            ));
        }
        return;
    }
    if app.transcript.detailed() {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => app.toggle_details(),
            KeyCode::PageUp | KeyCode::Up => app
                .transcript
                .scroll(Scroll::Up(if key.code == KeyCode::Up { 1 } else { 10 })),
            KeyCode::PageDown | KeyCode::Down => app
                .transcript
                .scroll(Scroll::Down(if key.code == KeyCode::Down { 1 } else { 10 })),
            KeyCode::Home => app.transcript.scroll(Scroll::Start),
            KeyCode::End => app.transcript.scroll(Scroll::End),
            _ => {}
        }
        return;
    }
    match key.code {
        KeyCode::Esc => app.cancel(),
        KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
            app.input.insert_newline();
        }
        KeyCode::Enter
            if app.input.lines()[app.input.cursor().0].ends_with('\\')
                && app.input.cursor().1
                    == app.input.lines()[app.input.cursor().0].chars().count() =>
        {
            app.input.delete_char();
            app.input.insert_newline();
        }
        KeyCode::Enter => submit(app),
        KeyCode::PageUp => app.transcript.scroll(Scroll::Up(10)),
        KeyCode::PageDown => app.transcript.scroll(Scroll::Down(10)),
        KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.transcript.scroll(Scroll::End)
        }
        _ => {
            app.hint = None;
            let before = app.input.lines().to_vec();
            app.input.input(key);
            if app.input.lines() != before {
                app.history.reset_navigation();
                app.menu_selected = 0;
                app.menu_dismissed = false;
            }
        }
    }
}

fn consume_input(app: &mut App, text: &str) {
    if let Err(e) = app.history.record(text) {
        app.push(EntryKind::Note(i18n::fill(
            app.lang,
            Key::NoteHistorySaveFailed,
            &[("e", &e.to_string())],
        )));
    }
    app.input = new_input(app.lang);
    app.menu_selected = 0;
    app.menu_dismissed = false;
}

fn submit(app: &mut App) {
    let text = app.input.lines().join("\n").trim().to_string();
    if text.is_empty() {
        return;
    }
    if matches!(text.as_str(), "/quit" | "/q") {
        consume_input(app, &text);
        app.quit = true;
        return;
    }
    if text == "/new" && !app.restarting {
        consume_input(app, &text);
        app.restarting = true;
        app.start(i18n::text(app.lang, Key::StatusNewSession));
        app.session.send(SessionCommand::NewSession);
        return;
    }
    // Frontend settings and panels also work during an active turn.
    if matches!(text.as_str(), "/help" | "/tasks" | "/todos")
        || text == "/lang"
        || text.starts_with("/lang ")
        || text == "/theme"
        || text.starts_with("/theme ")
    {
        // A rejected command (bad /lang argument) keeps the draft for editing.
        if handle_command(app, text.trim_start_matches('/')) {
            consume_input(app, &text);
        }
        return;
    }
    if app.busy {
        app.hint = Some(i18n::text(app.lang, Key::NoteBusyDraftKept).into());
        return;
    }
    if let Some(cmd) = text.strip_prefix('/') {
        if handle_command(app, cmd) {
            consume_input(app, &text);
        }
        return;
    }
    consume_input(app, &text);
    app.transcript.scroll(Scroll::End);
    app.push(EntryKind::User(text.clone()));
    app.start(i18n::text(app.lang, Key::StatusGenerating));
    app.session.send(SessionCommand::Submit(text));
}

fn handle_command(app: &mut App, cmd: &str) -> bool {
    let mut words = cmd.split_whitespace();
    let command = words.next();
    if command == Some("memory") {
        let args: Vec<_> = words.collect();
        let control = match args.as_slice() {
            [] | ["show"] => None,
            ["read", "on"] => Some(crate::agent::event::MemoryControl::Read(true)),
            ["read", "off"] => Some(crate::agent::event::MemoryControl::Read(false)),
            ["write", "on"] => Some(crate::agent::event::MemoryControl::Write(true)),
            ["write", "off"] => Some(crate::agent::event::MemoryControl::Write(false)),
            _ => {
                app.hint = Some(i18n::text(app.lang, Key::UsageMemory).into());
                return false;
            }
        };
        app.session.send(SessionCommand::Memory { control });
        return true;
    }
    if command == Some("theme") {
        let value = words.next();
        if words.next().is_some() || value.is_some_and(|v| Theme::parse(v).is_none()) {
            app.hint = Some(i18n::text(app.lang, Key::UsageTheme).into());
            return false;
        }
        if value.is_none() {
            controls::close_panel(app);
            app.panel = Some(Panel::Theme {
                selected: Theme::ALL
                    .iter()
                    .position(|theme| *theme == app.theme)
                    .unwrap_or(0),
            });
            return true;
        }
        if let Some(value) = value.and_then(Theme::parse) {
            app.theme = value;
            if let Some(path) = &app.theme_path
                && let Err(e) = crate::config::save_theme(path, value)
            {
                app.push(EntryKind::Note(i18n::fill(
                    app.lang,
                    Key::NoteThemeSaveFailed,
                    &[("e", &e.to_string())],
                )));
            }
        }
        app.push(EntryKind::Info(i18n::fill(
            app.lang,
            Key::InfoThemeSet,
            &[("theme", app.theme.code())],
        )));
        return true;
    }
    if command == Some("permissions") {
        let value = words.next();
        if words.next().is_some() || value.is_some_and(|v| PermissionMode::parse(v).is_none()) {
            app.hint = Some(i18n::text(app.lang, Key::UsagePermissions).into());
            return false;
        }
        if let Some(mode) = value.and_then(PermissionMode::parse) {
            app.session.send(SessionCommand::SetPermissionMode(mode));
        } else {
            let selected = PermissionMode::ALL
                .iter()
                .position(|m| *m == app.permission_mode)
                .unwrap_or(0);
            app.panel = Some(Panel::Permissions { selected });
        }
        return true;
    }
    if command == Some("model") {
        let name = words.next();
        if words.next().is_some() {
            app.hint = Some(i18n::text(app.lang, Key::UsageModel).into());
            return false;
        }
        if let Some(name) = name {
            app.session.send(SessionCommand::SelectModel(name.into()));
        } else if app.models.is_empty() {
            app.push(EntryKind::Note(
                i18n::text(app.lang, Key::NoteNoModels).into(),
            ));
        } else {
            let selected = app.models.iter().position(|m| m == &app.model).unwrap_or(0);
            app.panel = Some(Panel::Model { selected });
        }
        return true;
    }
    if command == Some("effort") {
        let value = words.next().map(str::to_owned);
        if words.next().is_some() {
            app.hint = Some(i18n::text(app.lang, Key::UsageEffort).into());
            return false;
        }
        if let Some(value) = value {
            app.session.send(SessionCommand::SetReasoningEffort(value));
        } else if app.reasoning_efforts.is_empty() {
            app.push(EntryKind::Note(
                i18n::text(app.lang, Key::NoteNoReasoningEfforts).into(),
            ));
        } else {
            let selected = app
                .reasoning_efforts
                .iter()
                .position(|v| Some(v) == app.reasoning_effort.as_ref())
                .unwrap_or(0);
            app.panel = Some(Panel::Effort { selected });
        }
        return true;
    }
    if command == Some("lang") {
        let value = words.next();
        if words.next().is_some() {
            app.hint = Some(i18n::text(app.lang, Key::UsageLang).into());
            return false;
        }
        let lang = match value {
            None => app.lang.toggled(),
            Some(value) => match Lang::parse(value) {
                Some(lang) => lang,
                None => {
                    app.hint = Some(i18n::text(app.lang, Key::UsageLang).into());
                    return false;
                }
            },
        };
        app.set_lang(lang);
        // The agent renders its system prompt in the same language.
        app.session.send(SessionCommand::SetLang(lang));
        if let Some(path) = &app.language_path
            && let Err(e) = crate::config::save_language(path, lang)
        {
            app.push(EntryKind::Note(i18n::fill(
                lang,
                Key::NoteLanguageSaveFailed,
                &[("e", &e.to_string())],
            )));
        }
        app.push(EntryKind::Info(i18n::fill(
            lang,
            Key::InfoLanguageSet,
            &[("label", lang.label())],
        )));
        return true;
    }
    match cmd {
        "plan" => controls::toggle_mode(app),
        "help" => app.panel = Some(Panel::Help { scroll: 0 }),
        "todos" => app.panel = Some(Panel::Todos { scroll: 0 }),
        "tasks" => controls::open_tasks(app),
        "sessions" => {
            app.sessions.clear();
            app.panel = Some(Panel::Sessions {
                selected: 0,
                loading: true,
            });
            app.session.send(SessionCommand::ShowSessions);
        }
        "skills" => app.session.send(SessionCommand::ShowSkills),
        "compact" => {
            app.start(i18n::text(app.lang, Key::StatusCompacting));
            app.session.send(SessionCommand::Compact);
        }
        _ => {
            app.hint = Some(i18n::fill(
                app.lang,
                Key::NoteUnknownCommand,
                &[("cmd", cmd)],
            ));
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sessions_picker_restores_on_enter_and_preserves_conversation_on_failure() {
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = test_app(session);
        app.push(EntryKind::User("current conversation".into()));
        app.input = input::editor("/sessions", Lang::Zh);
        submit(&mut app);
        assert!(matches!(
            commands.try_recv().unwrap(),
            SessionCommand::ShowSessions
        ));
        assert!(render(&mut app, 100, 24).contains("正在读取会话"));
        handle_ui_event(&mut app, UiEvent::Sessions(Vec::new()));
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(commands.try_recv().is_err());
        assert!(render(&mut app, 100, 24).contains("暂无已保存的会话"));
        let items = ["first", "second"]
            .into_iter()
            .map(|id| crate::agent::transcripts::SessionView {
                id: id.into(),
                title: format!("title {id}"),
                updated: "2026-09-18 10:00".into(),
                current: false,
            })
            .collect();
        handle_ui_event(&mut app, UiEvent::Sessions(items));
        assert!(render(&mut app, 100, 24).contains("title first"));
        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        render(&mut app, 20, 8);
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(commands.try_recv().is_err());
        assert!(
            app.transcript
                .entries()
                .iter()
                .any(|e| matches!(e, EntryKind::User(s) if s == "current conversation"))
        );
        app.panel = Some(Panel::Sessions {
            selected: 1,
            loading: false,
        });
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(commands.try_recv().unwrap(), SessionCommand::RestoreSession(id) if id == "second")
        );
        assert!(app.busy);
        handle_ui_event(
            &mut app,
            UiEvent::SessionRestoreFailed("missing file".into()),
        );
        assert!(!app.busy);
        assert!(
            app.transcript
                .entries()
                .iter()
                .any(|e| matches!(e, EntryKind::User(s) if s == "current conversation"))
        );
        handle_ui_event(
            &mut app,
            UiEvent::SessionRestored {
                id: "first".into(),
                records: vec![crate::agent::transcripts::Record {
                    ts: "then".into(),
                    role: "user".into(),
                    content: "restored conversation".into(),
                }],
            },
        );
        assert!(!app.busy);
        assert!(
            app.transcript
                .entries()
                .iter()
                .any(|e| matches!(e, EntryKind::User(s) if s == "restored conversation"))
        );
        assert!(
            !app.transcript
                .entries()
                .iter()
                .any(|e| matches!(e, EntryKind::User(s) if s == "current conversation"))
        );
        app.busy = true;
        app.input = input::editor("/sessions", Lang::Zh);
        submit(&mut app);
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn work_restore_replays_full_tool_details_todos_and_marks_unfinished_tools() {
        use crate::agent::plan::{TodoItem, TodoStatus};
        use crate::agent::work::Trace;
        let mut app = app();
        let output = "full result ".repeat(1000);
        handle_ui_event(
            &mut app,
            UiEvent::WorkRestored(vec![
                Trace::User("work".into()),
                Trace::Text("checking".into()),
                Trace::ToolStart {
                    id: "done".into(),
                    name: "bash".into(),
                    summary: "test".into(),
                    arguments: r#"{"command":"test"}"#.into(),
                },
                Trace::ToolEnd {
                    id: "done".into(),
                    output: output.clone(),
                    is_error: false,
                    duration_ms: 42,
                },
                Trace::Todos(vec![TodoItem {
                    content: "continue work".into(),
                    status: TodoStatus::InProgress,
                }]),
                Trace::ToolStart {
                    id: "pending".into(),
                    name: "bash".into(),
                    summary: "unfinished".into(),
                    arguments: "{}".into(),
                },
            ]),
        );
        let entries = app.transcript.entries();
        assert!(entries.iter().any(|entry| matches!(entry, EntryKind::Tool(tool) if tool.id == "done" && tool.output.as_deref() == Some(&output) && tool.duration_ms == Some(42) && tool.state == ToolState::Succeeded)));
        assert!(entries.iter().any(|entry| matches!(entry, EntryKind::Tool(tool) if tool.id == "pending" && tool.state == ToolState::Cancelled)));
        assert!(entries.iter().any(
            |entry| matches!(entry, EntryKind::Todos(items) if items[0].content == "continue work")
        ));
        assert!(!app.busy);
    }

    #[test]
    fn permission_picker_commands_events_cancel_and_busy_guard() {
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = test_app(session);
        app.input.insert_str("/permissions");
        submit(&mut app);
        assert!(matches!(
            app.panel,
            Some(Panel::Permissions { selected: 0 })
        ));
        let screen = render(&mut app, 110, 24);
        assert!(screen.contains("Ask When Need"));
        assert!(screen.contains("Never Ask"));
        assert!(screen.contains("Auto Edit"));
        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(
            commands.try_recv().unwrap(),
            SessionCommand::SetPermissionMode(PermissionMode::AskWhenNeed)
        ));
        assert_eq!(app.permission_mode, PermissionMode::Normal);
        handle_ui_event(
            &mut app,
            UiEvent::PermissionMode(PermissionMode::AskWhenNeed),
        );
        assert!(render(&mut app, 110, 24).contains("Ask When Need"));
        assert!(handle_command(&mut app, "permissions"));
        assert!(matches!(
            app.panel,
            Some(Panel::Permissions { selected: 1 })
        ));
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(commands.try_recv().is_err());
        assert!(!handle_command(&mut app, "permissions typo"));
        assert!(!handle_command(&mut app, "permissions never_ask extra"));
        assert!(commands.try_recv().is_err());
        assert!(handle_command(&mut app, "permissions auto_edit"));
        assert!(matches!(
            commands.try_recv().unwrap(),
            SessionCommand::SetPermissionMode(PermissionMode::AutoEdit)
        ));
        app.input = input::editor("/permissions never_ask", Lang::Zh);
        app.busy = true;
        submit(&mut app);
        assert!(commands.try_recv().is_err());
        assert_eq!(app.input.lines()[0], "/permissions never_ask");
        app.busy = false;
        submit(&mut app);
        assert!(matches!(
            commands.try_recv().unwrap(),
            SessionCommand::SetPermissionMode(PermissionMode::NeverAsk)
        ));
        handle_ui_event(&mut app, UiEvent::PermissionMode(PermissionMode::NeverAsk));
        assert!(handle_command(&mut app, "permissions"));
        render(&mut app, 20, 8);
    }

    #[test]
    fn model_picker_confirms_cancels_and_waits_for_authoritative_settings() {
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = test_app(session);
        handle_ui_event(
            &mut app,
            UiEvent::ModelSettings {
                model: "a".into(),
                models: vec!["a".into(), "b".into()],
                reasoning_efforts: vec!["high".into()],
                reasoning_effort: Some("high".into()),
                context_window: Some(128000),
            },
        );
        app.input.insert_str("/model");
        submit(&mut app);
        assert!(matches!(app.panel, Some(Panel::Model { selected: 0 })));
        assert!(render(&mut app, 100, 30).contains("a（当前）"));
        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(commands.try_recv().is_err());
        handle_command(&mut app, "model");
        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(commands.try_recv().unwrap(), SessionCommand::SelectModel(v) if v == "b"));
        assert!(app.panel.is_none());
        assert_eq!(app.model, "a");
        handle_ui_event(
            &mut app,
            UiEvent::ModelSettings {
                model: "b".into(),
                models: vec!["a".into(), "b".into()],
                reasoning_efforts: vec![],
                reasoning_effort: None,
                context_window: None,
            },
        );
        let screen = render(&mut app, 100, 30);
        assert_eq!(app.model, "b");
        assert!(!screen.contains(" · high"));
        assert!(!screen.contains("128000"));
        app.busy = true;
        app.input.insert_str("/model");
        submit(&mut app);
        assert!(app.panel.is_none());
        assert!(commands.try_recv().is_err());
        assert_eq!(app.input.lines()[0], "/model");
    }

    #[test]
    fn effort_picker_selects_current_cancels_and_confirms() {
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = test_app(session);
        handle_ui_event(
            &mut app,
            UiEvent::ModelSettings {
                model: "test".into(),
                models: vec!["test".into()],
                reasoning_efforts: vec!["low".into(), "high".into()],
                reasoning_effort: Some("low".into()),
                context_window: Some(128000),
            },
        );
        app.input.insert_str("/effort");
        submit(&mut app);
        assert!(matches!(app.panel, Some(Panel::Effort { selected: 0 })));
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("low（当前）"));
        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.panel.is_none());
        assert!(commands.try_recv().is_err());
        handle_command(&mut app, "effort");
        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(
            matches!(commands.try_recv().unwrap(), SessionCommand::SetReasoningEffort(v) if v == "high")
        );
        assert!(app.panel.is_none());
        // The authoritative session event, not merely focusing a row, updates the bar.
        assert_eq!(app.reasoning_effort.as_deref(), Some("low"));
    }

    #[test]
    fn status_colors_are_not_dimmed_by_the_right_hand_hint() {
        use ratatui::{Terminal, backend::TestBackend, style::Modifier};
        for (mode, color) in [
            (PermissionMode::Normal, theme::TEXT),
            (PermissionMode::AskWhenNeed, theme::WARNING),
            (PermissionMode::AutoEdit, theme::WARNING),
            (PermissionMode::NeverAsk, theme::ERROR),
        ] {
            let mut app = app();
            app.permission_mode = mode;
            app.model = "test-model".into();
            app.reasoning_effort = Some("medium".into());
            app.context_window = Some(100);
            app.context_used = Some(1);
            let mut terminal = Terminal::new(TestBackend::new(120, 24)).unwrap();
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(1, 23)].fg, theme::foreground(app.theme, color));
            let status = format!("{} · test-model · medium · CTX [1%]", mode.label());
            for x in 1..=Line::from(status).width() as u16 {
                assert!(!buffer[(x, 23)].modifier.contains(Modifier::DIM));
            }
            let model_x = 1 + Line::from(format!("{} · ", mode.label())).width() as u16;
            assert_eq!(
                buffer[(model_x, 23)].fg,
                theme::foreground(app.theme, theme::TEXT)
            );
            assert!(buffer[(113, 23)].modifier.contains(Modifier::DIM));
        }
    }

    #[test]
    fn statusbar_shows_usage_without_explanatory_labels() {
        let mut app = app();
        app.model = "deepseek-v4-flash".into();
        app.reasoning_effort = Some("medium".into());
        app.context_window = Some(1_000_000);
        handle_ui_event(&mut app, UiEvent::ContextUsage(Some(120_000)));
        let screen = render(&mut app, 120, 24);
        assert!(screen.contains("Normal · deepseek-v4-flash · medium · CTX [12%]"));
        for label in ["权限", "思考", "上下文", "tokens", "1000000"] {
            assert!(!screen.contains(label), "unexpected label: {label}");
        }
        handle_ui_event(&mut app, UiEvent::ContextUsage(Some(0)));
        assert!(render(&mut app, 120, 24).contains("CTX [0%]"));
        handle_ui_event(&mut app, UiEvent::ContextUsage(None));
        assert!(render(&mut app, 120, 24).contains("CTX [--%]"));
        handle_ui_event(&mut app, UiEvent::ContextUsage(Some(120_000)));
        handle_ui_event(&mut app, UiEvent::SessionReset);
        assert!(render(&mut app, 120, 24).contains("CTX [--%]"));
        app.context_used = Some(120_000);
        handle_ui_event(
            &mut app,
            UiEvent::SessionRestored {
                id: "saved".into(),
                records: vec![],
            },
        );
        assert!(app.context_used.is_none());
        app.context_used = Some(120_000);
        handle_ui_event(
            &mut app,
            UiEvent::ModelSettings {
                model: "other".into(),
                models: vec![],
                reasoning_efforts: vec![],
                reasoning_effort: None,
                context_window: Some(128_000),
            },
        );
        assert!(render(&mut app, 120, 24).contains("CTX [--%]"));
        for (width, height) in [(40, 10), (20, 8), (1, 1)] {
            render(&mut app, width, height);
        }
    }

    #[test]
    fn model_metadata_is_independently_optional_and_effort_requires_support() {
        let mut app = app();
        assert!(!render(&mut app, 100, 30).contains("tokens"));
        handle_command(&mut app, "effort");
        assert!(app.panel.is_none());
        assert!(render(&mut app, 100, 30).contains("未配置思考档位"));
        handle_ui_event(
            &mut app,
            UiEvent::ModelSettings {
                model: "test".into(),
                models: vec!["test".into()],
                reasoning_efforts: vec![],
                reasoning_effort: None,
                context_window: Some(128000),
            },
        );
        assert!(render(&mut app, 100, 30).contains("CTX [--%]"));
        handle_ui_event(
            &mut app,
            UiEvent::ModelSettings {
                model: "test".into(),
                models: vec!["test".into()],
                reasoning_efforts: vec!["high".into()],
                reasoning_effort: Some("high".into()),
                context_window: None,
            },
        );
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains(" · high"));
        assert!(!screen.contains("tokens"));
        app.busy = true;
        app.input.insert_str("/effort");
        submit(&mut app);
        assert!(app.panel.is_none());
        assert_eq!(app.input.lines()[0], "/effort");
    }

    fn app() -> App {
        let (session, _) = SessionHandle::test_channel();
        test_app(session)
    }

    /// TUI tests assert Chinese labels, so they run in Chinese; English
    /// rendering is covered by the `i18n` unit tests and the /lang test.
    fn test_app(session: SessionHandle) -> App {
        let mut app = App::new(session);
        app.set_lang(Lang::Zh);
        app
    }

    #[test]
    fn enter_while_busy_keeps_draft() {
        let mut app = app();
        app.busy = true;
        app.input.insert_str("下一步检查测试");
        submit(&mut app);
        assert_eq!(app.input.lines().join("\n"), "下一步检查测试");
    }

    #[test]
    fn unrelated_key_does_not_reject_permission() {
        let mut app = app();
        let (tx, mut rx) = oneshot::channel();
        handle_ui_event(
            &mut app,
            UiEvent::PermissionRequest {
                text: "bash(ls)".into(),
                respond: tx,
            },
        );
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert!(matches!(
            rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn streaming_does_not_interrupt_reading_history() {
        let mut app = app();
        app.push(EntryKind::Assistant("history\n\n".repeat(40)));
        render(&mut app, 80, 15);
        app.transcript.scroll(Scroll::Start);
        app.transcript.scroll(Scroll::Down(7));
        handle_ui_event(&mut app, UiEvent::Text("new content".into()));
        assert!(!app.transcript.following());
        assert_eq!(app.transcript.scroll_offset(), 7);
    }

    #[test]
    fn theme_selection_survives_restart_and_preserves_language() {
        let root = std::env::temp_dir().join(format!("koala-theme-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("config.toml");
        std::fs::write(&config, "theme = \"dark\"\nlang = \"zh\"\n").unwrap();
        let load = || {
            Config::load_files(
                std::slice::from_ref(&config),
                root.join(".koala/language.toml"),
            )
            .unwrap()
        };
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = App::new(session);
        app.configure_appearance(&load());
        assert_eq!(app.theme, Theme::Dark);
        app.busy = true;
        for theme in [Theme::Light, Theme::Dark, Theme::Auto] {
            app.input = input::editor(&format!("/theme {}", theme.code()), app.lang);
            submit(&mut app);
            assert_eq!(app.theme, theme);
            assert_eq!(load().theme, theme);
            assert_eq!(load().lang, Lang::Zh);
            assert!(app.busy);
            assert!(commands.try_recv().is_err());
        }
        app.input = input::editor("/theme invalid", app.lang);
        submit(&mut app);
        assert_eq!(app.theme, Theme::Auto);
        assert_eq!(app.input.lines(), ["/theme invalid"]);
        assert_eq!(load().theme, Theme::Auto);
        assert!(!handle_command(&mut app, "theme dark extra"));
        app.input = input::editor("/theme", app.lang);
        submit(&mut app);
        assert!(matches!(app.panel, Some(Panel::Theme { selected: 0 })));
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("选择主题"));
        assert!(screen.contains("auto（当前）"));
        assert!(screen.contains("跟随终端"));
        app.input.insert_str("未发送草稿");
        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.theme, Theme::Auto);
        assert_eq!(load().theme, Theme::Auto);
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.panel.is_none());
        assert_eq!(load().theme, Theme::Auto);
        assert_eq!(app.input.lines(), ["未发送草稿"]);
        handle_command(&mut app, "theme");
        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert!(matches!(app.panel, Some(Panel::Theme { selected: 2 })));
        render(&mut app, 20, 8);
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(app.panel.is_none());
        assert_eq!(app.theme, Theme::Dark);
        assert_eq!(load().theme, Theme::Dark);
        assert_eq!(app.input.lines(), ["未发送草稿"]);
        assert!(app.busy);
        assert!(commands.try_recv().is_err());
        handle_command(&mut app, "theme");
        assert!(matches!(app.panel, Some(Panel::Theme { selected: 2 })));
        handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert!(matches!(app.panel, Some(Panel::Theme { selected: 0 })));
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(load().theme, Theme::Auto);
        let mut cfg = load();
        cfg.apply_env_with(|key| (key == "KOALA_THEME").then(|| "light".into()));
        assert_eq!(cfg.theme, Theme::Light);
        cfg.apply_env_with(|key| (key == "KOALA_THEME").then(|| "invalid".into()));
        assert_eq!(cfg.theme, Theme::Light);
        drop(app);
        let (session, _) = SessionHandle::test_channel();
        let mut app = App::new(session);
        app.configure_appearance(&load());
        assert_eq!(app.theme, Theme::Auto);
        app.input = input::editor("/lang en", app.lang);
        submit(&mut app);
        assert_eq!(load().theme, Theme::Auto);
        assert_eq!(load().lang, Lang::En);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn themes_recolor_cached_content_editor_and_modal_without_losing_draft() {
        use ratatui::{Terminal, backend::TestBackend, style::Color};
        let mut app = app();
        app.push(EntryKind::Assistant("Cached answer".into()));
        app.input.insert_str("unsent draft");
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        for theme in [Theme::Dark, Theme::Light, Theme::Auto, Theme::Dark] {
            app.theme = theme;
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(0, 0)].bg, theme::background(theme));
            for needle in ["Cached answer", "unsent draft"] {
                let cell = buffer
                    .content
                    .iter()
                    .find(|cell| cell.symbol() == &needle[..1])
                    .unwrap();
                assert_eq!(cell.fg, theme::foreground(theme, theme::TEXT));
            }
            assert_eq!(app.input.lines(), ["unsent draft"]);
            app.panel = Some(Panel::Help { scroll: 0 });
            terminal.draw(|f| draw(f, &mut app)).unwrap();
            // Cleared modal regions must inherit the selected background too.
            assert_eq!(
                terminal.backend().buffer()[(50, 5)].bg,
                theme::background(theme)
            );
            app.panel = None;
        }
        app.theme = Theme::Auto;
        terminal.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(terminal.backend().buffer()[(0, 0)].bg, Color::Reset);
        assert!(
            input::commands(Lang::En)
                .iter()
                .any(|(name, _)| *name == "theme")
        );
    }

    #[test]
    fn theme_save_failure_is_reported() {
        let root = std::env::temp_dir().join(format!("koala-theme-{}", uuid::Uuid::new_v4()));
        std::fs::write(&root, "not a directory").unwrap();
        let mut app = app();
        app.theme_path = Some(root.join("theme.toml"));
        assert!(handle_command(&mut app, "theme light"));
        assert_eq!(app.theme, Theme::Light);
        assert!(
            app.transcript
                .entries()
                .iter()
                .any(|e| matches!(e, EntryKind::Note(s) if s.contains("无法保存主题偏好")))
        );
        std::fs::remove_file(root).unwrap();
    }

    #[test]
    fn lang_command_survives_restart() {
        let root = std::env::temp_dir().join(format!("koala-language-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let config = root.join("config.toml");
        let source = "# Keep this comment and setting\nlang = \"en\"\n[llm]\nmodel = \"test\"\n";
        std::fs::write(&config, source).unwrap();
        let preference = root.join(".koala/language.toml");
        let load =
            || Config::load_files(std::slice::from_ref(&config), preference.clone()).unwrap();
        let cfg = load();
        assert_eq!(cfg.lang, Lang::En);
        let (session, _) = SessionHandle::test_channel();
        let mut app = App::new(session);
        app.configure_appearance(&cfg);
        app.input = input::editor("/lang", cfg.lang);
        submit(&mut app);
        assert_eq!(app.lang, Lang::Zh);
        drop(app);
        let cfg = load();
        let (session, _) = SessionHandle::test_channel();
        let mut app = App::new(session);
        app.configure_appearance(&cfg);
        assert_eq!(app.lang, Lang::Zh);
        assert_eq!(std::fs::read_to_string(&config).unwrap(), source);
        app.input = input::editor("/lang en", app.lang);
        submit(&mut app);
        assert_eq!(load().lang, Lang::En);
        app.input = input::editor("/lang invalid", app.lang);
        submit(&mut app);
        assert_eq!(load().lang, Lang::En);
        let mut cfg = load();
        cfg.apply_env_with(|key| (key == "KOALA_LANG").then(|| "zh".into()));
        assert_eq!(cfg.lang, Lang::Zh);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn language_save_failure_is_visible_and_does_not_block_switching() {
        let root = std::env::temp_dir().join(format!("koala-language-{}", uuid::Uuid::new_v4()));
        std::fs::write(&root, "not a directory").unwrap();
        let mut app = app();
        app.language_path = Some(root.join("language.toml"));
        app.input = input::editor("/lang zh", Lang::En);
        submit(&mut app);
        assert_eq!(app.lang, Lang::Zh);
        assert!(
            app.transcript
                .entries()
                .iter()
                .any(|e| matches!(e, EntryKind::Note(s) if s.contains("无法保存语言偏好")))
        );
        std::fs::remove_file(root).unwrap();
    }

    #[test]
    fn lang_command_toggles_labels_and_tells_the_agent() {
        let (session, mut commands) = SessionHandle::test_channel();
        // The default is English, so a fresh app starts in English.
        let mut app = App::new(session);
        app.input = input::editor("/lang", Lang::En);
        submit(&mut app);
        assert_eq!(app.lang, Lang::Zh);
        assert!(matches!(
            commands.try_recv().unwrap(),
            SessionCommand::SetLang(Lang::Zh)
        ));
        assert!(
            app.transcript
                .entries()
                .iter()
                .any(|e| matches!(e, EntryKind::Info(s) if s == "语言：中文"))
        );
        assert!(render(&mut app, 100, 24).contains("? 帮助"));
        // An explicit code pins the language instead of toggling.
        app.input = input::editor("/lang en", Lang::Zh);
        submit(&mut app);
        assert_eq!(app.lang, Lang::En);
        assert!(render(&mut app, 100, 24).contains("? help"));
        // Unknown codes are refused, and the unparsed argument is kept.
        app.input = input::editor("/lang klingon", Lang::En);
        submit(&mut app);
        assert_eq!(app.lang, Lang::En);
        assert!(app.hint.as_deref().unwrap().contains("Usage: /lang"));
        assert_eq!(app.input.lines(), ["/lang klingon"]);
        assert!(matches!(
            commands.try_recv().unwrap(),
            SessionCommand::SetLang(Lang::En)
        ));
    }

    #[test]
    fn switching_language_rerenders_cached_labels_but_keeps_the_draft() {
        let (session, _) = SessionHandle::test_channel();
        let mut app = App::new(session);
        app.input = input::editor("未发送草稿", Lang::En);
        app.push(EntryKind::Tool(ToolEntry {
            id: "1".into(),
            name: "bash".into(),
            summary: String::new(),
            arguments: "{}".into(),
            output: None,
            state: ToolState::Running,
            duration_ms: None,
        }));
        assert!(render(&mut app, 90, 24).contains("running"));
        app.set_lang(Lang::Zh);
        assert!(render(&mut app, 90, 24).contains("进行中"));
        assert_eq!(app.input.lines(), ["未发送草稿"]);
        // The composer placeholder follows the language too.
        app.set_lang(Lang::En);
        assert!(render(&mut app, 90, 24).contains("running"));
    }

    #[test]
    fn memory_commands_route_without_sending_a_model_turn() {
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = App::new(session);
        app.input = input::editor("/memory write off", Lang::En);
        submit(&mut app);
        assert!(matches!(
            commands.try_recv().unwrap(),
            SessionCommand::Memory {
                control: Some(crate::agent::event::MemoryControl::Write(false))
            }
        ));
        app.input = input::editor("/memory", Lang::En);
        submit(&mut app);
        assert!(matches!(
            commands.try_recv().unwrap(),
            SessionCommand::Memory { control: None }
        ));
        app.input = input::editor("/memory read maybe", Lang::En);
        submit(&mut app);
        assert!(commands.try_recv().is_err());
        assert_eq!(app.input.lines(), ["/memory read maybe"]);
    }

    #[test]
    fn language_command_is_listed_in_the_completion_menu_and_help() {
        assert!(
            input::commands(Lang::En)
                .iter()
                .any(|(n, d)| *n == "lang" && *d == "Switch the interface language")
        );
        assert!(
            input::commands(Lang::Zh)
                .iter()
                .any(|(n, d)| *n == "lang" && *d == "切换界面语言")
        );
        let mut app = app();
        app.input = input::editor("/la", Lang::Zh);
        assert!(!input::matches(&app.input).is_empty());
        app.panel = Some(Panel::Help { scroll: 0 });
        assert!(render(&mut app, 100, 40).contains("/lang"));
    }

    #[test]
    fn escape_does_not_exit() {
        let mut app = app();
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.quit);
    }
    fn render(app: &mut App, width: u16, height: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for row in buffer.content.chunks(width as usize) {
            let mut col = 0;
            while col < row.len() {
                let symbol = row[col].symbol();
                text.push_str(symbol);
                col += Line::from(symbol).width().max(1);
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn page_up_starts_at_visible_bottom_and_new_output_stays_unread() {
        let mut app = app();
        app.push(EntryKind::Assistant(
            (0..100).map(|i| format!("line {i}\n\n")).collect(),
        ));
        render(&mut app, 80, 24);
        let bottom = app.transcript.scroll_offset();
        assert!(bottom > 10);
        handle_key(&mut app, KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.transcript.scroll_offset(), bottom - 10);
        handle_ui_event(&mut app, UiEvent::Note("background finished".into()));
        let screen = render(&mut app, 80, 24);
        assert_eq!(app.transcript.scroll_offset(), bottom - 10);
        assert!(screen.contains("新内容"));
        for _ in 0..20 {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            );
        }
        assert!(app.transcript.following());
        assert!(!app.transcript.unread());
    }

    #[test]
    fn permission_selection_requires_confirmation_and_escape_declines() {
        for allow in [true, false] {
            let mut app = app();
            let (tx, mut rx) = oneshot::channel();
            handle_ui_event(
                &mut app,
                UiEvent::PermissionRequest {
                    text: "bash(ls)".into(),
                    respond: tx,
                },
            );
            if allow {
                handle_key(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
                assert!(matches!(
                    rx.try_recv(),
                    Err(oneshot::error::TryRecvError::Empty)
                ));
            }
            let key = if allow { KeyCode::Enter } else { KeyCode::Esc };
            handle_key(&mut app, KeyEvent::new(key, KeyModifiers::NONE));
            assert_eq!(rx.try_recv().unwrap(), allow);
            assert!(!app.quit);
        }
    }

    #[test]
    fn reset_waits_for_backend_and_clears_old_view_state() {
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = test_app(session);
        app.push(EntryKind::Assistant("old session".into()));
        app.busy = true;
        app.input.insert_str("/new");
        submit(&mut app);
        assert!(matches!(
            commands.try_recv(),
            Ok(SessionCommand::NewSession)
        ));
        assert!(app.restarting);
        handle_ui_event(&mut app, UiEvent::Done);
        assert!(app.busy);
        handle_ui_event(&mut app, UiEvent::SessionReset);
        let screen = render(&mut app, 80, 24);
        assert!(!screen.contains("old session"));
        assert!(screen.contains("新会话已开始"));
        assert!(!app.busy);
        assert!(app.transcript.following());
    }

    #[test]
    fn cancel_keeps_draft_and_sends_command_instead_of_quitting() {
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = test_app(session);
        app.start("正在生成");
        app.input.insert_str("draft");
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(commands.try_recv(), Ok(SessionCommand::Cancel)));
        handle_ui_event(&mut app, UiEvent::Cancelled);
        assert_eq!(app.input.lines().join("\n"), "draft");
        assert!(!app.quit);
        assert!(!app.busy);
    }

    #[test]
    fn modal_band_hides_transcript_fragments_and_restores_on_close() {
        let mut app = app();
        app.push(EntryKind::Assistant(
            (0..60).map(|_| "Z".repeat(110) + "\n\n").collect(),
        ));
        app.input.insert_str("draft");
        let before = render(&mut app, 120, 32);
        app.panel = Some(Panel::Permissions { selected: 0 });
        let screen = render(&mut app, 120, 32);
        let lines: Vec<_> = screen.lines().collect();
        let top = lines
            .iter()
            .position(|line| line.contains("权限等级"))
            .unwrap();
        let bottom = lines
            .iter()
            .position(|line| line.contains("Enter 确认"))
            .unwrap();
        for line in &lines[top.saturating_sub(1)..=(bottom + 1)] {
            assert!(!line.contains('Z'), "transcript leaks beside modal: {line}");
        }
        assert!(screen.contains("Never Ask"));
        assert!(screen.contains("Auto Edit"));
        assert!(screen.contains("draft"));
        assert!(
            lines[..top.saturating_sub(1)]
                .iter()
                .any(|line| line.contains('Z'))
        );
        controls::close_panel(&mut app);
        assert_eq!(render(&mut app, 120, 32), before);
        let (tx, _rx) = oneshot::channel();
        handle_ui_event(
            &mut app,
            UiEvent::PermissionRequest {
                text: "run command".into(),
                respond: tx,
            },
        );
        let screen = render(&mut app, 120, 32);
        let lines: Vec<_> = screen.lines().collect();
        let top = lines
            .iter()
            .position(|line| line.contains("权限确认"))
            .unwrap();
        let hint = lines
            .iter()
            .position(|line| line.contains("方向键选择"))
            .unwrap();
        for line in &lines[top.saturating_sub(1)..=(hint + 2)] {
            assert!(
                !line.contains('Z'),
                "transcript leaks beside permission: {line}"
            );
        }
        for (width, height) in [(40, 12), (20, 8), (1, 1)] {
            render(&mut app, width, height);
        }
    }

    #[test]
    fn status_permission_and_small_terminal_render() {
        let mut app = app();
        app.start("正在执行 bash");
        let (tx, _rx) = oneshot::channel();
        handle_ui_event(
            &mut app,
            UiEvent::PermissionRequest {
                text: "bash\ncommand: cargo test\n".repeat(20),
                respond: tx,
            },
        );
        let screen = render(&mut app, 80, 24);
        assert!(screen.contains("权限确认"));
        assert!(screen.contains("等待权限确认"));
        assert!(screen.contains("Enter 确认"));
        for (width, height) in [(20, 8), (5, 3), (1, 1)] {
            render(&mut app, width, height);
        }
    }
    #[test]
    fn tool_results_match_ids_and_details_preserve_draft_and_viewport() {
        let mut app = app();
        for id in ["one", "two"] {
            handle_ui_event(
                &mut app,
                UiEvent::ToolStart {
                    id: id.into(),
                    name: "bash".into(),
                    summary: "cargo test".into(),
                    arguments: "{\"command\":\"cargo test\"}".into(),
                },
            );
        }
        handle_ui_event(&mut app, UiEvent::Note("background finished".into()));
        handle_ui_event(
            &mut app,
            UiEvent::ToolEnd {
                id: "one".into(),
                output: (0..10).map(|i| format!("result line {i}\n")).collect(),
                is_error: true,
                duration_ms: 1200,
            },
        );
        let EntryKind::Tool(first) = &app.transcript.entries()[0] else {
            panic!("missing first tool")
        };
        assert!(first.state == ToolState::Failed);
        let EntryKind::Tool(second) = &app.transcript.entries()[1] else {
            panic!("missing second tool")
        };
        assert!(second.state == ToolState::Running);
        let compact = render(&mut app, 100, 40);
        assert!(compact.contains("失败 · 1.2s"));
        assert!(compact.contains("Ctrl+O 展开"));
        assert!(!compact.contains("result line 8"));
        render(&mut app, 100, 10);
        app.transcript.scroll(Scroll::Start);
        app.transcript.scroll(Scroll::Down(2));
        app.input.insert_str("keep this draft");
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
        );
        let details = render(&mut app, 100, 40);
        assert!(details.contains("result line 8"));
        assert!(details.contains("command"));
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.input.lines().join("\n"), "keep this draft");
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.transcript.detailed());
        assert!(!app.transcript.following());
        assert_eq!(app.transcript.scroll_offset(), 2);
        assert!(!app.quit);
    }

    #[test]
    fn startup_metadata_and_resizing_show_real_state() {
        let mut app = app();
        app.model = "local-model".into();
        app.directory = "/workspace/koala".into();
        handle_ui_event(&mut app, UiEvent::PlanMode(true));
        handle_ui_event(&mut app, UiEvent::BackgroundCount(2));
        let screen = render(&mut app, 100, 28);
        assert!(screen.contains("koala"));
        assert!(screen.contains("local-model"));
        assert!(screen.contains("/workspace/koala"));
        assert!(screen.contains("Plan · 后台 2"));
        assert!(screen.contains("输入消息"));
        assert!(screen.contains("╭"));
        handle_ui_event(
            &mut app,
            UiEvent::Text(
                "## 进展\n\n中文内容很长，用于验证窄屏换行。\n\n```rust\nlet x = 42;\n```".into(),
            ),
        );
        for (width, height) in [(40, 20), (20, 8), (5, 3), (1, 1)] {
            render(&mut app, width, height);
        }
        handle_ui_event(&mut app, UiEvent::BackgroundCount(0));
        assert!(!render(&mut app, 100, 28).contains("后台 0"));
    }
    #[test]
    fn layout_fixture() {
        let mut app = app();
        app.model = "local-model".into();
        app.directory = "/workspace/koala".into();
        app.push(EntryKind::User("检查任务执行情况，并列出下一步。".into()));
        app.push(EntryKind::Assistant("## 检查结果\n\n已确认 **核心交互** 正常，接下来检查：\n\n- 工具输出与错误状态\n- 中文换行和窄窗口显示\n\n```rust\nlet ready = true;\n```".into()));
        handle_ui_event(
            &mut app,
            UiEvent::ToolStart {
                id: "fixture".into(),
                name: "bash".into(),
                summary: "cargo test".into(),
                arguments: "{\"command\":\"cargo test\"}".into(),
            },
        );
        handle_ui_event(
            &mut app,
            UiEvent::ToolEnd {
                id: "fixture".into(),
                output: (1..9).map(|i| format!("test case_{i} ... ok\n")).collect(),
                is_error: false,
                duration_ms: 840,
            },
        );
        handle_ui_event(&mut app, UiEvent::BackgroundCount(1));
        let screen = render(&mut app, 94, 34);
        println!("{screen}");
        assert!(screen.contains("检查结果"));
        assert!(screen.contains("成功 · 0.8s"));
        assert!(screen.contains("后台 1 · local-model"));
    }
    #[test]
    fn command_menu_filters_completes_and_does_not_send_on_tab() {
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = test_app(session);
        app.input.insert_str("/ta");
        assert!(render(&mut app, 90, 24).contains("/tasks"));
        handle_key(&mut app, KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(app.input.lines(), ["/tasks"]);
        assert!(commands.try_recv().is_err());
        app.busy = true;
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(matches!(app.panel, Some(Panel::Tasks { .. })));
        assert!(matches!(commands.try_recv(), Ok(SessionCommand::ShowTasks)));
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(commands.try_recv(), Ok(SessionCommand::HideTasks)));
        app.input = input::editor("/does-not-exist", Lang::Zh);
        app.busy = false;
        submit(&mut app);
        assert!(app.hint.as_deref().unwrap().contains("未知命令"));
        assert_eq!(app.input.lines(), ["/does-not-exist"]);
    }

    #[test]
    fn paste_and_multiline_shortcuts_never_submit() {
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = test_app(session);
        handle_paste(&mut app, "第一行\r\n第二行\n/quit");
        assert_eq!(app.input.lines(), ["第一行", "第二行", "/quit"]);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
        );
        assert_eq!(app.input.lines().len(), 4);
        app.input.insert_str("continued\\");
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.input.lines()[3], "continued");
        assert_eq!(app.input.lines().len(), 5);
        assert!(!app.quit);
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn history_search_accepts_without_sending_and_cancel_keeps_draft() {
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = test_app(session);
        app.history.record("检查代码\n运行测试").unwrap();
        app.history.record("另一个任务").unwrap();
        app.input.insert_str("当前草稿");
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        );
        handle_paste(&mut app, "测试");
        assert!(render(&mut app, 80, 24).contains("检查代码 ↵ 运行测试"));
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.input.lines(), ["当前草稿"]);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL),
        );
        handle_paste(&mut app, "测试");
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(app.input.lines(), ["检查代码", "运行测试"]);
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn arrows_move_within_multiline_before_recalling_history() {
        let mut app = app();
        app.history.record("old input").unwrap();
        app.input = input::editor("first\nsecond", Lang::Zh);
        handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input.lines(), ["first", "second"]);
        handle_key(&mut app, KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(app.input.lines(), ["old input"]);
        handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        assert_eq!(app.input.lines(), ["first", "second"]);
    }

    #[test]
    fn mode_shortcut_waits_for_backend_and_busy_mode_is_unchanged() {
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = test_app(session);
        app.input.insert_str("draft");
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        );
        assert!(matches!(
            commands.try_recv(),
            Ok(SessionCommand::TogglePlanMode)
        ));
        assert!(!app.plan_mode);
        handle_ui_event(&mut app, UiEvent::PlanMode(true));
        assert!(app.plan_mode);
        app.busy = true;
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        );
        assert!(commands.try_recv().is_err());
        assert!(app.plan_mode);
        assert_eq!(app.input.lines(), ["draft"]);
    }

    #[test]
    fn todo_shortcut_shows_full_list_without_opening_tool_details() {
        use crate::agent::event::{TodoState, TodoView};
        let mut app = app();
        app.input.insert_str("keep draft");
        handle_ui_event(
            &mut app,
            UiEvent::Todos(
                (0..30)
                    .map(|i| TodoView {
                        content: format!("todo item {i:02}"),
                        status: TodoState::Pending,
                    })
                    .collect(),
            ),
        );
        let compact = render(&mut app, 90, 24);
        assert!(
            compact.contains("Ctrl+T"),
            "Todo overflow must advertise its own shortcut"
        );
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        );
        assert!(!app.transcript.detailed());
        for (width, height) in [(40, 12), (20, 8), (1, 1)] {
            render(&mut app, width, height);
        }
        let mut visible = String::new();
        for _ in 0..40 {
            visible.push_str(&render(&mut app, 90, 24));
            handle_key(&mut app, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        for i in 0..30 {
            assert!(
                visible.contains(&format!("todo item {i:02}")),
                "missing todo {i}"
            );
        }
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        );
        assert!(app.panel.is_none());
        assert_eq!(app.input.lines(), ["keep draft"]);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL),
        );
        assert!(app.transcript.detailed());
    }

    #[test]
    fn todo_panel_highlights_active_and_closes_without_losing_data() {
        use crate::agent::event::{TodoState, TodoView};
        let mut app = app();
        handle_ui_event(
            &mut app,
            UiEvent::Todos(vec![
                TodoView {
                    content: "已完成项目".into(),
                    status: TodoState::Done,
                },
                TodoView {
                    content: "当前正在处理".into(),
                    status: TodoState::InProgress,
                },
            ]),
        );
        let screen = render(&mut app, 90, 24);
        assert!(screen.contains("▾ 1/2"));
        assert!(screen.contains("◐ 当前正在处理"));
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        );
        assert!(matches!(app.panel, Some(Panel::Todos { .. })));
        assert!(render(&mut app, 90, 24).contains("◐ 当前正在处理"));
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(app.panel.is_none());
        assert!(render(&mut app, 90, 24).contains("▾ 1/2"));
        assert_eq!(panels::todos(&app).len(), 2);
    }

    #[test]
    fn task_output_stop_and_panels_preserve_drafts_and_render_narrow() {
        use crate::agent::event::TaskState;
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = test_app(session);
        app.input.insert_str("keep me");
        controls::open_tasks(&mut app);
        assert!(matches!(commands.try_recv(), Ok(SessionCommand::ShowTasks)));
        handle_ui_event(
            &mut app,
            UiEvent::Tasks(vec![TaskView {
                id: 7,
                kind: "bash".into(),
                description: "sleep 30".into(),
                status: TaskState::Running,
                output: "full output".into(),
                elapsed_ms: 2000,
            }]),
        );
        assert!(render(&mut app, 90, 24).contains("#7 [bash] 进行中"));
        handle_key(&mut app, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(render(&mut app, 90, 24).contains("full output"));
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );
        assert!(matches!(
            commands.try_recv(),
            Ok(SessionCommand::StopTask(7))
        ));
        for (width, height) in [(20, 8), (5, 3), (1, 1)] {
            render(&mut app, width, height);
        }
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(app.input.lines(), ["keep me"]);
        app.input = new_input(Lang::Zh);
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );
        assert!(matches!(app.panel, Some(Panel::Help { .. })));
        for (width, height) in [(90, 24), (20, 8), (1, 1)] {
            render(&mut app, width, height);
        }
    }
}
