mod controls;
mod input;
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
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures_util::StreamExt;
use ratatui::style::Style;
#[cfg(test)]
use ratatui::text::Line;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use transcript::{EntryKind, Rendered, ToolEntry, ToolState};
use tui_textarea::TextArea;
use view::draw;

enum Panel {
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
    tasks_received: Instant,
    todos_expanded: bool,
    model: String,
    directory: String,
    plan_mode: bool,
    background_count: usize,
    detailed: bool,
    compact_viewport: Option<(usize, bool)>,
    rendered: Option<Rendered>,
    input: TextArea<'static>,
    entries: Vec<EntryKind>,
    assistant_open: bool,
    last_todos: Option<usize>,
    permission: Option<PermissionPrompt>,
    busy: bool,
    restarting: bool,
    status: String,
    started: Option<Instant>,
    hint: Option<String>,
    follow: bool,
    unread: bool,
    scroll: usize,
    bottom: usize,
    quit: bool,
}

fn new_input() -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_placeholder_text("输入消息 · / 命令 · ? 帮助");
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
            tasks_received: Instant::now(),
            todos_expanded: true,
            model: String::new(),
            directory: String::new(),
            plan_mode: false,
            background_count: 0,
            detailed: false,
            compact_viewport: None,
            rendered: None,
            input: new_input(),
            entries: Vec::new(),
            assistant_open: false,
            last_todos: None,
            permission: None,
            busy: false,
            restarting: false,
            status: String::new(),
            started: None,
            hint: None,
            follow: true,
            unread: false,
            scroll: 0,
            bottom: 0,
            quit: false,
        }
    }

    fn push(&mut self, entry: EntryKind) {
        self.rendered = None;
        self.entries.push(entry);
        self.assistant_open = false;
        self.unread |= !self.follow;
    }

    fn toggle_details(&mut self) {
        if self.detailed {
            if let Some((scroll, follow)) = self.compact_viewport.take() {
                self.scroll = scroll;
                self.follow = follow;
                if follow {
                    self.unread = false;
                }
            }
        } else {
            self.compact_viewport = Some((self.scroll, self.follow));
            self.follow = true;
        }
        self.detailed = !self.detailed;
        self.rendered = None;
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
        self.assistant_open = false;
        if !self.restarting {
            self.busy = false;
            self.started = None;
            self.status.clear();
        }
    }

    fn cancel(&mut self) {
        if self.busy && !self.restarting {
            self.session.send(SessionCommand::Cancel);
            self.status = "正在中断".into();
            self.permission = None;
        }
    }
}

