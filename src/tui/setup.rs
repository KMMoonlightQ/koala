use crate::{config::Config, i18n::Lang};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use tui_textarea::TextArea;

#[derive(Debug, PartialEq)]
enum Action {
    Continue,
    Save,
    Cancel,
}
struct Form {
    fields: [TextArea<'static>; 3],
    locked: [bool; 3],
    selected: usize,
    lang: Lang,
    error: Option<String>,
}
impl Form {
    fn new(cfg: &Config, locked: [bool; 3]) -> Self {
        let mut fields = [&cfg.llm.base_url, &cfg.llm.api_key, &cfg.llm.model]
            .map(|s| TextArea::new(vec![s.clone()]));
        fields[1].set_mask_char('*');
        Self {
            fields,
            locked,
            selected: 0,
            lang: cfg.lang,
            error: None,
        }
    }
    fn text(&self, en: &'static str, zh: &'static str) -> &'static str {
        if self.lang == Lang::Zh { zh } else { en }
    }
    fn submit(&mut self) -> Action {
        let values = self.values();
        let error = if !crate::setup::valid_base_url(&values[0]) {
            Some(self.text(
                "Enter a valid http:// or https:// base_url.",
                "请输入有效的 http:// 或 https:// base_url。",
            ))
        } else if !crate::setup::valid_model(&values[2]) {
            Some(self.text(
                "Enter a model name without whitespace.",
                "请输入不含空白字符的模型名称。",
            ))
        } else if values[1] == "sk-xxx" {
            Some(self.text(
                "Replace the example API key, or leave it empty for a keyless endpoint.",
                "请替换示例 API Key；免密服务可留空。",
            ))
        } else {
            None
        };
        self.error = error.map(str::to_owned);
        if self.error.is_some() {
            Action::Continue
        } else {
            Action::Save
        }
    }
    fn values(&self) -> [String; 3] {
        std::array::from_fn(|i| self.fields[i].lines()[0].trim().to_owned())
    }
    fn handle(&mut self, event: Event) -> Action {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                match key.code {
                    KeyCode::Esc => return Action::Cancel,
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Action::Cancel;
                    }
                    KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return self.submit();
                    }
                    KeyCode::Tab | KeyCode::Down => self.selected = (self.selected + 1) % 3,
                    KeyCode::BackTab | KeyCode::Up => self.selected = (self.selected + 2) % 3,
                    KeyCode::Enter if self.selected == 2 => return self.submit(),
                    KeyCode::Enter => self.selected += 1,
                    // Keep all fields single-line; clipboard paste is handled below.
                    KeyCode::Char(_)
                        if key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {}
                    _ if !self.locked[self.selected] => {
                        self.fields[self.selected].input(key);
                    }
                    _ => {}
                }
            }
            Event::Paste(text) if !self.locked[self.selected] => {
                let text: String = text.chars().filter(|c| !c.is_control()).collect();
                self.fields[self.selected].insert_str(text);
            }
            _ => {}
        }
        Action::Continue
    }
    fn draw(&mut self, frame: &mut Frame) {
        let areas = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(2),
        ])
        .margin(1)
        .split(frame.area());
        frame.render_widget(Paragraph::new(self.text(
            "Welcome to Koala — connection setup\nSaved to ~/.koala/config.toml. API key may be empty for keyless services.",
            "欢迎使用 Koala — 模型连接设置\n保存至 ~/.koala/config.toml。免密服务的 API Key 可留空。",
        )).wrap(Wrap { trim: false }), areas[0]);
        for (i, label) in ["base_url", "api_key", "model"].iter().enumerate() {
            let title = if self.locked[i] {
                format!("{label} (KOALA_{} · env)", label.to_uppercase())
            } else {
                (*label).into()
            };
            let style = if self.selected == i {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            };
            self.fields[i].set_block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .border_style(style),
            );
            self.fields[i].set_cursor_line_style(Style::default());
            self.fields[i].set_cursor_style(if self.selected == i && !self.locked[i] {
                Style::default().add_modifier(ratatui::style::Modifier::REVERSED)
            } else {
                Style::default()
            });
            frame.render_widget(&self.fields[i], areas[i + 1]);
        }
        let help = self.text("Tab / ↑↓: select · Enter: next/save · Ctrl-S: save · Esc: exit\nFields marked env are controlled by environment variables.",
            "Tab / ↑↓ 切换 · Enter 下一项/保存 · Ctrl-S 保存 · Esc 退出\n标记 env 的字段由环境变量控制。" );
        let message = match &self.error {
            Some(e) => format!("{help}\n{e}"),
            None => help.to_owned(),
        };
        frame.render_widget(Paragraph::new(message).wrap(Wrap { trim: false }), areas[4]);
    }
}

