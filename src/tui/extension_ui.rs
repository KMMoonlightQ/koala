//! Declarative component rendering and keyboard state, independent of Agent execution.
use super::{markdown, text, theme};
use crate::i18n::{self, Key, Lang};
use crate::{
    agent::extension_ui::{Snapshot, Surface, UiAction},
    extensions::*,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span},
    widgets::{Block, Clear, Paragraph},
};
use std::collections::{HashMap, HashSet};
use tui_textarea::TextArea;

type SurfaceKey = (String, SurfaceKind, String, u64);
fn surface_key(s: &Surface) -> SurfaceKey {
    (
        s.extension.clone(),
        s.kind,
        s.id.clone(),
        s.content_revision,
    )
}
#[derive(Default)]
struct Local {
    focus: usize,
    selected: HashMap<String, usize>,
    inputs: HashMap<String, TextArea<'static>>,
    confirm: bool,
    scroll: usize,
    follow_focus: bool,
}
#[derive(Default)]
pub(super) struct View {
    pub snapshot: Snapshot,
    list: bool,
    selected: usize,
    focused: Option<SurfaceKey>,
    local: HashMap<SurfaceKey, Local>,
    dismissed: HashSet<SurfaceKey>,
}
impl View {
    pub fn update(&mut self, snapshot: Snapshot) {
        let keys: HashSet<_> = snapshot.surfaces.iter().map(surface_key).collect();
        // Preserve focus on the same surface when the host publishes a new revision.
        if let Some(old) = &self.focused {
            self.focused = snapshot
                .surfaces
                .iter()
                .find(|s| s.extension == old.0 && s.kind == old.1 && s.id == old.2)
                .map(surface_key);
        }
        self.local.retain(|k, _| keys.contains(k));
        self.dismissed.retain(|k| keys.contains(k));
        for s in &snapshot.surfaces {
            self.local.entry(surface_key(s)).or_insert_with(|| {
                let mut local = Local {
                    follow_focus: true,
                    ..Default::default()
                };
                for block in &s.blocks {
                    if let UiBlock::Input { id, value, .. } = block {
                        local
                            .inputs
                            .insert(id.clone(), TextArea::from(vec![single_line(value)]));
                    }
                }
                local
            });
        }
        self.snapshot = snapshot;
    }
    pub fn open(&mut self) {
        self.list = true;
        self.focused = None;
        self.selected = 0;
    }
    pub fn close(&mut self) {
        self.list = false;
        self.focused = None;
    }
    fn active_surface(&self) -> Option<Surface> {
        self.snapshot
            .surfaces
            .iter()
            .find(|s| s.kind == SurfaceKind::Dialog && !self.dismissed.contains(&surface_key(s)))
            .or_else(|| {
                self.focused.as_ref().and_then(|key| {
                    self.snapshot
                        .surfaces
                        .iter()
                        .find(|s| surface_key(s) == *key)
                })
            })
            .cloned()
    }
    pub fn active(&self) -> bool {
        self.list || self.active_surface().is_some()
    }
    fn action(
        s: &Surface,
        kind: UiEventType,
        control_id: Option<String>,
        value: serde_json::Value,
    ) -> UiAction {
        UiAction {
            extension: s.extension.clone(),
            event: UiInputEvent {
                kind,
                event_id: String::new(),
                surface: Some(s.kind),
                surface_id: Some(s.id.clone()),
                revision: s.revision,
                control_id,
                value,
            },
        }
    }
    pub fn key(&mut self, key: KeyEvent) -> Option<UiAction> {
        if let Some(s) = self.active_surface() {
            let sk = surface_key(&s);
            if key.code == KeyCode::Esc {
                self.close();
                if s.kind == SurfaceKind::Dialog && !s.pending {
                    self.dismissed.insert(sk);
                    return Some(Self::action(
                        &s,
                        UiEventType::Cancel,
                        None,
                        serde_json::Value::Null,
                    ));
                }
                return None;
            }
            if s.pending {
                return None;
            }
            let local = self.local.get_mut(&sk).unwrap();
            if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
                local.follow_focus = false;
                local.scroll = if key.code == KeyCode::PageUp {
                    local.scroll.saturating_sub(5)
                } else {
                    local.scroll.saturating_add(5)
                };
                return None;
            }
            local.follow_focus = true;
            if matches!(s.dialog, Some(Dialog::Confirm { .. })) {
                match key.code {
                    KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                        local.confirm = !local.confirm
                    }
                    KeyCode::Enter => {
                        return Some(Self::action(
                            &s,
                            UiEventType::Confirm,
                            None,
                            serde_json::json!(local.confirm),
                        ));
                    }
                    _ => {}
                }
                return None;
            }
            let controls: Vec<_> = s.blocks.iter().filter(|b| b.id().is_some()).collect();
            if controls.is_empty() {
                local.follow_focus = false;
                match key.code {
                    KeyCode::Up => local.scroll = local.scroll.saturating_sub(1),
                    KeyCode::Down => local.scroll += 1,
                    _ => {}
                }
                return None;
            }
            local.focus = local.focus.min(controls.len() - 1);
            match key.code {
                KeyCode::Tab => {
                    local.focus = (local.focus + 1) % controls.len();
                    return None;
                }
                KeyCode::BackTab => {
                    local.focus = (local.focus + controls.len() - 1) % controls.len();
                    return None;
                }
                _ => {}
            }
            match controls[local.focus] {
                UiBlock::Button { id, .. } if key.code == KeyCode::Enter => Some(Self::action(
                    &s,
                    UiEventType::Click,
                    Some(id.clone()),
                    serde_json::Value::Null,
                )),
                UiBlock::Select { id, options, .. } => {
                    let selected = local.selected.entry(id.clone()).or_default();
                    match key.code {
                        KeyCode::Up => *selected = selected.saturating_sub(1),
                        KeyCode::Down => {
                            *selected = (*selected + 1).min(options.len().saturating_sub(1))
                        }
                        KeyCode::Enter => {
                            return options.get(*selected).map(|o| {
                                Self::action(
                                    &s,
                                    UiEventType::Select,
                                    Some(id.clone()),
                                    serde_json::json!(o.id),
                                )
                            });
                        }
                        _ => {}
                    }
                    None
                }
                UiBlock::Input { id, .. } => {
                    let input = local.inputs.get_mut(id).unwrap();
                    if key.code == KeyCode::Enter {
                        return Some(Self::action(
                            &s,
                            UiEventType::Submit,
                            Some(id.clone()),
                            serde_json::json!(input.lines()[0]),
                        ));
                    }
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(key.code, KeyCode::Char('j' | 'm'))
                    {
                        return None;
                    }
                    let before = input.clone();
                    input.input(key);
                    if input.lines().len() != 1 || input.lines()[0].len() > MAX_TEXT {
                        *input = before;
                    }
                    None
                }
                _ => None,
            }
        } else if self.list {
            let widgets: Vec<_> = self
                .snapshot
                .surfaces
                .iter()
                .filter(|s| s.kind == SurfaceKind::Widget)
                .collect();
            self.selected = self.selected.min(widgets.len().saturating_sub(1));
            match key.code {
                KeyCode::Esc => self.close(),
                KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down => {
                    self.selected = (self.selected + 1).min(widgets.len().saturating_sub(1))
                }
                KeyCode::Enter => {
                    self.focused = widgets.get(self.selected).map(|s| surface_key(s));
                    self.list = false;
                }
                _ => {}
            }
            None
        } else {
            None
        }
    }
    pub fn paste(&mut self, value: &str) {
        let Some(s) = self.active_surface().filter(|s| !s.pending) else {
            return;
        };
        let Some(local) = self.local.get_mut(&surface_key(&s)) else {
            return;
        };
        let Some(UiBlock::Input { id, .. }) = s
            .blocks
            .iter()
            .filter(|b| b.id().is_some())
            .nth(local.focus)
        else {
            return;
        };
        if let Some(input) = local.inputs.get_mut(id) {
            let value = single_line(value);
            if input.lines()[0].len() + value.len() <= MAX_TEXT {
                input.insert_str(value);
                local.follow_focus = true;
            }
        }
    }
    pub fn inline_lines(&self, placement: Placement, width: u16, lang: Lang) -> Vec<Line<'static>> {
        let mut lines = vec![];
        for s in self
            .snapshot
            .surfaces
            .iter()
            .filter(|s| s.kind == SurfaceKind::Widget && s.placement == placement)
        {
            lines.push(Line::styled(
                format!(
                    "[{} / {}]  /extensions",
                    single_line(&s.extension),
                    single_line(&s.id)
                ),
                theme::suggestion(),
            ));
            let (body, _) = body(s, self.local.get(&surface_key(s)), width as usize, lang);
            lines.extend(body);
        }
        lines
    }
    pub fn draw_inline(&self, f: &mut Frame, area: Rect, placement: Placement, lang: Lang) {
        if area.height == 0 {
            return;
        }
        f.render_widget(
            Paragraph::new(self.inline_lines(placement, area.width, lang)),
            area,
        );
    }
    pub fn draw_status(&self, f: &mut Frame, area: Rect) {
        let text = self
            .snapshot
            .statuses
            .iter()
            .map(|s| format!("{}: {}", single_line(&s.extension), single_line(&s.text)))
            .collect::<Vec<_>>()
            .join(" · ");
        f.render_widget(Paragraph::new(text).style(theme::subtle()), area);
    }
    pub fn draw_overlay(&mut self, f: &mut Frame, area: Rect, lang: Lang) {
        if !self.active() || area.width == 0 || area.height == 0 {
            return;
        }
        let active = self.active_surface();
        let title = active
            .as_ref()
            .map(|s| {
                let title = match &s.dialog {
                    Some(Dialog::Confirm { title, .. } | Dialog::Form { title, .. }) => {
                        title.as_str()
                    }
                    None => &s.id,
                };
                format!(
                    "{} · {}{}",
                    single_line(&s.extension),
                    single_line(title),
                    if s.pending { " …" } else { "" }
                )
            })
            .unwrap_or_else(|| i18n::text(lang, Key::ExtTitle).into());
        let dialog = super::view::centered(area, 88, area.height.min(22));
        let block = Block::bordered()
            .title(title)
            .title_bottom(i18n::text(lang, Key::ExtKeys))
            .border_style(theme::suggestion());
        let inner = block.inner(dialog);
        f.render_widget(Clear, dialog);
        f.render_widget(block, dialog);
        if let Some(s) = active {
            let (lines, focused_row) = body(
                &s,
                self.local.get(&surface_key(&s)),
                inner.width as usize,
                lang,
            );
            let local = self.local.get_mut(&surface_key(&s)).unwrap();
            let h = inner.height as usize;
            if local.follow_focus {
                if focused_row < local.scroll {
                    local.scroll = focused_row;
                }
                if focused_row >= local.scroll + h {
                    local.scroll = focused_row.saturating_sub(h.saturating_sub(1));
                }
            }
            local.scroll = local.scroll.min(lines.len().saturating_sub(h));
            f.render_widget(
                Paragraph::new(lines.into_iter().skip(local.scroll).collect::<Vec<_>>()),
                inner,
            );
        } else {
            let lines: Vec<_> = self
                .snapshot
                .surfaces
                .iter()
                .filter(|s| s.kind == SurfaceKind::Widget)
                .enumerate()
                .map(|(i, s)| {
                    Line::styled(
                        format!(
                            "{} {} / {}",
                            if i == self.selected { "❯" } else { " " },
                            single_line(&s.extension),
                            single_line(&s.id)
                        ),
                        if i == self.selected {
                            theme::accent()
                        } else {
                            theme::subtle()
                        },
                    )
                })
                .collect();
            if lines.is_empty() {
                f.render_widget(Paragraph::new(i18n::text(lang, Key::ExtEmpty)), inner);
            } else {
                let scroll = self
                    .selected
                    .saturating_sub((inner.height as usize).saturating_sub(1));
                f.render_widget(
                    Paragraph::new(lines.into_iter().skip(scroll).collect::<Vec<_>>()),
                    inner,
                );
            }
        }
    }
}
fn single_line(value: &str) -> String {
    text::clean(value).replace(['\n', '\r'], " ")
}
fn body(
    s: &Surface,
    local: Option<&Local>,
    width: usize,
    lang: Lang,
) -> (Vec<Line<'static>>, usize) {
    let mut lines = vec![];
    let mut focused_row = 0;
    let mut control = 0;
    if let Some(Dialog::Confirm { text: message, .. }) = &s.dialog {
        for line in text::clean(message).lines() {
            lines.extend(text::wrap(Line::raw(line.to_owned()), width));
        }
        focused_row = lines.len();
        let yes = local.is_some_and(|l| l.confirm);
        lines.push(Line::styled(
            format!(
                "{} {}    {} {}",
                if yes { "○" } else { "●" },
                i18n::text(lang, Key::ExtCancel),
                if yes { "●" } else { "○" },
                i18n::text(lang, Key::ExtConfirm)
            ),
            theme::accent(),
        ));
        return (lines, focused_row);
    }
    for block in &s.blocks {
        let focused = local.is_some_and(|l| l.focus == control) && block.id().is_some();
        if focused {
            focused_row = lines.len();
        }
        let prefix = if focused { "❯ " } else { "  " };
        let style = if focused {
            theme::accent()
        } else {
            theme::subtle()
        };
        match block {
            UiBlock::Markdown { text: value } => {
                lines.extend(markdown::render(&text::clean(value), width.max(1), lang))
            }
            UiBlock::Text { text: value } => {
                for line in text::clean(value).lines() {
                    lines.extend(text::wrap(Line::raw(line.to_owned()), width));
                }
            }
            UiBlock::Button { label, .. } => lines.extend(text::wrap(
                Line::styled(format!("{prefix}[ {} ]", single_line(label)), style),
                width,
            )),
            UiBlock::Input { id, label, value } => {
                let mut value = local
                    .and_then(|l| l.inputs.get(id))
                    .map(|i| i.lines()[0].clone())
                    .unwrap_or_else(|| single_line(value));
                let mut caret_row = 0;
                if focused {
                    let column = local
                        .and_then(|l| l.inputs.get(id))
                        .map(|i| i.cursor().1)
                        .unwrap_or(0);
                    let byte = value
                        .char_indices()
                        .nth(column)
                        .map(|(i, _)| i)
                        .unwrap_or(value.len());
                    caret_row = text::wrap(
                        Line::raw(format!(
                            "{prefix}{}: {}▏",
                            single_line(label),
                            &value[..byte]
                        )),
                        width,
                    )
                    .len()
                    .saturating_sub(1);
                    value.insert(byte, '▏');
                }
                if focused {
                    focused_row = lines.len() + caret_row;
                }
                lines.extend(text::wrap(
                    Line::styled(format!("{prefix}{}: {value}", single_line(label)), style),
                    width,
                ));
            }
            UiBlock::Select { id, label, options } => {
                lines.extend(text::wrap(
                    Line::styled(format!("{prefix}{}", single_line(label)), style),
                    width,
                ));
                let selected = local.and_then(|l| l.selected.get(id)).copied().unwrap_or(0);
                for (i, option) in options.iter().enumerate() {
                    if focused && i == selected {
                        focused_row = lines.len();
                    }
                    lines.extend(text::wrap(
                        Line::from(vec![Span::styled(
                            format!(
                                "  {} {}",
                                if i == selected { "●" } else { "○" },
                                single_line(&option.label)
                            ),
                            if focused && i == selected {
                                theme::accent()
                            } else {
                                theme::subtle()
                            },
                        )]),
                        width,
                    ));
                }
            }
        }
        if block.id().is_some() {
            control += 1;
        }
    }
    (lines, focused_row)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::extension_ui::Surface;
    fn snapshot() -> Snapshot {
        Snapshot {
            surfaces: vec![Surface {
                extension: "demo".into(),
                id: "form".into(),
                kind: SurfaceKind::Widget,
                revision: 1,
                content_revision: 1,
                pending: false,
                placement: Placement::AboveEditor,
                dialog: None,
                blocks: vec![
                    UiBlock::Input {
                        id: "name".into(),
                        label: "姓名".into(),
                        value: String::new(),
                    },
                    UiBlock::Button {
                        id: "save".into(),
                        label: "Save".into(),
                    },
                ],
            }],
            statuses: vec![],
        }
    }
    #[test]
    fn keyboard_and_paste_submit_extension_input_not_chat() {
        let mut ui = View::default();
        ui.update(snapshot());
        ui.open();
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        ui.paste("小明\n测试");
        let action = ui
            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(action.event.kind, UiEventType::Submit);
        assert_eq!(action.event.value, "小明 测试");
        assert_eq!(action.extension, "demo");
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let action = ui
            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(action.event.kind, UiEventType::Click);
    }
    #[test]
    fn callback_ack_preserves_unsent_input_and_focus() {
        let mut ui = View::default();
        ui.update(snapshot());
        ui.open();
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        ui.paste("keep this draft");
        ui.key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        let mut next = snapshot();
        next.surfaces[0].revision = 2;
        ui.update(next);
        let action = ui
            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(
            action.event.kind,
            UiEventType::Click,
            "focus lost after acknowledgement"
        );
        ui.key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::NONE));
        let action = ui
            .key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();
        assert_eq!(action.event.value, "keep this draft");
    }
    #[test]
    fn long_input_scrolling_keeps_caret_visible() {
        let mut ui = View::default();
        ui.update(snapshot());
        ui.open();
        ui.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        ui.paste(&"abc".repeat(100));
        ui.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(20, 8)).unwrap();
        terminal
            .draw(|f| ui.draw_overlay(f, f.area(), Lang::Zh))
            .unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(content.contains('▏'), "cursor scrolled out of view");
    }
    #[test]
    fn widgets_render_in_their_declared_slots_and_strip_terminal_controls() {
        let mut ui = View::default();
        let mut s = snapshot();
        s.surfaces[0].blocks = vec![UiBlock::Text {
            text: "ABOVE\x1b[31m".into(),
        }];
        let mut below = s.surfaces[0].clone();
        below.id = "below".into();
        below.content_revision = 2;
        below.placement = Placement::BelowEditor;
        below.blocks = vec![UiBlock::Text {
            text: "BELOW".into(),
        }];
        s.surfaces.push(below);
        ui.update(s);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 10)).unwrap();
        terminal
            .draw(|f| {
                ui.draw_inline(f, Rect::new(0, 0, 60, 4), Placement::AboveEditor, Lang::En);
                ui.draw_inline(f, Rect::new(0, 6, 60, 4), Placement::BelowEditor, Lang::En);
            })
            .unwrap();
        let cells = &terminal.backend().buffer().content;
        let top: String = cells[..240].iter().map(|c| c.symbol()).collect();
        let bottom: String = cells[360..].iter().map(|c| c.symbol()).collect();
        assert!(top.contains("ABOVE") && !top.contains("BELOW") && !top.contains("31m"));
        assert!(bottom.contains("BELOW") && !bottom.contains("ABOVE"));
    }
    #[test]
    fn renders_chinese_widgets_and_small_dialogs_without_panics() {
        let mut ui = View::default();
        ui.update(snapshot());
        ui.open();
        for (w, h) in [(100, 30), (20, 8), (1, 1)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            terminal
                .draw(|f| ui.draw_overlay(f, f.area(), Lang::Zh))
                .unwrap();
        }
    }
}
