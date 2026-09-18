use ratatui::text::{Line, Span};
use unicode_segmentation::UnicodeSegmentation;

/// Treat model/tool output as text, never terminal escape sequences.
pub fn clean(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.next() {
                Some('[') => {
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\x07' || (c == '\x1b' && chars.next() == Some('\\')) {
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\t' => out.push_str("    "),
            '\n' => out.push('\n'),
            c if !c.is_control() => out.push(c),
            _ => {}
        }
    }
    out
}

/// Wrap styled text by display columns without breaking CJK or emoji graphemes.
pub fn wrap(line: Line<'static>, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut current = Line::default();
    let mut used = 0;
    for span in line.spans {
        let mut chunk = String::new();
        for g in span.content.graphemes(true) {
            let size = Span::raw(g).width();
            if used > 0 && used + size > width {
                if !chunk.is_empty() {
                    current
                        .spans
                        .push(Span::styled(std::mem::take(&mut chunk), span.style));
                }
                lines.push(std::mem::take(&mut current));
                used = 0;
            }
            chunk.push_str(g);
            used += size;
        }
        if !chunk.is_empty() {
            current.spans.push(Span::styled(chunk, span.style));
        }
    }
    if !current.spans.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}

/// Like [`prefixed`], but with styled prefix spans (e.g. a colored bullet).
pub fn prefixed_styled(
    lines: Vec<Line<'static>>,
    first: Span<'static>,
    rest: Span<'static>,
) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .enumerate()
        .map(|(i, mut line)| {
            line.spans
                .insert(0, if i == 0 { first.clone() } else { rest.clone() });
            line
        })
        .collect()
}

pub fn prefixed(lines: Vec<Line<'static>>, first: &str, rest: &str) -> Vec<Line<'static>> {
    prefixed_styled(
        lines,
        Span::raw(first.to_owned()),
        Span::raw(rest.to_owned()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_graphemes_and_strips_terminal_controls() {
        let lines = wrap(Line::from("中文👩‍💻abcd"), 4);
        assert!(lines.iter().all(|l| l.width() <= 4));
        let joined: String = lines
            .iter()
            .flat_map(|l| &l.spans)
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(joined, "中文👩‍💻abcd");
        assert_eq!(
            clean("\x1b[31mred\x1b[0m\x1b]0;title\x07\nnext"),
            "red\nnext"
        );
    }
}