/// Run before constructing the agent, so no provider or MCP starts before setup.
pub(super) async fn run(cfg: &Config) -> anyhow::Result<bool> {
    use futures_util::StreamExt;
    use std::io::IsTerminal;
    anyhow::ensure!(
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        "Connection setup requires a terminal. Run koala interactively or configure ~/.koala/config.toml / KOALA_BASE_URL / KOALA_API_KEY / KOALA_MODEL."
    );
    let locked =
        ["KOALA_BASE_URL", "KOALA_API_KEY", "KOALA_MODEL"].map(|key| std::env::var(key).is_ok());
    let mut form = Form::new(cfg, locked);
    let path = crate::config::koala_dir()?.join("config.toml");
    let mut terminal = ratatui::init();
    let result = async {
        crossterm::execute!(std::io::stdout(), crossterm::event::EnableBracketedPaste)?;
        let mut events = crossterm::event::EventStream::new();
        loop {
            terminal.draw(|frame| form.draw(frame))?;
            let Some(event) = events.next().await else { return Ok(false); };
            match form.handle(event?) {
                Action::Cancel => return Ok(false),
                Action::Continue => {},
                Action::Save => {
                    let values = form.values();
                    let updates = std::array::from_fn(|i| if locked[i] { None } else { Some(values[i].as_str()) });
                    match crate::setup::save_connection(&path, updates) {
                        Ok(()) => return Ok(true),
                        // Never render parser errors: their source excerpts may contain secrets.
                        Err(_) => form.error = Some(form.text(
                            "Could not save configuration. Check file permissions and TOML syntax, then retry.",
                            "无法保存配置，请检查文件权限及 TOML 格式后重试。",
                        ).into()),
                    }
                }
            }
        }
    }.await;
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::DisableBracketedPaste);
    ratatui::restore();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    #[test]
    fn completes_form_with_keyless_endpoint_and_cancels_without_saving() {
        let mut cfg = Config::default();
        cfg.llm.base_url = "http://localhost:1234/v1".into();
        let mut form = Form::new(&cfg, [false; 3]);
        form.handle(key(KeyCode::Tab));
        form.handle(key(KeyCode::Tab));
        form.handle(Event::Paste("local-model\r\n".into()));
        assert_eq!(form.handle(key(KeyCode::Enter)), Action::Save);
        assert_eq!(form.fields[2].lines(), &["local-model"]);
        assert_eq!(form.handle(key(KeyCode::Esc)), Action::Cancel);
    }
    #[test]
    fn rejects_invalid_values_and_protects_environment_fields() {
        let mut cfg = Config::default();
        cfg.llm.base_url = "invalid".into();
        let mut form = Form::new(&cfg, [true, false, false]);
        form.handle(Event::Paste("secret".into()));
        assert_eq!(form.fields[0].lines(), &["invalid"]);
        assert_eq!(
            form.handle(Event::Key(KeyEvent::new(
                KeyCode::Char('s'),
                KeyModifiers::CONTROL
            ))),
            Action::Continue
        );
        assert!(form.error.is_some());
    }
    #[test]
    fn invalid_model_stays_in_form_instead_of_saving_unusable_config() {
        let mut cfg = Config::default();
        cfg.llm.model = "my model".into();
        let mut form = Form::new(&cfg, [false; 3]);
        assert_eq!(form.submit(), Action::Continue);
        assert!(form.error.is_some());
    }

    #[test]
    fn rendered_form_masks_key() {
        let mut cfg = Config::default();
        cfg.llm.api_key = "secret-never-render".into();
        let mut form = Form::new(&cfg, [false; 3]);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 24)).unwrap();
        terminal.draw(|f| form.draw(f)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("api_key"));
        assert!(!screen.contains("secret-never-render"));
        assert!(screen.contains("******"));
    }
}
