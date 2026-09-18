use super::plan::{TodoItem, TodoStatus};
use crate::i18n::{self, Key, Lang};
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
    ShowSessions,
    RestoreSession(String),
    /// Toggle plan mode; the new state is reported back as `UiEvent::PlanMode`.
    TogglePlanMode,
    /// Select one of the configured reasoning efforts.
    SetReasoningEffort(String),
    SelectModel(String),
    SetPermissionMode(crate::config::PermissionMode),
    /// Compact the conversation history now.
    Compact,
    /// Subscribe to structured task snapshots.
    ShowTasks,
    HideTasks,
    StopTask(usize),
    /// Report the skills listing as `UiEvent::Info`.
    ShowSkills,
    /// Inspect the next memory index or change session-local read/write controls.
    Memory {
        control: Option<MemoryControl>,
    },
    /// Switch the language of agent-side messages and of the system prompt.
    SetLang(Lang),
}

pub enum MemoryControl {
    Read(bool),
    Write(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TaskState {
    Running,
    Stopping,
    Done,
    Failed,
    Stopped,
}

impl TaskState {
    pub fn label(self, lang: Lang) -> &'static str {
        i18n::text(
            lang,
            match self {
                Self::Running => Key::TaskRunning,
                Self::Stopping => Key::TaskStopping,
                Self::Done => Key::TaskDone,
                Self::Failed => Key::TaskFailed,
                Self::Stopped => Key::TaskStopped,
            },
        )
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
    /// Latest foreground request input + output tokens; None means unavailable.
    ContextUsage(Option<u64>),
    ModelSettings {
        model: String,
        models: Vec<String>,
        reasoning_efforts: Vec<String>,
        reasoning_effort: Option<String>,
        context_window: Option<u64>,
    },
    Status(String),
    Cancelled,
    SessionReset,
    Sessions(Vec<super::transcripts::SessionView>),
    SessionRestored {
        id: String,
        records: Vec<super::transcripts::Record>,
    },
    WorkRestored(Vec<super::work::Trace>),
    SessionRestoreFailed(String),
    PlanMode(bool),
    PermissionMode(crate::config::PermissionMode),
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
            UiEvent::ContextUsage(tokens) => write!(f, "ContextUsage({tokens:?})"),
            UiEvent::ModelSettings {
                reasoning_effort,
                context_window,
                ..
            } => write!(f, "ModelSettings({reasoning_effort:?}, {context_window:?})"),
            UiEvent::Status(s) => write!(f, "Status({s:?})"),
            UiEvent::Cancelled => write!(f, "Cancelled"),
            UiEvent::Sessions(items) => write!(f, "Sessions({} items)", items.len()),
            UiEvent::SessionRestored { id, .. } => write!(f, "SessionRestored({id})"),
            UiEvent::SessionRestoreFailed(error) => write!(f, "SessionRestoreFailed({error})"),
            UiEvent::WorkRestored(_) => write!(f, "WorkRestored"),
            UiEvent::SessionReset => write!(f, "SessionReset"),
            UiEvent::PermissionMode(mode) => write!(f, "PermissionMode({mode:?})"),
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

/// Closed event sink for sub-agents: drops display events and permission senders.
/// The permission receiver observes cancellation, which the tool runner rejects.
pub fn null_events() -> EventSender {
    tokio::sync::mpsc::unbounded_channel().0
}
