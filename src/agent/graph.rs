//! Durable execution graph. Containment and input edges are recorded explicitly;
//! display order never implies that one operation consumed another's output.
use super::work::Journal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc, time::Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Turn,
    Subagent,
    Model,
    Tool,
    Background,
    Compaction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Running,
    Succeeded,
    Failed,
    Rejected,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub session_id: String,
    pub run_id: String,
    pub parent_id: Option<String>,
    pub inputs: Vec<String>,
    pub kind: Kind,
    pub label: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub duration_ms: Option<u64>,
    pub status: Status,
    pub data: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub version: u32,
    pub id: String,
    pub node: Node,
}

#[derive(Debug, Clone, Default)]
pub struct Graph {
    pub current_turn: Option<String>,
    pub nodes: Vec<Arc<Node>>,
    index: HashMap<String, usize>,
}
impl Graph {
    pub fn apply(&mut self, node: Node) {
        if let Some(&index) = self.index.get(&node.id) {
            self.nodes[index] = Arc::new(node);
        } else {
            if node.kind == Kind::Turn {
                self.current_turn = Some(node.id.clone());
            }
            self.index.insert(node.id.clone(), self.nodes.len());
            self.nodes.push(Arc::new(node));
        }
    }
    pub fn turn_parent(&self, id: &str) -> Option<String> {
        let node = self.get(id)?;
        if let Some(parent) = node.data.get("conversation_parent") {
            return parent.as_str().map(str::to_owned);
        }
        // Legacy journals are linear until an explicit navigation was recorded.
        self.nodes
            .iter()
            .take_while(|n| n.id != id)
            .filter(|n| n.kind == Kind::Turn)
            .last()
            .map(|n| n.id.clone())
    }

    pub fn get(&self, id: &str) -> Option<&Node> {
        self.index.get(id).map(|&i| self.nodes[i].as_ref())
    }
}

/// Cloned through sub-agent seeds, independent of their suppressed display sink.
#[derive(Clone)]
pub struct Recorder {
    journal: Journal,
    parent_id: Option<String>,
    run_id: String,
}
impl Recorder {
    pub fn new(journal: Journal) -> Self {
        Self {
            journal,
            parent_id: None,
            run_id: String::new(),
        }
    }
    pub fn annotate(&self, key: &str, value: Value) -> std::io::Result<()> {
        if let Some(id) = &self.parent_id
            && let Some(mut node) = self.journal.graph_get(id)
        {
            node.data[key] = value;
            self.journal.graph_node(node)?;
        }
        Ok(())
    }
    pub fn start(
        &self,
        kind: Kind,
        label: impl Into<String>,
        data: Value,
        inputs: Vec<String>,
    ) -> std::io::Result<Span> {
        let id = uuid::Uuid::new_v4().to_string();
        let node = Node {
            session_id: self
                .journal
                .path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            run_id: if matches!(kind, Kind::Turn | Kind::Subagent) || self.run_id.is_empty() {
                id.clone()
            } else {
                self.run_id.clone()
            },
            id,
            parent_id: self.parent_id.clone(),
            inputs,
            kind,
            label: label.into(),
            started_at: chrono::Utc::now().to_rfc3339(),
            finished_at: None,
            duration_ms: None,
            status: Status::Running,
            data,
        };
        self.journal.graph_node(node.clone())?;
        Ok(Span {
            recorder: self.clone(),
            node,
            started: Instant::now(),
            finished: false,
        })
    }
}

pub struct Span {
    recorder: Recorder,
    pub node: Node,
    started: Instant,
    finished: bool,
}
impl Span {
    pub fn recorder(&self) -> Recorder {
        Recorder {
            parent_id: Some(self.node.id.clone()),
            run_id: self.node.run_id.clone(),
            ..self.recorder.clone()
        }
    }
    pub fn finish(&mut self, status: Status, output: Value) -> std::io::Result<()> {
        if let Some(current) = self.recorder.journal.graph_get(&self.node.id) {
            self.node.data = current.data;
        }
        self.node.status = if status == Status::Failed && self.node.data["permission"] == "rejected"
        {
            Status::Rejected
        } else {
            status
        };
        self.node.finished_at = Some(chrono::Utc::now().to_rfc3339());
        self.node.duration_ms = Some(self.started.elapsed().as_millis() as u64);
        self.node.data["result"] = output;
        self.recorder.journal.graph_node(self.node.clone())?;
        self.finished = true;
        Ok(())
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        if !self.finished {
            // Dropping a future cannot establish whether its external effects happened.
            let _ = self.finish(
                Status::Unknown,
                serde_json::json!({"reason": "Execution interrupted; outcome unknown"}),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn journal_restores_edges_annotations_and_unknown_outcomes() {
        let root = std::env::temp_dir().join(format!("koala-graph-{}", uuid::Uuid::new_v4()));
        let journal = Journal::new(root.join("session.work"));
        let recorder = Recorder::new(journal.clone());
        let mut turn = recorder
            .start(Kind::Turn, "hello", json!({"input":"hello"}), vec![])
            .unwrap();
        let mut model = turn
            .recorder()
            .start(Kind::Model, "test", json!({}), vec![])
            .unwrap();
        model.finish(Status::Succeeded, json!({})).unwrap();
        let mut tool = model
            .recorder()
            .start(Kind::Tool, "read", json!({}), vec![])
            .unwrap();
        tool.recorder()
            .annotate("effective_arguments", json!({"path":"example"}))
            .unwrap();
        tool.finish(Status::Succeeded, json!({"output":"contents"}))
            .unwrap();
        let next = turn
            .recorder()
            .start(Kind::Model, "test", json!({}), vec![tool.node.id.clone()])
            .unwrap();
        // Loading the active journal must not interrupt live nodes.
        assert_eq!(
            journal
                .load()
                .unwrap()
                .unwrap()
                .graph
                .get(&next.node.id)
                .unwrap()
                .status,
            Status::Running
        );
        // A fresh process cannot claim that a saved running operation is still alive.
        let fresh = Journal::new(journal.path.clone());
        let restored = fresh.load().unwrap().unwrap().graph;
        assert_eq!(restored.nodes.len(), 4);
        assert_eq!(restored.get(&next.node.id).unwrap().status, Status::Unknown);
        assert_eq!(
            restored.get(&next.node.id).unwrap().inputs,
            [tool.node.id.clone()]
        );
        let saved_tool = restored.get(&tool.node.id).unwrap();
        assert_eq!(saved_tool.parent_id.as_ref(), Some(&model.node.id));
        assert_eq!(saved_tool.data["effective_arguments"]["path"], "example");
        assert_eq!(saved_tool.run_id, turn.node.id);
        let next_id = next.node.id.clone();
        drop(next);
        assert_eq!(
            journal.graph_snapshot().get(&next_id).unwrap().status,
            Status::Unknown
        );
        turn.finish(Status::Failed, json!({"error":"interrupted"}))
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn legacy_work_journal_has_no_invented_graph_nodes() {
        let root =
            std::env::temp_dir().join(format!("koala-graph-legacy-{}", uuid::Uuid::new_v4()));
        let journal = Journal::new(root.join("session.work"));
        journal
            .trace(super::super::work::Trace::User("old prompt".into()))
            .unwrap();
        let saved = journal.load().unwrap().unwrap();
        assert!(saved.graph.nodes.is_empty());
        assert_eq!(saved.messages[0].content.as_deref(), Some("old prompt"));
        std::fs::remove_dir_all(root).unwrap();
    }
}
