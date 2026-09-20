use super::{text, theme, transcript};
use crate::{
    agent::graph::{Graph, Kind, Node, Status},
    i18n::{self, Key, Lang},
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line, Span, Text},
    widgets::Paragraph,
};
use std::collections::{HashMap, HashSet};

#[derive(Default)]
pub(super) struct View {
    pub selected: Option<String>,
    initialized: bool,
    known_turns: HashSet<String>,
    collapsed: HashSet<String>,
    expanded_tools: HashSet<String>,
    query: String,
    searching: bool,
    detail: bool,
    scroll: usize,
    input_index: usize,
    input_origin: Option<String>,
}

/// Conversation ancestry is independent of the internal execution hierarchy.
fn rows(graph: &Graph, view: &View) -> Vec<(String, usize)> {
    let mut children: HashMap<Option<String>, Vec<String>> = HashMap::new();
    for node in &graph.nodes {
        let parent = if node.kind == Kind::Turn {
            graph.turn_parent(&node.id)
        } else {
            node.parent_id.clone()
        };
        children
            .entry(parent.filter(|id| graph.get(id).is_some()))
            .or_default()
            .push(node.id.clone());
    }
    let mut result = Vec::new();
    let mut visited = HashSet::new();
    let mut stack: Vec<_> = children
        .get(&None)
        .into_iter()
        .flatten()
        .rev()
        .map(|id| (id.clone(), 0))
        .collect();
    while let Some((id, depth)) = stack.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let node = graph.get(&id).unwrap();
        if view.query.is_empty()
            || format!("{} {}", node.label, node.data)
                .to_lowercase()
                .contains(&view.query.to_lowercase())
        {
            result.push((id.clone(), depth));
        }
        if let Some(next) = children.get(&Some(id.clone())) {
            for child in next.iter().rev() {
                let is_turn = graph.get(child).is_some_and(|n| n.kind == Kind::Turn);
                // Collapsing a turn hides execution details, never its later conversation.
                if is_turn
                    || !view.query.is_empty()
                    || (!view.collapsed.contains(&id)
                        && (node.kind != Kind::Tool || view.expanded_tools.contains(&id)))
                {
                    stack.push((child.clone(), depth + 1));
                }
            }
        }
    }
    result
}

fn tree_prefix(rows: &[(String, usize)], index: usize) -> String {
    let depth = rows[index].1;
    let mut prefix = String::new();
    for level in 1..=depth.min(16) {
        let has_sibling = rows[index + 1..]
            .iter()
            .take_while(|(_, d)| *d >= level)
            .any(|(_, d)| *d == level);
        prefix.push_str(if level == depth {
            if has_sibling { "├─ " } else { "└─ " }
        } else if has_sibling {
            "│  "
        } else {
            "   "
        });
    }
    prefix
}

fn node_label(node: &Node) -> String {
    let label = text::clean(node.label.lines().next().unwrap_or_default());
    if node.kind != Kind::Tool {
        return label;
    }
    let arguments = node
        .data
        .get("effective_arguments")
        .map(|v| v.to_string())
        .or_else(|| {
            node.data.get("arguments").map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string())
            })
        })
        .unwrap_or_default();
    let summary = crate::agent::tools::summarize_args(&node.label, &arguments);
    if summary.is_empty() {
        label
    } else {
        format!("{label} {}", text::clean(&summary))
    }
}

pub(super) fn browsing(view: &View) -> bool {
    !view.searching && !view.detail
}

pub(super) fn retry_turn(view: &View, graph: &Graph) -> Option<String> {
    if view.searching || view.detail {
        return None;
    }
    view.selected
        .as_ref()
        .filter(|id| graph.get(id).is_some_and(|n| n.kind == Kind::Turn))
        .cloned()
}

