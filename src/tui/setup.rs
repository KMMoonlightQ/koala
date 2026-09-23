use crate::{config::Config, i18n::Lang};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
    style::{Color, Style},
    widgets::{Block, Borders, Paragraph, Wrap},
};
use tui_textarea::TextArea;

const COMMON_PROVIDERS: &[&str] = &[
    "openai_compatible",
    "openai",
    "anthropic",
    "gemini",
    "deepseek",
    "open_router",
    "ollama",
];

#[derive(Debug, PartialEq)]
pub(super) enum Action {
    Continue,
    Save,
    Cancel,
}
pub(super) struct Form {
    fields: [TextArea<'static>; 4],
    initial: [String; 4],
    locked: [bool; 4],
    selected: usize,
    lang: Lang,
    error: Option<String>,
}
impl Form {
    pub(super) fn new(cfg: &Config, locked: [bool; 4]) -> Self {
        let mut fields = [
            &cfg.llm.provider,
            &cfg.llm.base_url,
            &cfg.llm.api_key,
            &cfg.llm.model,
        ]
        .map(|s| TextArea::new(vec![s.clone()]));
        fields[2].set_mask_char('*');
        Self {
            fields,
            initial: [
                cfg.llm.provider.clone(),
                cfg.llm.base_url.clone(),
                cfg.llm.api_key.clone(),
                cfg.llm.model.clone(),
            ],
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
        let error = if !crate::setup::valid_provider(&values[0]) {
            Some(self.text("Unsupported provider.", "不支持该模型服务商。"))
        } else if (values[0] == "openai_compatible" || !values[1].is_empty())
            && !crate::setup::valid_base_url(&values[1])
        {
            Some(self.text(
                "Enter a valid http:// or https:// base_url.",
                "请输入有效的 http:// 或 https:// base_url。",
            ))
        } else if !crate::setup::valid_model(&values[3]) {
            Some(self.text(
                "Enter a model name without whitespace.",
                "请输入不含空白字符的模型名称。",
            ))
        } else if values[2] == "sk-xxx" {
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
    pub(super) fn values(&self) -> [String; 4] {
        std::array::from_fn(|i| self.fields[i].lines()[0].trim().to_owned())
    }
    pub(super) fn handle(&mut self, event: Event) -> Action {
        let previous_provider = self.fields[0].lines()[0].clone();
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
                    KeyCode::Tab | KeyCode::Down => self.selected = (self.selected + 1) % 4,
                    KeyCode::BackTab | KeyCode::Up => self.selected = (self.selected + 3) % 4,
                    KeyCode::Enter if self.selected == 3 => return self.submit(),
                    KeyCode::Enter => self.selected += 1,
                    KeyCode::Left | KeyCode::Right if self.selected == 0 && !self.locked[0] => {
                        let current = self.fields[0].lines()[0].as_str();
                        let index = COMMON_PROVIDERS
                            .iter()
                            .position(|provider| *provider == current);
                        let next = match key.code {
                            KeyCode::Right => {
                                index.map(|i| (i + 1) % COMMON_PROVIDERS.len()).unwrap_or(0)
                            }
                            _ => index
                                .map(|i| (i + COMMON_PROVIDERS.len() - 1) % COMMON_PROVIDERS.len())
                                .unwrap_or(COMMON_PROVIDERS.len() - 1),
                        };
                        self.fields[0] = TextArea::new(vec![COMMON_PROVIDERS[next].into()]);
                    }
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
        if self.fields[0].lines()[0] != previous_provider {
            // Credentials and model IDs belong to the previous provider. Avoid
            // silently sending its key to the newly chosen provider.
            for index in 1..4 {
                if !self.locked[index] && self.fields[index].lines()[0] == self.initial[index] {
                    let replacement =
                        if index == 1 && self.fields[0].lines()[0] == "openai_compatible" {
                            "https://api.openai.com/v1"
                        } else {
                            ""
                        };
                    self.fields[index] = TextArea::new(vec![replacement.into()]);
                    if index == 2 {
                        self.fields[index].set_mask_char('*');
                    }
                }
            }
        }
        Action::Continue
    }
    pub(super) fn draw(&mut self, frame: &mut Frame) {
        let areas = Layout::vertical([
            Constraint::Length(5),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(2),
        ])
        .margin(1)
        .split(frame.area());
        frame.render_widget(Paragraph::new(self.text(
            "Koala /login — provider setup\nProvider: openai_compatible, openai, anthropic, gemini, deepseek, open_router, ollama... Native providers may leave base_url empty.",
            "Koala /login — 模型服务配置\nProvider 示例：openai_compatible、openai、anthropic、gemini、deepseek、open_router、ollama。原生服务可留空 base_url。",
        )).wrap(Wrap { trim: false }), areas[0]);
        for (i, label) in ["provider", "base_url", "api_key", "model"]
            .iter()
            .enumerate()
        {
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
        let help = self.text("Tab / ↑↓: select · ←→: provider · Enter: next/save · Ctrl-S: save · Esc: exit\nFields marked env are controlled by environment variables.",
            "Tab / ↑↓ 切换 · ←→ 选择服务商 · Enter 下一项/保存 · Ctrl-S 保存 · Esc 退出\n标记 env 的字段由环境变量控制。" );
        let message = match &self.error {
            Some(e) => format!("{help}\n{e}"),
            None => help.to_owned(),
        };
        frame.render_widget(Paragraph::new(message).wrap(Wrap { trim: false }), areas[5]);
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
    let locked = [
        "KOALA_PROVIDER",
        "KOALA_BASE_URL",
        "KOALA_API_KEY",
        "KOALA_MODEL",
    ]
    .map(|key| std::env::var(key).is_ok());
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
                    match crate::setup::save_login_fields(&path, updates) {
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
        let mut form = Form::new(&cfg, [false; 4]);
        form.handle(key(KeyCode::Tab));
        form.handle(key(KeyCode::Tab));
        form.handle(key(KeyCode::Tab));
        form.handle(Event::Paste("local-model\r\n".into()));
        assert_eq!(form.handle(key(KeyCode::Enter)), Action::Save);
        assert_eq!(form.fields[3].lines(), &["local-model"]);
        assert_eq!(form.handle(key(KeyCode::Esc)), Action::Cancel);
    }
    #[test]
    fn rejects_invalid_values_and_protects_environment_fields() {
        let mut cfg = Config::default();
        cfg.llm.base_url = "invalid".into();
        let mut form = Form::new(&cfg, [false, true, false, false]);
        form.handle(Event::Paste("secret".into()));
        assert_eq!(form.fields[1].lines(), &["invalid"]);
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
        let mut form = Form::new(&cfg, [false; 4]);
        assert_eq!(form.submit(), Action::Continue);
        assert!(form.error.is_some());
    }

    #[test]
    fn rendered_form_masks_key() {
        let mut cfg = Config::default();
        cfg.llm.api_key = "secret-never-render".into();
        let mut form = Form::new(&cfg, [false; 4]);
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

    #[test]
    fn switching_provider_clears_previous_credentials_and_model() {
        let mut cfg = Config::default();
        cfg.llm.api_key = "old-provider-secret".into();
        cfg.llm.model = "old-model".into();
        let mut form = Form::new(&cfg, [false; 4]);
        form.handle(key(KeyCode::Right));
        form.handle(key(KeyCode::Right));
        let values = form.values();
        assert_eq!(values[0], "anthropic");
        assert!(values[1].is_empty());
        assert!(values[2].is_empty());
        assert!(values[3].is_empty());
    }
}
