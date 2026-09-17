use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TodoStatus {
    Pending,
    InProgress,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: TodoStatus,
}

#[derive(Debug, Default, Clone)]
pub struct TodoList {
    pub items: Vec<TodoItem>,
}

impl TodoList {
    pub fn replace(&mut self, items: Vec<TodoItem>) {
        self.items = items;
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Compact text form for the system prompt.
    pub fn render_prompt(&self) -> String {
        self.items
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let mark = match t.status {
                    TodoStatus::Done => "[x]",
                    TodoStatus::InProgress => "[~]",
                    TodoStatus::Pending => "[ ]",
                };
                format!("{}. {} {}", i + 1, mark, t.content)
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_and_render() {
        let mut list = TodoList::default();
        assert!(list.is_empty());
        list.replace(vec![
            TodoItem {
                content: "a".into(),
                status: TodoStatus::Done,
            },
            TodoItem {
                content: "b".into(),
                status: TodoStatus::InProgress,
            },
            TodoItem {
                content: "c".into(),
                status: TodoStatus::Pending,
            },
        ]);
        assert_eq!(list.render_prompt(), "1. [x] a\n2. [~] b\n3. [ ] c");
    }
}