pub async fn run(cfg: &Config) -> anyhow::Result<()> {
    let (handle, events) = session::spawn(Agent::new(cfg)?);
    let mut app = App::new(handle);
    app.model = cfg.llm.model.clone();
    app.directory = std::env::current_dir()?.display().to_string();
    match input::History::load(
        cfg.agent.session_dir.join(".input-history"),
        app.directory.clone(),
    ) {
        Ok(history) => app.history = history,
        Err(e) => app.push(EntryKind::Note(format!("输入历史读取失败：{e}"))),
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
    if !matches!(
        &ev,
        UiEvent::Status(_) | UiEvent::BackgroundCount(_) | UiEvent::PlanMode(_) | UiEvent::Tasks(_)
    ) {
        app.rendered = None;
    }
    match ev {
        UiEvent::Tasks(tasks) => {
            app.tasks = tasks;
            app.tasks_received = Instant::now();
            if let Some(Panel::Tasks { selected, .. }) = &mut app.panel
                && !app.tasks.iter().any(|t| Some(t.id) == *selected)
            {
                *selected = app.tasks.first().map(|t| t.id);
            }
        }
        UiEvent::PlanMode(on) => app.plan_mode = on,
        UiEvent::BackgroundCount(n) => app.background_count = n,
        UiEvent::Status(status) => app.status = status,
        UiEvent::Text(delta) => {
            match app.entries.last_mut() {
                Some(EntryKind::Assistant(text)) if app.assistant_open => text.push_str(&delta),
                _ => {
                    app.entries.push(EntryKind::Assistant(delta));
                    app.assistant_open = true;
                }
            }
            app.unread |= !app.follow;
        }
        UiEvent::ToolStart {
            id,
            name,
            summary,
            arguments,
        } => {
            app.status = format!("正在执行 {name}");
            app.push(EntryKind::Tool(ToolEntry {
                id,
                name,
                summary,
                arguments,
                output: None,
                state: ToolState::Running,
                duration_ms: None,
            }));
        }
        UiEvent::ToolEnd {
            id,
            output,
            is_error,
            duration_ms,
        } => {
            if let Some(EntryKind::Tool(tool)) = app
                .entries
                .iter_mut()
                .rev()
                .find(|e| matches!(e, EntryKind::Tool(tool) if tool.id == id))
            {
                tool.output = Some(output);
                tool.state = if is_error {
                    ToolState::Failed
                } else {
                    ToolState::Succeeded
                };
                tool.duration_ms = Some(duration_ms);
            }
            app.status = "正在处理结果".into();
            app.unread |= !app.follow;
        }
        UiEvent::Todos(items) => {
            match app.last_todos {
                Some(idx) => app.entries[idx] = EntryKind::Todos(items),
                None => {
                    app.last_todos = Some(app.entries.len());
                    app.push(EntryKind::Todos(items));
                }
            }
            app.unread |= !app.follow;
        }
        UiEvent::Note(text) => app.push(EntryKind::Note(text)),
        UiEvent::Info(text) => app.push(EntryKind::Info(text)),
        UiEvent::PermissionRequest { text, respond } => {
            app.status = "等待权限确认".into();
            app.permission = Some(PermissionPrompt {
                text,
                respond,
                allow: false,
                scroll: 0,
            });
        }
        UiEvent::Done => app.finish(),
        UiEvent::Cancelled => {
            for entry in &mut app.entries {
                if let EntryKind::Tool(tool) = entry
                    && tool.state == ToolState::Running
                {
                    tool.state = ToolState::Cancelled;
                    tool.output = Some("已中断（已完成的操作不会撤销）".into());
                }
            }
            app.finish();
            app.push(EntryKind::Note(
                "已中断，可以继续输入。后台任务仍会继续运行。".into(),
            ));
        }
        UiEvent::SessionReset => {
            controls::close_panel(app);
            app.entries.clear();
            app.detailed = false;
            app.compact_viewport = None;
            app.last_todos = None;
            app.restarting = false;
            app.finish();
            app.follow = true;
            app.unread = false;
            app.scroll = 0;
            app.bottom = 0;
            app.hint = None;
            app.push(EntryKind::Info("新会话已开始".into()));
        }
        UiEvent::Error(err) => {
            app.permission = None;
            for entry in &mut app.entries {
                if let EntryKind::Tool(tool) = entry
                    && tool.state == ToolState::Running
                {
                    tool.state = ToolState::Failed;
                    tool.output = Some(err.clone());
                }
            }
            app.push(EntryKind::Error(err));
        }
    }
}

fn handle_paste(app: &mut App, pasted: &str) {
    if app.permission.is_some() || app.detailed {
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
                    app.input = new_input();
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
            app.status = "正在继续".into();
            app.push(EntryKind::Note(
                if allow {
                    "已允许本次操作"
                } else {
                    "已拒绝本次操作"
                }
                .into(),
            ));
        }
        return;
    }
    if app.detailed {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => app.toggle_details(),
            KeyCode::PageUp | KeyCode::Up => {
                app.follow = false;
                app.scroll =
                    app.scroll
                        .saturating_sub(if key.code == KeyCode::Up { 1 } else { 10 });
            }
            KeyCode::PageDown | KeyCode::Down => {
                app.scroll = app
                    .scroll
                    .saturating_add(if key.code == KeyCode::Down { 1 } else { 10 })
                    .min(app.bottom);
                if app.scroll == app.bottom {
                    app.follow = true;
                    app.unread = false;
                }
            }
            KeyCode::Home => {
                app.follow = false;
                app.scroll = 0;
            }
            KeyCode::End => {
                app.follow = true;
                app.unread = false;
            }
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
        KeyCode::PageUp => {
            app.follow = false;
            app.scroll = app.scroll.saturating_sub(10);
        }
        KeyCode::PageDown => {
            app.scroll = app.scroll.saturating_add(10).min(app.bottom);
            if app.scroll == app.bottom {
                app.follow = true;
                app.unread = false;
            }
        }
        KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.follow = true;
            app.unread = false;
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
        app.push(EntryKind::Note(format!("输入历史保存失败：{e}")));
    }
    app.input = new_input();
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
        app.start("正在创建新会话");
        app.session.send(SessionCommand::NewSession);
        return;
    }
    if matches!(text.as_str(), "/help" | "/tasks" | "/todos") {
        consume_input(app, &text);
        handle_command(app, text.trim_start_matches('/'));
        return;
    }
    if app.busy {
        app.hint = Some("正在执行，草稿已保留；Esc 中断后可发送".into());
        return;
    }
    if let Some(cmd) = text.strip_prefix('/') {
        if handle_command(app, cmd) {
            consume_input(app, &text);
        }
        return;
    }
    consume_input(app, &text);
    app.follow = true;
    app.unread = false;
    app.push(EntryKind::User(text.clone()));
    app.start("正在生成");
    app.session.send(SessionCommand::Submit(text));
}

