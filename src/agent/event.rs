use super::plan::{TodoItem, TodoStatus};
use tokio::sync::oneshot;

/// Commands a frontend (TUI, CLI, tests, ...) can send to a running session.
/// This is the entire input surface of the agent core: frontends never touch
/// `Agent` directly.
pub enum SessionCommand {
    /// Run one agent turn with the given user input.
    Submit(String),
    /// Stop the foreground operation, leaving background tasks running.
    Cancel,
    Shutdown,
    /// Start a fresh session (clears history and todos).
    NewSession,
    /// Toggle plan mode; the new state is reported back as `UiEvent::PlanMode`.
    TogglePlanMode,
    /// Compact the conversation history now.
    Compact,
    /// Subscribe to structured task snapshots.
    ShowTasks,
    HideTasks,
    StopTask(usize),
    /// Report the skills listing as `UiEvent::Info`.
    ShowSkills,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskState {
    Running,
    Stopping,
    Done,
    Failed,
    Stopped,
}

impl TaskState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "进行中",
            Self::Stopping => "停止中",
            Self::Done => "已完成",
            Self::Failed => "失败",
            Self::Stopped => "已停止",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TaskView {
    pub id: usize,
    pub kind: String,
    pub description: String,
    pub status: TaskState,
    pub output: String,
    pub elapsed_ms: u64,
}

/// UI-neutral view of a todo item, so frontends don't depend on `plan`.
#[derive(Clone)]
pub struct TodoView {
    pub content: String,
    pub status: TodoState,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TodoState {
    Pending,
    InProgress,
    Done,
}

impl From<&TodoItem> for TodoView {
    fn from(item: &TodoItem) -> Self {
        Self {
            content: item.content.clone(),
            status: match item.status {
                TodoStatus::Pending => TodoState::Pending,
                TodoStatus::InProgress => TodoState::InProgress,
                TodoStatus::Done => TodoState::Done,
            },
        }
    }
}

/// Everything the agent core can tell a frontend during a session.
pub enum UiEvent {
    Status(String),
    Cancelled,
    SessionReset,
    PlanMode(bool),
    BackgroundCount(usize),
    Tasks(Vec<TaskView>),
    /// Streaming assistant text delta.
    Text(String),
    ToolStart {
        id: String,
        name: String,
        summary: String,
        arguments: String,
    },
    ToolEnd {
        id: String,
        output: String,
        is_error: bool,
        duration_ms: u64,
    },
    /// Todo list changed; the frontend replaces its current todo block.
    Todos(Vec<TodoView>),
    /// System/background notice (hook output, task completion, ...).
    Note(String),
    /// Response to an informational command (ShowTasks, ShowSkills, ...).
    Info(String),
    PermissionRequest {
        text: String,
        respond: oneshot::Sender<bool>,
    },
    Done,
    Error(String),
}

impl std::fmt::Debug for UiEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UiEvent::Status(s) => write!(f, "Status({s:?})"),
            UiEvent::Cancelled => write!(f, "Cancelled"),
            UiEvent::SessionReset => write!(f, "SessionReset"),
            UiEvent::PlanMode(on) => write!(f, "PlanMode({on})"),
            UiEvent::Tasks(tasks) => write!(f, "Tasks({} items)", tasks.len()),
            UiEvent::BackgroundCount(n) => write!(f, "BackgroundCount({n})"),
            UiEvent::Text(t) => write!(f, "Text({t:?})"),
            UiEvent::ToolStart { name, .. } => write!(f, "ToolStart({name})"),
            UiEvent::ToolEnd { .. } => write!(f, "ToolEnd(..)"),
            UiEvent::Todos(t) => write!(f, "Todos({} items)", t.len()),
            UiEvent::Note(n) => write!(f, "Note({n:?})"),
            UiEvent::Info(i) => write!(f, "Info({i:?})"),
            UiEvent::PermissionRequest { .. } => write!(f, "PermissionRequest(..)"),
            UiEvent::Done => write!(f, "Done"),
            UiEvent::Error(e) => write!(f, "Error({e:?})"),
        }
    }
}

pub type EventSender = tokio::sync::mpsc::UnboundedSender<UiEvent>;

/// Event sink for sub-agents and background tasks: drops streaming noise,
/// keeps nothing on screen. Completion is reported by the task itself.
pub fn null_events() -> EventSender {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<UiEvent>();
    tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if let UiEvent::PermissionRequest { respond, .. } = ev {
                let _ = respond.send(false);
            }
        }
    });
    tx
}
