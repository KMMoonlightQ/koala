//! Mouse coordinates are translated using the last rendered viewport. Output
//! selections use document rows so scrolling never changes what is copied.
use super::*;
use crossterm::event::{MouseButton, MouseEvent};
use ratatui::{Frame, layout::Rect, text::Line};
use tui_textarea::CursorMove;
use unicode_segmentation::UnicodeSegmentation;

type Position = (usize, usize); // document row, display column

#[derive(Clone, Copy)]
enum Target {
    Input,
    Output,
}

struct Selection {
    anchor: Position,
    head: Position,
    text: String,
}

impl Selection {
    fn bounds(&self) -> (Position, Position) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }
}

#[derive(Default)]
pub(super) struct State {
    frame: Rect,
    context: Option<(Lang, bool)>,
    output_area: Rect,
    input_area: Rect,
    input_top: Position,
    output: Option<Selection>,
    dragging: Option<Target>,
    pointer: (u16, u16),
    extend_after_scroll: bool,
}

fn blocked(app: &App) -> bool {
    app.panel.is_some() || app.permission.is_some() || app.extension_ui.active()
}

pub(super) fn begin_frame(app: &mut App, frame: Rect) {
    let context = (app.lang, app.transcript.detailed());
    if app.mouse.frame != frame || app.mouse.context != Some(context) || blocked(app) {
        app.mouse.output = None;
        app.mouse.dragging = None;
    }
    app.mouse.frame = frame;
    app.mouse.context = Some(context);
    app.mouse.input_area = Rect::default();
    app.mouse.output_area = Rect::default();
}

pub(super) fn record_input(app: &mut App, area: Rect) {
    if blocked(app) || area.is_empty() {
        return;
    }
    app.mouse.input_area = area;
    // TextArea keeps its viewport private. InViewport on a clone reads the
    // actual viewport without changing the editor, cursor, selection or undo.
    let mut probe = app.input.clone();
    probe.cancel_selection();
    probe.move_cursor(CursorMove::Jump(0, 0));
    probe.move_cursor(CursorMove::InViewport);
    let top = probe.cursor().0;
    probe.move_cursor(CursorMove::Jump(app.input.cursor().0 as u16, 0));
    probe.move_cursor(CursorMove::InViewport);
    app.mouse.input_top = (top, probe.cursor().1);
}

fn line_text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

// Return a grapheme boundary in scalar characters and display columns. A click
// on either cell of a wide glyph belongs to the same character.
fn column(text: &str, display: usize) -> (usize, usize) {
    let (mut chars, mut cells) = (0, 0);
    for grapheme in text.graphemes(true) {
        let width = Line::from(grapheme).width();
        if cells + width > display {
            break;
        }
        chars += grapheme.chars().count();
        cells += width;
    }
    (chars, cells)
}

fn output_position(app: &App, (x, y): (u16, u16)) -> Option<Position> {
    let area = app.mouse.output_area;
    let lines = app.transcript.rendered_lines();
    if area.is_empty() || lines.is_empty() {
        return None;
    }
    let row = (app.transcript.scroll_offset()
        + y.clamp(area.y, area.bottom() - 1).saturating_sub(area.y) as usize)
        .min(lines.len() - 1);
    let col = x.clamp(area.x, area.right()).saturating_sub(area.x) as usize;
    Some((row, column(&line_text(&lines[row]), col).1))
}

fn move_input(app: &mut App, (x, y): (u16, u16)) {
    let area = app.mouse.input_area;
    if area.is_empty() {
        return;
    }
    let row = (app.mouse.input_top.0
        + y.clamp(area.y, area.bottom() - 1).saturating_sub(area.y) as usize)
        .min(app.input.lines().len() - 1);
    let display =
        app.mouse.input_top.1 + x.clamp(area.x, area.right()).saturating_sub(area.x) as usize;
    let col = column(&app.input.lines()[row], display).0;
    app.input
        .move_cursor(CursorMove::Jump(row as u16, col as u16));
}

