use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum MarkdownError {
    #[error("invalid frontmatter YAML: {0}")]
    Yaml(#[from] serde_yml::Error),
}

/// Only `name` and `description` are promised; every other field round-trips via `extra`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FrontMatter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(flatten)]
    pub extra: serde_yml::Mapping,
}

#[derive(Debug, Clone)]
pub struct ParsedMarkdown {
    pub frontmatter: Option<FrontMatter>,
    pub body: String,
    /// 1-based line in the original text where the body starts (after the closing `---`).
    pub body_start_line: usize,
}

impl ParsedMarkdown {
    pub fn plain(text: &str) -> Self {
        Self {
            frontmatter: None,
            body: text.to_string(),
            body_start_line: 1,
        }
    }
}

pub fn parse(text: &str) -> Result<ParsedMarkdown, MarkdownError> {
    let mut lines = text.lines();
    match lines.next() {
        None => return Ok(ParsedMarkdown::plain(text)),
        Some(first) if first.trim_end() == "---" => {}
        Some(_) => return Ok(ParsedMarkdown::plain(text)),
    }
    let rest: Vec<&str> = text.lines().skip(1).collect();
    for (i, line) in rest.iter().enumerate() {
        if line.trim_end() == "---" {
            let yaml = rest[..i].join("\n");
            let frontmatter: FrontMatter = serde_yml::from_str(&yaml)?;
            let mut body_lines = &rest[i + 1..];
            let mut body_start_line = i + 3;
            if body_lines.first().is_some_and(|l| l.trim().is_empty()) {
                body_lines = &body_lines[1..];
                body_start_line += 1;
            }
            return Ok(ParsedMarkdown {
                frontmatter: Some(frontmatter),
                body: body_lines.join("\n"),
                body_start_line,
            });
        }
    }
    Ok(ParsedMarkdown::plain(text))
}

pub fn render(frontmatter: &FrontMatter, body: &str) -> Result<String, MarkdownError> {
    let yaml = serde_yml::to_string(frontmatter)?;
    let yaml = yaml.strip_prefix("---\n").unwrap_or(&yaml);
    Ok(format!(
        "---\n{}\n---\n\n{}",
        yaml.trim_end_matches('\n'),
        body
    ))
}

/// A `[[path]]`, `[[path|label]]`, `[[path#L9]]` or `[[path#L9-L10]]` reference.
/// `target` is the literal workspace-relative path; no same-name resolution.
#[derive(Debug, Clone, PartialEq)]
pub struct WikiLink {
    pub target: String,
    pub label: Option<String>,
    pub line: Option<usize>,
    pub end_line: Option<usize>,
}

pub fn extract_wikilinks(text: &str) -> Vec<WikiLink> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("[[") {
        let after = &rest[open + 2..];
        let Some(close) = after.find("]]") else { break };
        let inner = &after[..close];
        rest = &after[close + 2..];
        if inner.is_empty() || inner.contains("[[") || inner.contains('\n') {
            continue;
        }
        out.push(parse_link(inner));
    }
    out
}

fn parse_link(inner: &str) -> WikiLink {
    let (path_part, label) = match inner.split_once('|') {
        Some((p, l)) => (p, Some(l.trim().to_string())),
        None => (inner, None),
    };
    let (target, spec) = match path_part.split_once('#') {
        Some((p, s)) => (p, Some(s)),
        None => (path_part, None),
    };
    let (line, end_line) = parse_line_spec(spec);
    WikiLink {
        target: target.trim().to_string(),
        label,
        line,
        end_line,
    }
}

fn parse_line_spec(spec: Option<&str>) -> (Option<usize>, Option<usize>) {
    let Some(spec) = spec else {
        return (None, None);
    };
    let mut parts = spec.splitn(2, '-');
    let start = parse_ln(parts.next());
    let end = parse_ln(parts.next());
    (start, end)
}

fn parse_ln(part: Option<&str>) -> Option<usize> {
    part?.strip_prefix('L')?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter_with_extra_fields() {
        let text = "---\nname: rust-notes\ndescription: Rust 学习笔记\ntags: [rust, systems]\ncount: 3\n---\n\nbody line\n";
        let parsed = parse(text).unwrap();
        let fm = parsed.frontmatter.unwrap();
        assert_eq!(fm.name.as_deref(), Some("rust-notes"));
        assert_eq!(fm.description.as_deref(), Some("Rust 学习笔记"));
        assert_eq!(
            fm.extra.get("tags").unwrap().as_sequence().unwrap().len(),
            2
        );
        assert_eq!(fm.extra.get("count").unwrap().as_i64(), Some(3));
        assert_eq!(parsed.body, "body line");
        assert_eq!(parsed.body_start_line, 8);
    }

    #[test]
    fn text_without_frontmatter_is_plain_body() {
        let parsed = parse("# Title\n\nno frontmatter\n").unwrap();
        assert!(parsed.frontmatter.is_none());
        assert_eq!(parsed.body_start_line, 1);
    }

    #[test]
    fn unclosed_frontmatter_is_plain_body() {
        let parsed = parse("---\nname: x\nno closing fence\n").unwrap();
        assert!(parsed.frontmatter.is_none());
    }

    #[test]
    fn frontmatter_roundtrip_preserves_extra_fields() {
        let original = "---\nname: card\ndescription: a card\ntags:\n  - rust\npinned: true\n---\n\nhello body\n";
        let parsed = parse(original).unwrap();
        let fm = parsed.frontmatter.unwrap();
        let rendered = render(&fm, &parsed.body).unwrap();
        let reparsed = parse(&rendered).unwrap();
        let fm2 = reparsed.frontmatter.unwrap();
        assert_eq!(fm, fm2);
        assert_eq!(reparsed.body, parsed.body);
    }

    #[test]
    fn extracts_all_wikilink_forms() {
        let text = "see [[daily/2026-09-17/rust.md]] and [[digest/wiki/borrow.md|借用检查]] plus [[digest/wiki/iter.md#L9]] and [[digest/wiki/iter.md#L9-L10]]";
        let links = extract_wikilinks(text);
        assert_eq!(links.len(), 4);
        assert_eq!(links[0].target, "daily/2026-09-17/rust.md");
        assert_eq!(links[0].label, None);
        assert_eq!(links[1].target, "digest/wiki/borrow.md");
        assert_eq!(links[1].label.as_deref(), Some("借用检查"));
        assert_eq!(links[2].target, "digest/wiki/iter.md");
        assert_eq!(links[2].line, Some(9));
        assert_eq!(links[2].end_line, None);
        assert_eq!(links[3].target, "digest/wiki/iter.md");
        assert_eq!(links[3].line, Some(9));
        assert_eq!(links[3].end_line, Some(10));
    }

    #[test]
    fn skips_malformed_wikilinks() {
        assert!(extract_wikilinks("[[unclosed and [[]] empty").is_empty());
        assert!(extract_wikilinks("[[]]").is_empty());
        assert!(extract_wikilinks("[[multi\nline]]").is_empty());
    }

    #[test]
    fn no_links_in_plain_text() {
        assert!(extract_wikilinks("just [single] brackets").is_empty());
    }
}
