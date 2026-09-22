use super::*;
use crossterm::event::{MouseButton, MouseEvent};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};

fn render(app: &mut App, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|f| draw(f, app)).unwrap();
    terminal.backend().buffer().clone()
}

fn locate(buffer: &Buffer, text: &str) -> (u16, u16) {
    for y in buffer.area.top()..buffer.area.bottom() {
        for x in buffer.area.left()..buffer.area.right() {
            let tail: String = (x..buffer.area.right())
                .map(|col| buffer[(col, y)].symbol())
                .collect();
            if tail.starts_with(text) {
                return (x, y);
            }
        }
    }
    panic!("missing {text:?} in rendered frame");
}

fn mouse(app: &mut App, kind: MouseEventKind, (column, row): (u16, u16)) {
    handle_terminal_event(
        app,
        Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }),
    );
}

fn drag(app: &mut App, from: (u16, u16), to: (u16, u16)) {
    mouse(app, MouseEventKind::Down(MouseButton::Left), from);
    mouse(app, MouseEventKind::Drag(MouseButton::Left), to);
    mouse(app, MouseEventKind::Up(MouseButton::Left), to);
}

#[test]
fn input_mouse_selection_can_be_replaced_without_sending() {
    let (session, mut commands) = SessionHandle::test_channel();
    let mut app = App::new(session);
    app.input.insert_str("abcdef");
    let frame = render(&mut app, 80, 24);
    let (x, y) = locate(&frame, "abcdef");
    drag(&mut app, (x + 1, y), (x + 4, y));
    assert_eq!(app.input.selection_range(), Some(((0, 1), (0, 4))));
    handle_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE),
    );
    assert_eq!(app.input.lines(), ["aXef"]);
    assert!(commands.try_recv().is_err());
}

#[test]
fn output_drag_highlights_only_selected_text_and_keeps_draft() {
    let (session, _) = SessionHandle::test_channel();
    let mut app = App::new(session);
    app.push(EntryKind::Assistant("abcdef".into()));
    app.input.insert_str("draft");
    let before = render(&mut app, 80, 24);
    let (x, y) = locate(&before, "abcdef");
    drag(&mut app, (x + 1, y), (x + 4, y));
    let after = render(&mut app, 80, 24);
    assert_ne!(after[(x + 1, y)].bg, before[(x + 1, y)].bg);
    assert_eq!(after[(x, y)].bg, before[(x, y)].bg);
    assert_eq!(after[(x + 4, y)].bg, before[(x + 4, y)].bg);
    assert_eq!(app.input.lines(), ["draft"]);
    assert_eq!(mouse::selected_text(&app).as_deref(), Some("bcd"));
}

#[test]
fn input_drag_respects_unicode_and_horizontal_and_vertical_viewports() {
    let (session, _) = SessionHandle::test_channel();
    let mut app = App::new(session);
    app.input.insert_str("a中文e\u{301}👩‍💻z");
    let frame = render(&mut app, 80, 24);
    let (x, y) = locate(&frame, "a中");
    drag(&mut app, (x + 2, y), (x + 8, y));
    assert_eq!(
        mouse::selected_text(&app).as_deref(),
        Some("中文e\u{301}👩‍💻")
    );
    // Reverse drag selects the same complete graphemes.
    drag(&mut app, (x + 8, y), (x + 2, y));
    assert_eq!(
        mouse::selected_text(&app).as_deref(),
        Some("中文e\u{301}👩‍💻")
    );

    app.input = input::editor("zero\none\ntwo\nthree\nfour\nfive\nsix", Lang::En);
    let frame = render(&mut app, 40, 24);
    let (x, y) = locate(&frame, "four");
    drag(&mut app, (x, y), (x + 3, y + 1));
    assert_eq!(mouse::selected_text(&app).as_deref(), Some("four\nfiv"));

    app.input = input::editor("012345678901234567890123456789abcdef", Lang::En);
    let frame = render(&mut app, 24, 20);
    let (x, y) = locate(&frame, "abcdef");
    drag(&mut app, (x + 1, y), (x + 4, y));
    assert_eq!(mouse::selected_text(&app).as_deref(), Some("bcd"));
}

