//! Terminal Markdown, separate from the knowledge-base frontmatter parser.
use super::{text, theme};
use crate::i18n::{self, Key, Lang};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

pub fn render(source: &str, width: usize, lang: Lang) -> Vec<Line<'static>> {
    let source = text::clean(source);
    let mut view = Renderer {
        width: width.max(1),
        lang,
        lines: Vec::new(),
        spans: Vec::new(),
        styles: vec![Style::default()],
        lists: Vec::new(),
        marker: None,
        quotes: 0,
        code: None,
        links: Vec::new(),
        table: None,
        row: Vec::new(),
        cell: String::new(),
    };
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    for event in Parser::new_ext(&source, options) {
        view.event(event);
    }
    view.flush();
    while view.lines.last().is_some_and(|l| l.spans.is_empty()) {
        view.lines.pop();
    }
    view.lines
}

struct Renderer {
    width: usize,
    lang: Lang,
    lines: Vec<Line<'static>>,
    spans: Vec<Span<'static>>,
    styles: Vec<Style>,
    lists: Vec<Option<u64>>,
    marker: Option<String>,
    quotes: usize,
    code: Option<(String, String)>,
    links: Vec<String>,
    table: Option<Vec<Vec<String>>>,
    row: Vec<String>,
    cell: String,
}