pub(super) fn handle_key(view: &mut View, graph: &Graph, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    if ctrl && key.code == KeyCode::Char('c') {
        return true;
    }
    if view.searching {
        match key.code {
            KeyCode::Esc => {
                view.searching = false;
                view.query.clear();
            }
            KeyCode::Enter => view.searching = false,
            KeyCode::Backspace => {
                view.query.pop();
                view.selected = None;
            }
            KeyCode::Char(c) if !ctrl => {
                view.query.push(c);
                view.selected = None;
            }
            _ => {}
        }
        return false;
    }
    if key.code == KeyCode::Esc {
        if view.detail {
            view.detail = false;
            view.scroll = 0;
        } else if !view.query.is_empty() {
            view.query.clear();
        } else {
            return true;
        }
        return false;
    }
    if view.detail {
        match key.code {
            KeyCode::Up => view.scroll = view.scroll.saturating_sub(1),
            KeyCode::Down => view.scroll = view.scroll.saturating_add(1),
            KeyCode::PageUp => view.scroll = view.scroll.saturating_sub(8),
            KeyCode::PageDown => view.scroll = view.scroll.saturating_add(8),
            KeyCode::Home => view.scroll = 0,
            KeyCode::End => view.scroll = usize::MAX,
            KeyCode::Enter => {
                view.detail = false;
                view.scroll = 0;
            }
            _ => {}
        }
        return false;
    }
    if key.code != KeyCode::Tab {
        view.input_origin = None;
        view.input_index = 0;
    }
    let rows = rows(graph, view);
    let index = rows
        .iter()
        .position(|(id, _)| Some(id) == view.selected.as_ref())
        .unwrap_or(0);
    if view.selected.is_none() {
        view.selected = rows.first().map(|r| r.0.clone());
    }
    match key.code {
        KeyCode::Up | KeyCode::PageUp => {
            view.selected = rows
                .get(index.saturating_sub(if key.code == KeyCode::Up { 1 } else { 8 }))
                .map(|r| r.0.clone())
        }
        KeyCode::Down | KeyCode::PageDown => {
            view.selected = rows
                .get(
                    (index + if key.code == KeyCode::Down { 1 } else { 8 })
                        .min(rows.len().saturating_sub(1)),
                )
                .map(|r| r.0.clone())
        }
        KeyCode::Home => view.selected = rows.first().map(|r| r.0.clone()),
        KeyCode::End => view.selected = rows.last().map(|r| r.0.clone()),
        KeyCode::Left => {
            if let Some(id) = &view.selected {
                view.collapsed.insert(id.clone());
                view.expanded_tools.remove(id);
            }
        }
        KeyCode::Right => {
            if let Some(id) = &view.selected {
                view.collapsed.remove(id);
                view.expanded_tools.insert(id.clone());
            }
        }
        KeyCode::Enter | KeyCode::Char('d') => {
            view.detail = true;
            view.scroll = 0;
        }
        KeyCode::Char('/') => {
            view.searching = true;
            view.query.clear();
        }
        KeyCode::Tab => {
            if view.input_origin.is_none() {
                view.input_origin = view.selected.clone();
            }
            if let Some(node) = view.input_origin.as_deref().and_then(|id| graph.get(id)) {
                let target = node
                    .inputs
                    .get(view.input_index % node.inputs.len().max(1))
                    .or(node.parent_id.as_ref())
                    .cloned();
                view.input_index += 1;
                if let Some(target) = target {
                    view.query.clear();
                    let mut ancestor = graph.get(&target);
                    let mut visited = HashSet::new();
                    while let Some(node) = ancestor {
                        if !visited.insert(&node.id) {
                            break;
                        }
                        view.collapsed.remove(&node.id);
                        view.expanded_tools.insert(node.id.clone());
                        ancestor = node.parent_id.as_deref().and_then(|id| graph.get(id));
                    }
                    view.selected = Some(target);
                }
            }
        }
        _ => {}
    }
    false
}