fn handle_command(app: &mut App, cmd: &str) -> bool {
    match cmd {
        "plan" => controls::toggle_mode(app),
        "help" => app.panel = Some(Panel::Help { scroll: 0 }),
        "todos" => app.todos_expanded = !app.todos_expanded,
        "tasks" => controls::open_tasks(app),
        "skills" => app.session.send(SessionCommand::ShowSkills),
        "compact" => {
            app.start("压缩上下文中");
            app.session.send(SessionCommand::Compact);
        }
        _ => {
            app.hint = Some(format!("未知命令：/{cmd}"));
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        let (session, _) = SessionHandle::test_channel();
        App::new(session)
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
        app.follow = false;
        app.scroll = 7;
        handle_ui_event(&mut app, UiEvent::Text("new content".into()));
        assert!(!app.follow);
        assert_eq!(app.scroll, 7);
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
        let bottom = app.scroll;
        assert!(bottom > 10);
        handle_key(&mut app, KeyEvent::new(KeyCode::PageUp, KeyModifiers::NONE));
        assert_eq!(app.scroll, bottom - 10);
        handle_ui_event(&mut app, UiEvent::Note("background finished".into()));
        let screen = render(&mut app, 80, 24);
        assert_eq!(app.scroll, bottom - 10);
        assert!(screen.contains("有新内容"));
        for _ in 0..20 {
            handle_key(
                &mut app,
                KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
            );
        }
        assert!(app.follow);
        assert!(!app.unread);
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
        let mut app = App::new(session);
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
        assert!(app.follow);
    }

    #[test]
    fn cancel_keeps_draft_and_sends_command_instead_of_quitting() {
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = App::new(session);
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
        let EntryKind::Tool(first) = &app.entries[0] else {
            panic!("missing first tool")
        };
        assert!(first.state == ToolState::Failed);
        let EntryKind::Tool(second) = &app.entries[1] else {
            panic!("missing second tool")
        };
        assert!(second.state == ToolState::Running);
        let compact = render(&mut app, 100, 40);
        assert!(compact.contains("失败 · 1.2s"));
        assert!(compact.contains("Ctrl+O 展开"));
        assert!(!compact.contains("result line 8"));
        app.follow = false;
        app.scroll = 2;
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
        assert!(!app.detailed);
        assert!(!app.follow);
        assert_eq!(app.scroll, 2);
        assert!(!app.quit);
    }

    #[test]
    fn startup_metadata_and_resizing_show_real_state() {
        let mut app = app();
        app.model = "local-model".into();
        app.directory = "/workspace/kb-agent".into();
        handle_ui_event(&mut app, UiEvent::PlanMode(true));
        handle_ui_event(&mut app, UiEvent::BackgroundCount(2));
        let screen = render(&mut app, 100, 28);
        assert!(screen.contains("Airplane"));
        assert!(screen.contains("local-model"));
        assert!(screen.contains("/workspace/kb-agent"));
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
        assert!(render(&mut app, 100, 28).contains("Plan · 后台 0"));
    }
    #[test]
    fn layout_fixture() {
        let mut app = app();
        app.model = "local-model".into();
        app.directory = "/workspace/kb-agent".into();
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
        assert!(screen.contains("Normal · 后台 1 · local-model"));
    }
    #[test]
    fn command_menu_filters_completes_and_does_not_send_on_tab() {
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = App::new(session);
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
        app.input = input::editor("/does-not-exist");
        app.busy = false;
        submit(&mut app);
        assert!(app.hint.as_deref().unwrap().contains("未知命令"));
        assert_eq!(app.input.lines(), ["/does-not-exist"]);
    }

    #[test]
    fn paste_and_multiline_shortcuts_never_submit() {
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = App::new(session);
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
        let mut app = App::new(session);
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
        app.input = input::editor("first\nsecond");
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
        let mut app = App::new(session);
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
    fn todo_panel_highlights_active_and_collapses_without_losing_data() {
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
        assert!(screen.contains("Todo 1/2"));
        assert!(screen.contains("◐ 当前正在处理"));
        handle_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        );
        let collapsed = render(&mut app, 90, 24);
        assert!(!collapsed.contains("当前正在处理"));
        assert!(collapsed.contains("Todo 1/2"));
        assert_eq!(panels::todos(&app).len(), 2);
    }

    #[test]
    fn task_output_stop_and_panels_preserve_drafts_and_render_narrow() {
        use crate::agent::event::TaskState;
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = App::new(session);
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
        app.input = new_input();
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