impl Renderer {
    fn style(&self) -> Style {
        *self.styles.last().unwrap()
    }
    fn styled(&mut self, text: impl Into<String>, style: Style) {
        self.spans.push(Span::styled(text.into(), style));
    }
    fn push_style(&mut self, style: Style) {
        self.styles.push(self.style().patch(style));
    }
    fn blank(&mut self) {
        self.flush();
        if self.lines.last().is_some_and(|l| !l.spans.is_empty()) {
            self.lines.push(Line::default());
        }
    }
    fn flush(&mut self) {
        if self.spans.is_empty() {
            return;
        }
        let indent = format!(
            "{}{}",
            "│ ".repeat(self.quotes),
            "  ".repeat(self.lists.len().saturating_sub(1))
        );
        let marker = self.marker.take().unwrap_or_else(|| {
            if self.lists.is_empty() {
                String::new()
            } else {
                "  ".into()
            }
        });
        let first = format!("{indent}{marker}");
        let rest = format!("{indent}{}", " ".repeat(Span::raw(&marker).width()));
        let available = self.width.saturating_sub(Span::raw(&first).width()).max(1);
        let wrapped = text::wrap(Line::from(std::mem::take(&mut self.spans)), available);
        self.lines.extend(text::prefixed(wrapped, &first, &rest));
    }
    fn event(&mut self, event: Event<'_>) {
        if self.table.is_some() {
            match event {
                Event::Text(s) | Event::Code(s) => self.cell.push_str(&s),
                Event::SoftBreak | Event::HardBreak => self.cell.push(' '),
                Event::End(TagEnd::TableCell) => self.row.push(std::mem::take(&mut self.cell)),
                Event::End(TagEnd::TableHead | TagEnd::TableRow) => self
                    .table
                    .as_mut()
                    .unwrap()
                    .push(std::mem::take(&mut self.row)),
                Event::End(TagEnd::Table) => self.finish_table(),
                _ => {}
            }
            return;
        }
        match event {
            Event::Start(Tag::Table(_)) => {
                self.blank();
                self.table = Some(Vec::new());
            }
            Event::Start(Tag::Heading { .. }) => {
                self.blank();
                self.push_style(theme::heading());
            }
            Event::End(TagEnd::Heading(_)) => {
                self.flush();
                self.styles.pop();
                self.blank();
            }
            Event::Start(Tag::Paragraph) => self.flush(),
            Event::End(TagEnd::Paragraph) => {
                self.flush();
                if self.lists.is_empty() {
                    self.blank();
                }
            }
            Event::Start(Tag::Strong) => {
                self.push_style(Style::default().add_modifier(Modifier::BOLD))
            }
            Event::Start(Tag::Emphasis) => {
                self.push_style(Style::default().add_modifier(Modifier::ITALIC))
            }
            Event::Start(Tag::Strikethrough) => {
                self.push_style(Style::default().add_modifier(Modifier::CROSSED_OUT))
            }
            Event::End(TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough) => {
                self.styles.pop();
            }
            Event::Start(Tag::BlockQuote(_)) => {
                self.flush();
                self.quotes += 1;
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                self.flush();
                self.quotes = self.quotes.saturating_sub(1);
            }
            Event::Start(Tag::List(start)) => {
                self.flush();
                self.lists.push(start);
            }
            Event::End(TagEnd::List(_)) => {
                self.flush();
                self.lists.pop();
                if self.lists.is_empty() {
                    self.blank();
                }
            }
            Event::Start(Tag::Item) => {
                self.flush();
                self.marker = Some(match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let marker = format!("{n}. ");
                        *n += 1;
                        marker
                    }
                    _ => "• ".into(),
                });
            }
            Event::End(TagEnd::Item) => self.flush(),
            Event::Start(Tag::CodeBlock(kind)) => {
                self.blank();
                let language = match kind {
                    CodeBlockKind::Fenced(s) => {
                        s.split_whitespace().next().unwrap_or("").to_owned()
                    }
                    _ => String::new(),
                };
                self.code = Some((language, String::new()));
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((language, code)) = self.code.take() {
                    self.styled(
                        format!(
                            "╭─ {}",
                            if language.is_empty() {
                                "code"
                            } else {
                                &language
                            }
                        ),
                        theme::muted(),
                    );
                    self.flush();
                    for line in code.lines() {
                        let wrapped =
                            text::wrap(highlight(line, &language), self.width.saturating_sub(2));
                        self.lines.extend(text::prefixed(wrapped, "│ ", "│ "));
                    }
                    self.styled("╰─", theme::muted());
                    self.flush();
                    self.blank();
                }
            }
            Event::Text(s) => {
                if let Some((_, code)) = &mut self.code {
                    code.push_str(&s);
                } else {
                    self.styled(s.into_string(), self.style());
                }
            }
            Event::Code(s) => self.styled(s.into_string(), self.style().patch(theme::accent())),
            Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) => {
                self.links.push(dest_url.into_string());
                self.push_style(theme::accent().add_modifier(Modifier::UNDERLINED));
            }
            Event::End(TagEnd::Link | TagEnd::Image) => {
                self.styles.pop();
                if let Some(url) = self.links.pop() {
                    self.styled(format!(" ({url})"), theme::muted());
                }
            }
            Event::SoftBreak => self.styled(" ", self.style()),
            Event::HardBreak => self.flush(),
            Event::Rule => {
                self.blank();
                self.styled("─".repeat(self.width.min(32)), theme::muted());
                self.blank();
            }
            Event::TaskListMarker(done) => {
                self.styled(if done { "☒ " } else { "☐ " }, theme::accent())
            }
            Event::Html(s) | Event::InlineHtml(s) => self.styled(s.into_string(), theme::muted()),
            _ => {}
        }
    }
    fn finish_table(&mut self) {
        let rows = self.table.take().unwrap();
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        if columns == 0 {
            return;
        }
        let cell_width = self.width.saturating_sub((columns - 1) * 3) / columns;
        if cell_width < 6 {
            // Narrow terminals get labeled rows rather than clipped columns.
            for (index, row) in rows.iter().enumerate().skip(1) {
                self.styled(
                    i18n::fill(
                        self.lang,
                        Key::RecordTitle,
                        &[("index", &index.to_string())],
                    ),
                    theme::heading(),
                );
                self.flush();
                for (col, cell) in row.iter().enumerate() {
                    let label = rows[0].get(col).map(String::as_str).unwrap_or("");
                    self.styled(format!("{label}: {cell}"), Style::default());
                    self.flush();
                }
                self.blank();
            }
        } else {
            for (index, row) in rows.into_iter().enumerate() {
                let cells: Vec<_> = row
                    .into_iter()
                    .map(|s| text::wrap(Line::from(s), cell_width))
                    .collect();
                for line_index in 0..cells.iter().map(Vec::len).max().unwrap_or(0) {
                    let mut line = Line::default();
                    for col in 0..columns {
                        if col > 0 {
                            line.spans.push(Span::styled(" │ ", theme::muted()));
                        }
                        let cell = cells
                            .get(col)
                            .and_then(|c| c.get(line_index))
                            .cloned()
                            .unwrap_or_default();
                        let pad = cell_width.saturating_sub(cell.width());
                        line.spans.extend(cell.spans.into_iter().map(|mut s| {
                            if index == 0 {
                                s.style = theme::heading();
                            }
                            s
                        }));
                        line.spans.push(Span::raw(" ".repeat(pad)));
                    }
                    self.lines.push(line);
                }
                if index == 0 {
                    self.lines
                        .push(Line::styled("─".repeat(self.width), theme::muted()));
                }
            }
        }
        self.blank();
    }
}