#[test]
fn output_selection_survives_wheel_scrolling_and_streaming() {
    let (session, _) = SessionHandle::test_channel();
    let mut app = App::new(session);
    app.push(EntryKind::User(
        (0..40).map(|n| format!("line{n:02}\n")).collect(),
    ));
    let frame = render(&mut app, 60, 16);
    let (x, y) = locate(&frame, "line38");
    drag(&mut app, (x, y), (x + 6, y));
    let bottom = app.transcript.scroll_offset();
    mouse(&mut app, MouseEventKind::ScrollUp, (x, y));
    render(&mut app, 60, 16);
    assert!(app.transcript.scroll_offset() < bottom);
    assert_eq!(mouse::selected_text(&app).as_deref(), Some("line38"));
    app.transcript.append_text("later content".into());
    render(&mut app, 60, 16);
    assert_eq!(mouse::selected_text(&app).as_deref(), Some("line38"));
    mouse(&mut app, MouseEventKind::ScrollDown, (x, y));
    let frame = render(&mut app, 60, 16);
    let (x, y) = locate(&frame, "line38");
    assert_ne!(frame[(x, y)].bg, ratatui::style::Color::Reset);
}

#[test]
fn output_drag_can_extend_across_scrolled_rows() {
    let (session, _) = SessionHandle::test_channel();
    let mut app = App::new(session);
    app.push(EntryKind::User(
        (0..40).map(|n| format!("line{n:02}\n")).collect(),
    ));
    let frame = render(&mut app, 60, 16);
    let from = locate(&frame, "line38");
    mouse(&mut app, MouseEventKind::Down(MouseButton::Left), from);
    mouse(&mut app, MouseEventKind::ScrollUp, from);
    let frame = render(&mut app, 60, 16);
    let to = locate(&frame, "line28");
    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), to);
    mouse(&mut app, MouseEventKind::Up(MouseButton::Left), to);
    let selected = mouse::selected_text(&app).unwrap();
    assert!(selected.starts_with("line28\n"), "{selected:?}");
    assert!(selected.contains("line35"));
    assert!(!selected.contains("line38"));
}

#[test]
fn escape_clears_selection_before_cancelling_and_overlay_blocks_hidden_text() {
    let (session, mut commands) = SessionHandle::test_channel();
    let mut app = App::new(session);
    app.busy = true;
    app.input.insert_str("abcdef");
    let frame = render(&mut app, 80, 24);
    let (x, y) = locate(&frame, "abcdef");
    drag(&mut app, (x, y), (x + 3, y));
    handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(mouse::selected_text(&app).is_none());
    assert!(commands.try_recv().is_err());
    handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(matches!(commands.try_recv(), Ok(SessionCommand::Cancel)));

    app.panel = Some(Panel::Help { scroll: 0 });
    render(&mut app, 80, 24);
    drag(&mut app, (x, y), (x + 3, y));
    assert!(mouse::selected_text(&app).is_none());
}

#[test]
fn resize_and_replaced_output_clear_stale_selection() {
    let (session, _) = SessionHandle::test_channel();
    let mut app = App::new(session);
    app.push(EntryKind::Assistant("abcdef".into()));
    let frame = render(&mut app, 80, 24);
    let (x, y) = locate(&frame, "abcdef");
    drag(&mut app, (x, y), (x + 3, y));
    render(&mut app, 40, 24);
    assert!(mouse::selected_text(&app).is_none());
    let frame = render(&mut app, 40, 24);
    let (x, y) = locate(&frame, "abcdef");
    drag(&mut app, (x, y), (x + 3, y));
    app.transcript.reset();
    app.push(EntryKind::Assistant("new text".into()));
    render(&mut app, 40, 24);
    assert!(mouse::selected_text(&app).is_none());
}

#[test]
fn mouse_targets_btw_and_escape_preserves_it_until_selection_is_cleared() {
    let (session, _commands) = SessionHandle::test_channel();
    let mut app = App::new(session);
    app.input.insert_str("main draft");
    btw::open(&mut app, "");
    app.btw.as_mut().unwrap().app.input.insert_str("side draft");
    let frame = render(&mut app, 80, 24);
    let (x, y) = locate(&frame, "side draft");
    drag(&mut app, (x, y), (x + 4, y));
    assert_eq!(
        mouse::selected_text(&app.btw.as_ref().unwrap().app).as_deref(),
        Some("side")
    );
    assert!(mouse::selected_text(&app).is_none());
    handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.btw.is_some());
    handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.btw.is_none());
    assert_eq!(app.input.lines(), ["main draft"]);
}

