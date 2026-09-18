use crate::markdown;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

pub const DEFAULT_MAX_CHUNK_LINES: usize = 80;

#[derive(Debug, Clone)]
pub struct Chunk {
    pub path: String,
    /// 1-based, inclusive, relative to the whole file (frontmatter included).
    pub start_line: usize,
    pub end_line: usize,
    /// e.g. `# 一级 > ## 二级`, with ` [Part X/N]` appended for line-split chunks.
    pub breadcrumb: String,
    pub text: String,
    /// What BM25 indexes: breadcrumb + text, plus frontmatter name/description on the first chunk.
    pub search_text: String,
}

#[derive(Debug)]
struct Heading {
    line: usize,
    level: usize,
    title: String,
}

pub fn chunk_markdown(
    path: &str,
    text: &str,
    max_chunk_lines: usize,
    fm_name: Option<&str>,
    fm_description: Option<&str>,
) -> Vec<Chunk> {
    let parsed = markdown::parse(text).unwrap_or_else(|_| markdown::ParsedMarkdown::plain(text));
    let offset = parsed.body_start_line - 1;
    let lines: Vec<&str> = parsed.body.lines().collect();
    let headings = find_headings(&parsed.body);
    let n = lines.len();

    let mut out = Vec::new();
    if n > 0 {
        if headings.is_empty() {
            emit_or_split(&mut out, path, &lines, 1, n, "", max_chunk_lines, offset);
        } else {
            if headings[0].line > 1 {
                emit_or_split(
                    &mut out,
                    path,
                    &lines,
                    1,
                    headings[0].line - 1,
                    "",
                    max_chunk_lines,
                    offset,
                );
            }
            let sec_end = section_ends(&headings, n);
            let top = headings.iter().map(|h| h.level).min().unwrap();
            for i in 0..headings.len() {
                if headings[i].level == top {
                    chunk_section(
                        &mut out,
                        path,
                        &lines,
                        &headings,
                        &sec_end,
                        i,
                        "",
                        max_chunk_lines,
                        offset,
                    );
                }
            }
        }
    }

    let mut prefix = String::new();
    if let Some(name) = fm_name {
        prefix.push_str(name);
        prefix.push('\n');
    }
    if let Some(desc) = fm_description {
        prefix.push_str(desc);
        prefix.push('\n');
    }
    if !prefix.is_empty() {
        match out.first_mut() {
            Some(first) => first.search_text = format!("{prefix}{}", first.search_text),
            None => out.push(Chunk {
                path: path.to_string(),
                start_line: 1,
                end_line: 1,
                breadcrumb: String::new(),
                text: String::new(),
                search_text: prefix.trim_end().to_string(),
            }),
        }
    }
    out
}

fn find_headings(body: &str) -> Vec<Heading> {
    let line_starts = line_starts(body);
    let mut headings = Vec::new();
    let mut current: Option<(usize, usize, String)> = None;
    for (event, range) in Parser::new_ext(body, Options::empty()).into_offset_iter() {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                current = Some((
                    offset_to_line(&line_starts, range.start),
                    level as usize,
                    String::new(),
                ));
            }
            Event::Text(t) | Event::Code(t) => {
                if let Some((_, _, title)) = current.as_mut() {
                    title.push_str(&t);
                }
            }
            Event::End(TagEnd::Heading(_)) => {
                if let Some((line, level, title)) = current.take() {
                    headings.push(Heading {
                        line,
                        level,
                        title: title.trim().to_string(),
                    });
                }
            }
            _ => {}
        }
    }
    headings
}

fn line_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            starts.push(i + 1);
        }
    }
    starts
}

fn offset_to_line(line_starts: &[usize], offset: usize) -> usize {
    line_starts.partition_point(|&s| s <= offset)
}