/// Basic lexical coloring for common languages. Unknown fences remain literal.
fn highlight(source: &str, language: &str) -> Line<'static> {
    let known = matches!(
        language,
        "rust"
            | "rs"
            | "python"
            | "py"
            | "javascript"
            | "js"
            | "typescript"
            | "ts"
            | "json"
            | "bash"
            | "sh"
            | "shell"
    );
    if !known {
        return Line::from(source.to_owned());
    }
    let mut spans = Vec::new();
    let mut chars = source.chars().peekable();
    while let Some(c) = chars.next() {
        let mut token = c.to_string();
        let style = if c == '"' || c == '\'' || c == '`' {
            let mut escaped = false;
            for next in chars.by_ref() {
                token.push(next);
                if next == c && !escaped {
                    break;
                }
                escaped = next == '\\' && !escaped;
            }
            theme::success()
        } else if (c == '#' && matches!(language, "python" | "py" | "bash" | "sh" | "shell"))
            || (c == '/' && chars.peek() == Some(&'/'))
        {
            token.extend(chars.by_ref());
            theme::muted()
        } else if c.is_alphanumeric() || c == '_' {
            while chars
                .peek()
                .is_some_and(|c| c.is_alphanumeric() || *c == '_')
            {
                token.push(chars.next().unwrap());
            }
            if c.is_ascii_digit() {
                theme::warning()
            } else if matches!(
                token.as_str(),
                "fn" | "let"
                    | "mut"
                    | "pub"
                    | "use"
                    | "impl"
                    | "struct"
                    | "enum"
                    | "match"
                    | "if"
                    | "else"
                    | "for"
                    | "while"
                    | "return"
                    | "async"
                    | "await"
                    | "def"
                    | "class"
                    | "import"
                    | "from"
                    | "const"
                    | "function"
                    | "export"
                    | "true"
                    | "false"
                    | "null"
                    | "None"
                    | "True"
                    | "False"
                    | "in"
                    | "do"
                    | "done"
                    | "then"
                    | "fi"
            ) {
                theme::heading()
            } else {
                Style::default()
            }
        } else {
            Style::default()
        };
        spans.push(Span::styled(token, style));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn plain(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn renders_headings_lists_quotes_links_and_inline_styles() {
        let lines = render(
            "# 标题\n\n**粗体** 和 *斜体* 与 `代码`\n\n- 第一项\n  - 子项\n\n> 引用\n\n[文档](https://example.com)\n",
            60,
            Lang::Zh,
        );
        let text = plain(&lines);
        assert!(!text.contains("**"));
        assert!(!text.contains("# 标题"));
        assert!(text.contains("• 第一项"));
        assert!(text.contains("  • 子项"));
        assert!(text.contains("│ 引用"));
        assert!(text.contains("文档 (https://example.com)"));
        assert!(
            lines
                .iter()
                .flat_map(|l| &l.spans)
                .any(|s| s.content == "粗体" && s.style.add_modifier.contains(Modifier::BOLD))
        );
    }

    #[test]
    fn incomplete_streaming_code_fence_and_highlighting_are_safe() {
        let lines = render("```rust\nlet 中文 = 42;\n", 24, Lang::Zh);
        let text = plain(&lines);
        assert!(text.contains("let 中文 = 42;"));
        assert!(!text.contains("```"));
        assert!(
            lines
                .iter()
                .flat_map(|l| &l.spans)
                .any(|s| s.content == "let" && s.style == theme::heading())
        );
    }

    #[test]
    fn tables_wrap_cjk_and_fall_back_to_labeled_rows() {
        let source = "| 项目 | 状态 |\n|---|---|\n| 中文长项目名称 | 已完成 |";
        let wide = render(source, 30, Lang::Zh);
        assert!(plain(&wide).contains("已完成"));
        assert!(wide.iter().all(|l| l.width() <= 30));
        let narrow = plain(&render(source, 14, Lang::Zh));
        assert!(narrow.contains("记录 1"));
        assert!(narrow.contains("状态: 已完成"));
    }

    #[test]
    fn long_lines_preserve_quote_and_list_continuations() {
        let lines = render("> - 中文中文中文中文中文中文中文中文", 18, Lang::Zh);
        assert!(lines.iter().all(|l| l.width() <= 18));
        assert!(
            plain(&lines)
                .lines()
                .filter(|l| !l.is_empty())
                .all(|l| l.starts_with("│ "))
        );
    }

    #[test]
    fn narrow_tables_use_labelled_rows_in_the_active_language() {
        let source = "| a | b |\n| --- | --- |\n| 1 | 2 |";
        let plain = |lang: Lang| {
            // Narrow enough that the table falls back to labelled rows.
            render(source, 14, lang)
                .iter()
                .map(|l| l.to_string())
                .collect::<String>()
        };
        assert!(plain(Lang::En).contains("Record 1"));
        assert!(plain(Lang::Zh).contains("记录 1"));
    }
}