pub(super) fn draw(f: &mut Frame, graph: &Graph, view: &mut View, area: Rect, lang: Lang) {
    if area.is_empty() {
        return;
    }
    for turn in graph.nodes.iter().filter(|n| n.kind == Kind::Turn) {
        if view.known_turns.insert(turn.id.clone()) && view.initialized {
            view.collapsed.insert(turn.id.clone());
        }
    }
    if !view.initialized && !graph.nodes.is_empty() {
        let turns: Vec<_> = graph
            .nodes
            .iter()
            .filter(|n| n.kind == Kind::Turn)
            .collect();
        for turn in turns.iter() {
            view.collapsed.insert(turn.id.clone());
        }
        view.selected = graph
            .current_turn
            .clone()
            .or_else(|| turns.last().map(|n| n.id.clone()));
        view.initialized = true;
    }
    if view.detail {
        let Some(node) = view.selected.as_deref().and_then(|id| graph.get(id)) else {
            return;
        };
        let content = serde_json::to_string_pretty(node).unwrap_or_default();
        let lines = transcript::literal(&text::clean(&content), area.width as usize, theme::text());
        view.scroll = view
            .scroll
            .min(lines.len().saturating_sub(area.height as usize));
        f.render_widget(
            Paragraph::new(Text::from(
                lines
                    .into_iter()
                    .skip(view.scroll)
                    .take(area.height as usize)
                    .collect::<Vec<_>>(),
            )),
            area,
        );
        return;
    }
    let rows = rows(graph, view);
    let index = rows
        .iter()
        .position(|(id, _)| Some(id) == view.selected.as_ref())
        .unwrap_or(0);
    view.selected = rows.get(index).map(|r| r.0.clone());
    let mut lines = vec![Line::styled(
        format!(
            "{}: {}{} · {} nodes",
            i18n::text(lang, Key::GraphSearch),
            text::clean(&view.query),
            if view.searching { "▏" } else { "" },
            graph.nodes.len()
        ),
        theme::muted(),
    )];
    lines.push(Line::styled(
        i18n::text(lang, Key::GraphLegend),
        theme::muted(),
    ));
    let capacity = (area.height.saturating_sub(2) as usize).div_ceil(2);
    let start = index.saturating_sub(capacity.saturating_sub(1));
    if rows.is_empty() {
        lines.push(Line::styled(
            i18n::text(lang, Key::GraphEmpty),
            theme::muted(),
        ));
    }
    for (row, (id, depth)) in rows.iter().enumerate().skip(start).take(capacity) {
        let node = graph.get(id).unwrap();
        let status = match node.status {
            Status::Running => "●",
            Status::Succeeded => "✓",
            Status::Failed => "✗",
            Status::Rejected => "⊘",
            Status::Unknown => "?",
        };
        let kind = match (node.kind, lang) {
            (Kind::Turn, Lang::Zh) => "轮次",
            (Kind::Subagent, Lang::Zh) => "子任务",
            (Kind::Model, Lang::Zh) => "模型",
            (Kind::Tool, Lang::Zh) => "工具",
            (Kind::Background, Lang::Zh) => "后台",
            (Kind::Compaction, Lang::Zh) => "压缩",
            (Kind::Turn, _) => "turn",
            (Kind::Subagent, _) => "agent",
            (Kind::Model, _) => "model",
            (Kind::Tool, _) => "tool",
            (Kind::Background, _) => "background",
            (Kind::Compaction, _) => "compact",
        };
        let tool_collapsed = node.kind == Kind::Tool && !view.expanded_tools.contains(id);
        let label = if tool_collapsed {
            text::clean(&node.label)
        } else {
            node_label(node)
        };
        let duration = node
            .duration_ms
            .map(|ms| format!(" {ms}ms"))
            .unwrap_or_default();
        let selected = Some(id) == view.selected.as_ref();
        let heading = if node.kind == Kind::Turn {
            format!(
                "{} {kind}: {label}",
                if view.collapsed.contains(id) {
                    "▸"
                } else {
                    "▾"
                }
            )
        } else if node.kind == Kind::Tool {
            format!("{} {kind}: {label}", if tool_collapsed { "▸" } else { "▾" })
        } else {
            format!(
                "{kind}: {label}{}",
                if view.collapsed.contains(id) {
                    " [+]"
                } else {
                    ""
                }
            )
        };
        let failed = matches!(node.status, Status::Failed | Status::Rejected);
        let state = match (node.status, lang) {
            (Status::Failed, Lang::Zh) => " 失败",
            (Status::Failed, _) => " failed",
            (Status::Rejected, Lang::Zh) => " 已拒绝",
            (Status::Rejected, _) => " rejected",
            _ => "",
        };
        lines.push(Line::from(vec![
            Span::styled(if selected { "❯ " } else { "  " }, theme::text()),
            Span::styled(tree_prefix(&rows, row), theme::muted()),
            Span::styled(
                format!("{status}{state} "),
                if failed {
                    theme::error()
                } else if node.status == Status::Succeeded {
                    theme::success()
                } else {
                    theme::muted()
                },
            ),
            Span::styled(
                heading,
                if selected {
                    theme::text().add_modifier(
                        ratatui::style::Modifier::BOLD | ratatui::style::Modifier::UNDERLINED,
                    )
                } else {
                    theme::text()
                },
            ),
            Span::styled(duration, theme::muted()),
            Span::styled(
                if graph.current_turn.as_ref() == Some(id) {
                    if lang == Lang::Zh {
                        " ◀ 当前"
                    } else {
                        " ◀ current"
                    }
                } else {
                    ""
                },
                theme::accent(),
            ),
        ]));
        let answer = node
            .data
            .pointer("/result/output")
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty());
        lines.push(Line::styled(
            if node.kind == Kind::Turn {
                answer
                    .map(|answer| {
                        format!(
                            "{}  {} {}",
                            "  ".repeat((*depth).min(16)),
                            if lang == Lang::Zh { "答:" } else { "A:" },
                            text::clean(answer.lines().next().unwrap_or_default())
                        )
                    })
                    .unwrap_or_default()
            } else {
                String::new()
            },
            theme::muted(),
        ));
    }
    // Keep navigation and status hints above the scrolling conversation tree.
    let headers: Vec<_> = lines.drain(..2).collect();
    f.render_widget(
        Paragraph::new(Text::from(headers)),
        Rect {
            height: area.height.min(2),
            ..area
        },
    );
    let body = Rect {
        y: area.y.saturating_add(2),
        height: area.height.saturating_sub(2),
        ..area
    };
    f.render_widget(Paragraph::new(Text::from(lines)), body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture() -> Graph {
        let mut graph = Graph::default();
        for (id, parent, kind, inputs, data) in [
            ("turn", None, "turn", vec![], json!({})),
            ("model", Some("turn"), "model", vec![], json!({})),
            (
                "a",
                Some("model"),
                "tool",
                vec![],
                json!({"output":"unique needle"}),
            ),
            ("b", Some("model"), "tool", vec![], json!({})),
            ("next", Some("turn"), "model", vec!["a", "b"], json!({})),
        ] {
            graph.apply(serde_json::from_value(json!({"id":id,"session_id":"s","run_id":"turn","parent_id":parent,"inputs":inputs,
                "kind":kind,"label":id,"started_at":"2026-09-20T00:00:00Z","finished_at":null,"duration_ms":null,"status":"succeeded","data":data})).unwrap());
        }
        graph
    }
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn search_finds_collapsed_results_and_tab_visits_each_input() {
        let graph = fixture();
        let mut view = View {
            selected: Some("model".into()),
            ..Default::default()
        };
        handle_key(&mut view, &graph, key(KeyCode::Left));
        assert_eq!(rows(&graph, &view).len(), 3);
        handle_key(&mut view, &graph, key(KeyCode::Char('/')));
        for c in "needle".chars() {
            handle_key(&mut view, &graph, key(KeyCode::Char(c)));
        }
        assert_eq!(rows(&graph, &view), [("a".into(), 2)]);
        handle_key(&mut view, &graph, key(KeyCode::Esc));
        view.selected = Some("next".into());
        handle_key(&mut view, &graph, key(KeyCode::Tab));
        assert_eq!(view.selected.as_deref(), Some("a"));
        assert!(!view.collapsed.contains("model"));
        handle_key(&mut view, &graph, key(KeyCode::Tab));
        assert_eq!(view.selected.as_deref(), Some("b"));
        handle_key(&mut view, &graph, key(KeyCode::Enter));
        assert!(view.detail);
        assert!(!handle_key(&mut view, &graph, key(KeyCode::Esc)));
        assert!(!view.detail);
        assert!(handle_key(&mut view, &graph, key(KeyCode::Esc)));
    }

    #[test]
    fn tools_are_unboxed_and_collapsed_until_expanded() {
        let mut graph = fixture();
        let mut tool = graph.get("a").unwrap().clone();
        tool.label = "bash".into();
        tool.data = json!({"arguments":"{\"command\":\"cargo test\"}"});
        graph.apply(tool);
        let mut view = View::default();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 20)).unwrap();
        let render =
            |view: &mut View, terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>| {
                terminal
                    .draw(|f| draw(f, &graph, view, f.area(), Lang::En))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                (0..20)
                    .map(|y| {
                        (0..100)
                            .map(|x| buffer[(x, y)].symbol())
                            .collect::<String>()
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            };
        let screen = render(&mut view, &mut terminal);
        assert!(!screen.contains('┐') && !screen.contains('┘'));
        handle_key(&mut view, &graph, key(KeyCode::Right));
        let screen = render(&mut view, &mut terminal);
        assert!(!screen.contains("cargo test"));
        view.selected = Some("a".into());
        handle_key(&mut view, &graph, key(KeyCode::Right));
        assert!(render(&mut view, &mut terminal).contains("cargo test"));
        handle_key(&mut view, &graph, key(KeyCode::Left));
        assert!(!render(&mut view, &mut terminal).contains("cargo test"));
    }

    #[test]
    fn tree_shows_sibling_branches_and_current_position() {
        let mut graph = fixture();
        for (id, parent) in [("branch-b", "turn"), ("branch-c", "turn")] {
            let mut turn = graph.get("turn").unwrap().clone();
            turn.id = id.into();
            turn.label = id.into();
            turn.data = json!({"input":id,"conversation_parent":parent,"result":{"output":"answer preview"}});
            graph.apply(turn);
        }
        let mut view = View::default();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 20)).unwrap();
        terminal
            .draw(|f| draw(f, &graph, &mut view, f.area(), Lang::En))
            .unwrap();
        let visible = rows(&graph, &view);
        assert_eq!(
            visible,
            [
                ("turn".into(), 0),
                ("branch-b".into(), 1),
                ("branch-c".into(), 1)
            ]
        );
        assert_eq!(tree_prefix(&visible, 1), "├─ ");
        assert_eq!(tree_prefix(&visible, 2), "└─ ");
        assert_eq!(view.selected.as_deref(), Some("branch-c"));
        let screen = (0..20)
            .map(|y| {
                (0..90)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(screen.contains("◀ current") && screen.contains("answer preview"));
    }

    #[test]
    fn default_view_shows_only_turns_and_retry_targets_turns() {
        let graph = fixture();
        let mut view = View::default();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 20)).unwrap();
        terminal
            .draw(|f| draw(f, &graph, &mut view, f.area(), Lang::En))
            .unwrap();
        assert_eq!(rows(&graph, &view), [("turn".into(), 0)]);
        assert_eq!(retry_turn(&view, &graph).as_deref(), Some("turn"));
        handle_key(&mut view, &graph, key(KeyCode::Right));
        assert!(rows(&graph, &view).len() > 1);
        view.selected = Some("a".into());
        assert!(retry_turn(&view, &graph).is_none());
        handle_key(&mut view, &graph, key(KeyCode::Char('/')));
        assert!(!browsing(&view));
        handle_key(&mut view, &graph, key(KeyCode::Char('r')));
        assert_eq!(view.query, "r");
    }

    #[test]
    fn tool_labels_use_effective_paths() {
        let graph = fixture();
        let mut node = graph.get("a").unwrap().clone();
        node.label = "read".into();
        node.data =
            json!({"arguments":"{\"path\":\"old\"}", "effective_arguments":{"path":"src/main.rs"}});
        assert_eq!(node_label(&node), "read src/main.rs");
    }

    #[test]
    fn graph_renders_unicode_and_details_at_small_terminal_sizes() {
        let graph = fixture();
        for (width, height) in [(1, 1), (20, 8), (100, 30)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            let mut view = View::default();
            terminal
                .draw(|f| draw(f, &graph, &mut view, f.area(), Lang::Zh))
                .unwrap();
            view.selected = Some("a".into());
            view.detail = true;
            view.scroll = usize::MAX;
            terminal
                .draw(|f| draw(f, &graph, &mut view, f.area(), Lang::Zh))
                .unwrap();
        }
    }
}