/// Inclusive body-relative end line of each heading's section:
/// up to the next heading of same-or-higher level, or end of document.
fn section_ends(headings: &[Heading], doc_lines: usize) -> Vec<usize> {
    headings
        .iter()
        .enumerate()
        .map(|(i, h)| {
            headings[i + 1..]
                .iter()
                .find(|other| other.level <= h.level)
                .map(|other| other.line - 1)
                .unwrap_or(doc_lines)
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
fn chunk_section(
    out: &mut Vec<Chunk>,
    path: &str,
    lines: &[&str],
    headings: &[Heading],
    sec_end: &[usize],
    i: usize,
    parent_bc: &str,
    max: usize,
    offset: usize,
) {
    let h = &headings[i];
    let title = format!("{} {}", "#".repeat(h.level), h.title);
    let bc = if parent_bc.is_empty() {
        title
    } else {
        format!("{parent_bc} > {title}")
    };
    let start = h.line;
    let end = sec_end[i];
    if end - start < max {
        push_chunk(out, path, lines, start, end, bc, offset);
        return;
    }
    let child_level = headings[i + 1..]
        .iter()
        .take_while(|c| c.line <= end)
        .filter(|c| c.level > h.level)
        .map(|c| c.level)
        .min();
    let Some(child_level) = child_level else {
        split_lines(out, path, lines, start, end, &bc, max, offset);
        return;
    };
    let children: Vec<usize> = (i + 1..headings.len())
        .take_while(|&j| headings[j].line <= end)
        .filter(|&j| headings[j].level == child_level)
        .collect();
    let intro_end = headings[children[0]].line - 1;
    emit_or_split(out, path, lines, start, intro_end, &bc, max, offset);
    for &c in &children {
        chunk_section(out, path, lines, headings, sec_end, c, &bc, max, offset);
    }
}

#[allow(clippy::too_many_arguments)]
fn emit_or_split(
    out: &mut Vec<Chunk>,
    path: &str,
    lines: &[&str],
    start: usize,
    end: usize,
    bc: &str,
    max: usize,
    offset: usize,
) {
    if start > end {
        return;
    }
    if end - start < max {
        push_chunk(out, path, lines, start, end, bc.to_string(), offset);
    } else {
        split_lines(out, path, lines, start, end, bc, max, offset);
    }
}

#[allow(clippy::too_many_arguments)]
fn split_lines(
    out: &mut Vec<Chunk>,
    path: &str,
    lines: &[&str],
    start: usize,
    end: usize,
    bc: &str,
    max: usize,
    offset: usize,
) {
    let parts = (end - start + 1).div_ceil(max);
    let mut pos = start;
    let mut idx = 1;
    while pos <= end {
        let chunk_end = (pos + max - 1).min(end);
        let label = if parts > 1 {
            format!("{bc} [Part {idx}/{parts}]")
        } else {
            bc.to_string()
        };
        push_chunk(out, path, lines, pos, chunk_end, label, offset);
        pos = chunk_end + 1;
        idx += 1;
    }
}

fn push_chunk(
    out: &mut Vec<Chunk>,
    path: &str,
    lines: &[&str],
    start: usize,
    end: usize,
    breadcrumb: String,
    offset: usize,
) {
    let text = lines[start - 1..end].join("\n");
    let search_text = if breadcrumb.is_empty() {
        text.clone()
    } else {
        format!("{breadcrumb}\n{text}")
    };
    out.push(Chunk {
        path: path.to_string(),
        start_line: start + offset,
        end_line: end + offset,
        breadcrumb,
        text,
        search_text,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(text: &str, max: usize) -> Vec<Chunk> {
        chunk_markdown("test.md", text, max, None, None)
    }

    #[test]
    fn splits_by_heading_structure_with_line_numbers() {
        let text = "# A\nline1\nline2\n## B\nline3\n### C\nline4\n# D\nline5\n";
        let out = chunks(text, 80);
        // `# A`'s section runs to the next same-or-higher heading, so B/C are inside it.
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].start_line, out[0].end_line), (1, 7));
        assert_eq!(out[0].breadcrumb, "# A");
        assert!(out[0].text.contains("### C"));
        assert_eq!((out[1].start_line, out[1].end_line), (8, 9));
        assert_eq!(out[1].breadcrumb, "# D");
    }

    #[test]
    fn preamble_before_first_heading_is_a_chunk() {
        let text = "intro line\nmore intro\n# A\nbody\n";
        let out = chunks(text, 80);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].start_line, out[0].end_line), (1, 2));
        assert_eq!(out[0].breadcrumb, "");
        assert_eq!((out[1].start_line, out[1].end_line), (3, 4));
    }

    #[test]
    fn long_leaf_section_splits_with_part_labels() {
        let mut text = String::from("# Big\n");
        for i in 1..=100 {
            text.push_str(&format!("line {i}\n"));
        }
        let out = chunks(&text, 20);
        assert_eq!(out.len(), 6); // 101 lines / 20
        assert_eq!((out[0].start_line, out[0].end_line), (1, 20));
        assert_eq!(out[0].breadcrumb, "# Big [Part 1/6]");
        assert_eq!((out[5].start_line, out[5].end_line), (101, 101));
        assert_eq!(out[5].breadcrumb, "# Big [Part 6/6]");
    }

    #[test]
    fn oversized_section_recurses_into_subsections() {
        let mut text = String::from("# Top\nintro\n");
        text.push_str("## S1\n");
        for i in 1..=5 {
            text.push_str(&format!("s1 line {i}\n"));
        }
        text.push_str("## S2\n");
        for i in 1..=5 {
            text.push_str(&format!("s2 line {i}\n"));
        }
        // 15 lines total, max 10: too big as one chunk, children fit individually.
        let out = chunks(&text, 10);
        assert_eq!(out.len(), 3);
        assert_eq!((out[0].start_line, out[0].end_line), (1, 2));
        assert_eq!(out[0].breadcrumb, "# Top");
        assert_eq!((out[1].start_line, out[1].end_line), (3, 8));
        assert_eq!(out[1].breadcrumb, "# Top > ## S1");
        assert_eq!((out[2].start_line, out[2].end_line), (9, 14));
        assert_eq!(out[2].breadcrumb, "# Top > ## S2");
    }

    #[test]
    fn frontmatter_offsets_lines_and_feeds_first_chunk() {
        let text = "---\nname: my-card\ndescription: 一张卡片\n---\n\n# A\nbody\n";
        let out = chunk_markdown("test.md", text, 80, Some("my-card"), Some("一张卡片"));
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].start_line, out[0].end_line), (6, 7));
        assert!(out[0].search_text.starts_with("my-card\n一张卡片\n"));
        assert!(!out[0].text.contains("my-card"));
    }

    #[test]
    fn frontmatter_only_document_still_yields_a_chunk() {
        let text = "---\nname: stub\ndescription: 只有元数据\n---\n";
        let out = chunk_markdown("test.md", text, 80, Some("stub"), Some("只有元数据"));
        assert_eq!(out.len(), 1);
        assert!(out[0].search_text.contains("stub"));
    }

    #[test]
    fn headings_inside_code_fences_are_ignored() {
        let text = "# Real\n```\n# not a heading\n```\nbody\n";
        let out = chunks(text, 80);
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].start_line, out[0].end_line), (1, 5));
    }
}