fn output_text(lines: &[Line<'_>], selection: &Selection) -> String {
    let (start, end) = selection.bounds();
    let mut result = Vec::new();
    for row in start.0..=end.0 {
        let Some(line) = lines.get(row) else {
            return String::new();
        };
        let text = line_text(line);
        let left = if row == start.0 { start.1 } else { 0 };
        let right = if row == end.0 { end.1 } else { line.width() };
        let from = column(&text, left).0;
        let to = column(&text, right).0;
        let part: String = text
            .chars()
            .skip(from)
            .take(to.saturating_sub(from))
            .collect();
        result.push(part);
    }
    result.join("\n")
}

fn extend_output(app: &mut App, pointer: (u16, u16)) {
    let Some(head) = output_position(app, pointer) else {
        return;
    };
    if let Some(selection) = &mut app.mouse.output {
        selection.head = head;
        selection.text = output_text(app.transcript.rendered_lines(), selection);
    }
}

pub(super) fn draw_output(f: &mut Frame, app: &mut App, area: Rect) {
    if blocked(app) {
        return;
    }
    app.mouse.output_area = area;
    // Streaming can reflow Markdown or replace tool summaries. Never leave a
    // selection highlighting different text after such a change.
    if app.mouse.output.as_ref().is_some_and(|selection| {
        output_text(app.transcript.rendered_lines(), selection) != selection.text
    }) {
        app.mouse.output = None;
        app.mouse.dragging = None;
    }
    if std::mem::take(&mut app.mouse.extend_after_scroll)
        && matches!(app.mouse.dragging, Some(Target::Output))
    {
        extend_output(app, app.mouse.pointer);
    }
    let Some(selection) = &app.mouse.output else {
        return;
    };
    let (start, end) = selection.bounds();
    for y in area.top()..area.bottom() {
        let row = app.transcript.scroll_offset() + (y - area.y) as usize;
        if row < start.0 || row > end.0 {
            continue;
        }
        let left = if row == start.0 { start.1 } else { 0 };
        let right = if row == end.0 {
            end.1
        } else {
            area.width as usize
        };
        for x in left.min(area.width as usize)..right.min(area.width as usize) {
            f.buffer_mut()[(area.x + x as u16, y)].set_style(theme::selection());
        }
    }
}

pub(super) fn handle(app: &mut App, event: MouseEvent) {
    if let Some(side) = &mut app.btw {
        handle(&mut side.app, event);
        return;
    }
    let pointer = (event.column, event.row);
    if let Some(code) = match event.kind {
        MouseEventKind::ScrollUp => Some(KeyCode::PageUp),
        MouseEventKind::ScrollDown => Some(KeyCode::PageDown),
        _ => None,
    } {
        // Paging preserves permission decisions and never recalls input history.
        super::handle_key(app, KeyEvent::new(code, KeyModifiers::NONE));
        app.mouse.extend_after_scroll = true;
        return;
    }
    if blocked(app) {
        return;
    }
    match event.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            app.mouse.output = None;
            app.mouse.dragging = None;
            app.input.cancel_selection();
            if app.mouse.input_area.contains(pointer.into()) {
                move_input(app, pointer);
                app.input.start_selection();
                app.mouse.dragging = Some(Target::Input);
            } else if app.mouse.output_area.contains(pointer.into()) {
                if let Some(anchor) = output_position(app, pointer) {
                    app.transcript.scroll(Scroll::Up(0));
                    app.mouse.output = Some(Selection {
                        anchor,
                        head: anchor,
                        text: String::new(),
                    });
                    app.mouse.dragging = Some(Target::Output);
                }
            }
            app.mouse.pointer = pointer;
        }
        MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) => {
            app.mouse.pointer = pointer;
            match app.mouse.dragging {
                Some(Target::Input) => {
                    if matches!(event.kind, MouseEventKind::Drag(_)) {
                        let area = app.mouse.input_area;
                        if !area.is_empty() {
                            let rows = if event.row < area.y {
                                -1
                            } else if event.row >= area.bottom() {
                                1
                            } else {
                                0
                            };
                            let cols = if event.column < area.x {
                                -1
                            } else if event.column >= area.right() {
                                1
                            } else {
                                0
                            };
                            if rows != 0 || cols != 0 {
                                app.input.scroll((rows, cols));
                                record_input(app, area);
                            }
                        }
                    }
                    move_input(app, pointer);
                }
                Some(Target::Output) => {
                    if matches!(event.kind, MouseEventKind::Drag(_)) {
                        let area = app.mouse.output_area;
                        if event.row < area.y {
                            app.transcript.scroll(Scroll::Up(1));
                        } else if event.row >= area.bottom() {
                            app.transcript.scroll(Scroll::Down(1));
                        }
                    }
                    extend_output(app, pointer);
                }
                None => {}
            }
            if matches!(event.kind, MouseEventKind::Up(_)) {
                app.mouse.dragging = None;
                if app.input.selection_range().is_some_and(|(a, b)| a == b) {
                    app.input.cancel_selection();
                }
            }
        }
        _ => {}
    }
}