#[test]
fn copy_shortcuts_with_selection_do_not_cancel_or_clear_the_draft() {
    for modifiers in [
        KeyModifiers::CONTROL,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        KeyModifiers::SUPER,
    ] {
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = App::new(session);
        app.busy = true;
        app.input.insert_str("draft");
        app.input.select_all();
        // A copy already in flight must consume duplicate copy shortcuts too.
        // No test writes to the user's system clipboard.
        let (_send, receive) = oneshot::channel();
        app.clipboard_copy_pending = Some(receive);
        handle_key(&mut app, KeyEvent::new(KeyCode::Char('c'), modifiers));
        assert_eq!(app.input.lines(), ["draft"]);
        assert_eq!(mouse::selected_text(&app).as_deref(), Some("draft"));
        assert!(commands.try_recv().is_err());
        clipboard::apply_copy(&mut app, Err("clipboard unavailable".into()));
        assert!(app.hint.as_ref().unwrap().contains("clipboard unavailable"));
        assert_eq!(mouse::selected_text(&app).as_deref(), Some("draft"));
    }
}

#[test]
fn output_copy_preserves_selected_spaces_and_complete_unicode() {
    let (session, _) = SessionHandle::test_channel();
    let mut app = App::new(session);
    app.push(EntryKind::User("a中文e\u{301}👩‍💻  z".into()));
    let frame = render(&mut app, 80, 24);
    let (x, y) = locate(&frame, "a中");
    drag(&mut app, (x + 2, y), (x + 10, y));
    assert_eq!(
        mouse::selected_text(&app).as_deref(),
        Some("中文e\u{301}👩‍💻  ")
    );
}

#[test]
fn tiny_frames_and_mouse_outside_content_are_safe() {
    let (session, _) = SessionHandle::test_channel();
    let mut app = App::new(session);
    app.input.insert_str("a");
    for size in 1..=4 {
        render(&mut app, size, size);
        drag(&mut app, (0, 0), (u16::MAX, u16::MAX));
        render(&mut app, size, size);
    }
    assert_eq!(app.input.lines(), ["a"]);
}

#[test]
fn dragging_above_input_scrolls_to_hidden_lines_without_losing_anchor() {
    let (session, _) = SessionHandle::test_channel();
    let mut app = App::new(session);
    app.input
        .insert_str("zero\none\ntwo\nthree\nfour\nfive\nsix");
    let frame = render(&mut app, 40, 24);
    let (x, bottom) = locate(&frame, "six");
    let (_, top) = locate(&frame, "two");
    mouse(
        &mut app,
        MouseEventKind::Down(MouseButton::Left),
        (x + 3, bottom),
    );
    mouse(
        &mut app,
        MouseEventKind::Drag(MouseButton::Left),
        (x, top - 1),
    );
    for _ in 0..3 {
        mouse::tick(&mut app);
        render(&mut app, 40, 24);
    }
    assert_eq!(
        mouse::selected_text(&app).as_deref(),
        Some("zero\none\ntwo\nthree\nfour\nfive\nsix")
    );
}

#[test]
fn keyboard_input_selection_takes_over_after_clicking_output() {
    let (session, _) = SessionHandle::test_channel();
    let mut app = App::new(session);
    app.input.insert_str("draft");
    app.push(EntryKind::Assistant("answer".into()));
    let frame = render(&mut app, 80, 24);
    let point = locate(&frame, "answer");
    drag(&mut app, point, point);
    handle_key(&mut app, KeyEvent::new(KeyCode::Home, KeyModifiers::SHIFT));
    assert_eq!(mouse::selected_text(&app).as_deref(), Some("draft"));
}

#[test]
fn hidden_input_selection_does_not_intercept_detailed_view_escape() {
    let (session, _) = SessionHandle::test_channel();
    let mut app = App::new(session);
    app.input.insert_str("draft");
    app.input.select_all();
    app.toggle_details();
    render(&mut app, 80, 24);
    assert!(mouse::selected_text(&app).is_none());
    handle_key(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.transcript.detailed());
}