pub(super) fn selected_text(app: &App) -> Option<String> {
    if blocked(app) {
        return None;
    }
    if let Some(selection) = &app.mouse.output {
        return (!selection.text.is_empty()).then(|| selection.text.clone());
    }
    if app.transcript.detailed() {
        return None;
    }
    let (start, end) = app.input.selection_range()?;
    if start == end {
        return None;
    }
    let mut parts = Vec::new();
    for row in start.0..=end.0 {
        let line = &app.input.lines()[row];
        let left = if row == start.0 { start.1 } else { 0 };
        let right = if row == end.0 {
            end.1
        } else {
            line.chars().count()
        };
        parts.push(
            line.chars()
                .skip(left)
                .take(right - left)
                .collect::<String>(),
        );
    }
    Some(parts.join("\n"))
}

pub(super) fn autoscrolling(app: &App) -> bool {
    if let Some(side) = &app.btw {
        return autoscrolling(&side.app);
    }
    if blocked(app) {
        return false;
    }
    let area = match app.mouse.dragging {
        Some(Target::Input) => app.mouse.input_area,
        Some(Target::Output) => app.mouse.output_area,
        None => return false,
    };
    let (x, y) = app.mouse.pointer;
    !area.is_empty()
        && (y < area.y
            || y >= area.bottom()
            || matches!(app.mouse.dragging, Some(Target::Input))
                && (x < area.x || x >= area.right()))
}

pub(super) fn tick(app: &mut App) {
    if let Some(side) = &mut app.btw {
        tick(&mut side.app);
        return;
    }
    if autoscrolling(app) {
        let (column, row) = app.mouse.pointer;
        handle(
            app,
            MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column,
                row,
                modifiers: KeyModifiers::NONE,
            },
        );
    }
}

pub(super) fn handle_key(app: &mut App, key: KeyEvent) -> bool {
    if let Some(side) = &mut app.btw {
        return handle_key(&mut side.app, key);
    }
    let copy = matches!(key.code, KeyCode::Char('c' | 'C'))
        && key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
    if copy {
        if let Some(text) = selected_text(app) {
            clipboard::copy(app, text);
            return true;
        }
    }
    if key.code == KeyCode::Esc
        && !blocked(app)
        && (app.mouse.output.is_some() || !app.transcript.detailed() && app.input.is_selecting())
    {
        app.mouse.output = None;
        app.mouse.dragging = None;
        app.input.cancel_selection();
        return true;
    }
    let scrolling = matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
        || app.transcript.detailed()
            && matches!(
                key.code,
                KeyCode::Up | KeyCode::Down | KeyCode::Home | KeyCode::End
            )
        || key.code == KeyCode::End && key.modifiers.contains(KeyModifiers::CONTROL);
    if !scrolling {
        app.mouse.output = None;
        app.mouse.dragging = None;
    }
    false
}
