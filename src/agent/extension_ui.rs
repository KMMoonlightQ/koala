//! Session-owned UI state. Callbacks never acquire the Agent mutex.
use super::event::{EventSender, UiEvent};
use crate::extensions::{
    ui::{self, RequestContext, UiDelivery},
    *,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::{
    sync::{mpsc, oneshot},
    task::{JoinHandle, JoinSet},
};

#[derive(Debug, Clone)]
pub struct UiAction {
    pub extension: String,
    pub event: UiInputEvent,
}
#[derive(Debug, Clone)]
pub struct Surface {
    pub extension: String,
    pub id: String,
    pub kind: SurfaceKind,
    pub revision: u64,
    /// Changes only on replacement, so acknowledgement does not clear local drafts.
    pub content_revision: u64,
    pub pending: bool,
    pub placement: Placement,
    pub blocks: Vec<UiBlock>,
    pub dialog: Option<Dialog>,
}
#[derive(Debug, Clone)]
pub struct StatusItem {
    pub extension: String,
    pub id: String,
    pub text: String,
}
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub surfaces: Vec<Surface>,
    pub statuses: Vec<StatusItem>,
}
#[derive(Default)]
struct State {
    snapshot: Snapshot,
}
// Revisions never repeat across live/replaced sessions in this process.
static NEXT_REVISION: AtomicU64 = AtomicU64::new(1);
fn next_revision() -> u64 {
    NEXT_REVISION.fetch_add(1, Ordering::Relaxed)
}
impl State {
    fn apply(&mut self, owner: &str, commands: Vec<UiCommand>) -> Result<Vec<String>, String> {
        validate_ui(&commands)?;
        let mut snapshot = self.snapshot.clone();
        let mut notes = vec![];
        for command in commands {
            match command {
                UiCommand::Notify { level, text } => {
                    notes.push(format!("[{owner} · {level:?}] {text}"))
                }
                UiCommand::RemoveStatus { id } => snapshot
                    .statuses
                    .retain(|s| s.extension != owner || s.id != id),
                UiCommand::SetStatus { id, text } => {
                    if let Some(s) = snapshot
                        .statuses
                        .iter_mut()
                        .find(|s| s.extension == owner && s.id == id)
                    {
                        s.text = text;
                    } else {
                        snapshot.statuses.push(StatusItem {
                            extension: owner.into(),
                            id,
                            text,
                        });
                    }
                }
                UiCommand::RemoveWidget { id } => snapshot.surfaces.retain(|s| {
                    !(s.extension == owner && s.id == id && s.kind == SurfaceKind::Widget)
                }),
                UiCommand::CloseDialog { id } => snapshot.surfaces.retain(|s| {
                    !(s.extension == owner && s.id == id && s.kind == SurfaceKind::Dialog)
                }),
                command => {
                    let (id, kind, placement, blocks, dialog) = match command {
                        UiCommand::SetWidget {
                            id,
                            placement,
                            blocks,
                        } => (id, SurfaceKind::Widget, placement, blocks, None),
                        UiCommand::OpenDialog { id, dialog } => {
                            let blocks = match &dialog {
                                Dialog::Form { blocks, .. } => blocks.clone(),
                                _ => vec![],
                            };
                            (
                                id,
                                SurfaceKind::Dialog,
                                Placement::AboveEditor,
                                blocks,
                                Some(dialog),
                            )
                        }
                        _ => unreachable!(),
                    };
                    let revision = next_revision();
                    let surface = Surface {
                        extension: owner.into(),
                        id: id.clone(),
                        kind,
                        revision,
                        content_revision: revision,
                        pending: false,
                        placement,
                        blocks,
                        dialog,
                    };
                    if let Some(old) = snapshot
                        .surfaces
                        .iter_mut()
                        .find(|s| s.extension == owner && s.id == id && s.kind == kind)
                    {
                        *old = surface;
                    } else {
                        snapshot.surfaces.push(surface);
                    }
                }
            }
            if snapshot
                .statuses
                .iter()
                .filter(|s| s.extension == owner)
                .count()
                > 16
                || snapshot
                    .surfaces
                    .iter()
                    .filter(|s| s.extension == owner && s.kind == SurfaceKind::Widget)
                    .count()
                    > 32
                || snapshot
                    .surfaces
                    .iter()
                    .filter(|s| s.extension == owner && s.kind == SurfaceKind::Dialog)
                    .count()
                    > 8
            {
                return Err("extension UI resource limit exceeded".into());
            }
        }
        self.snapshot = snapshot;
        Ok(notes)
    }
    fn surface(&self, action: &UiAction) -> Option<&Surface> {
        self.snapshot.surfaces.iter().find(|s| {
            s.extension == action.extension
                && Some(s.kind) == action.event.surface
                && Some(&s.id) == action.event.surface_id.as_ref()
        })
    }
    fn current(&self, action: &UiAction) -> bool {
        self.surface(action)
            .is_some_and(|s| s.revision == action.event.revision)
    }
    fn accept(&mut self, action: &UiAction) -> Result<(), String> {
        let e = &action.event;
        let s = self.surface(action).ok_or("unknown UI component")?;
        if s.revision != e.revision || s.pending {
            return Err("stale or pending UI component".into());
        }
        let valid = if e.kind == UiEventType::Cancel {
            s.kind == SurfaceKind::Dialog && e.control_id.is_none() && e.value.is_null()
        } else if matches!(s.dialog, Some(Dialog::Confirm { .. })) {
            e.kind == UiEventType::Confirm && e.value.is_boolean() && e.control_id.is_none()
        } else {
            s.blocks.iter().any(|block| {
                block.id() == e.control_id.as_deref()
                    && match block {
                        UiBlock::Button { .. } => e.kind == UiEventType::Click && e.value.is_null(),
                        UiBlock::Select { options, .. } => {
                            e.kind == UiEventType::Select
                                && options.iter().any(|o| e.value.as_str() == Some(&o.id))
                        }
                        UiBlock::Input { .. } => {
                            e.kind == UiEventType::Submit
                                && e.value.as_str().is_some_and(|v| {
                                    v.len() <= MAX_TEXT && !v.contains(['\n', '\r'])
                                })
                        }
                        _ => false,
                    }
            })
        };
        if !valid {
            return Err("invalid UI event or value".into());
        }
        let s = self
            .snapshot
            .surfaces
            .iter_mut()
            .find(|s| {
                s.extension == action.extension
                    && Some(s.kind) == e.surface
                    && Some(&s.id) == e.surface_id.as_ref()
            })
            .unwrap();
        s.pending = true;
        Ok(())
    }
    fn finish(&mut self, action: &UiAction, succeeded: bool) {
        if !self.current(action) {
            return;
        }
        if action.event.kind == UiEventType::Cancel
            || (succeeded && action.event.kind == UiEventType::Confirm)
        {
            self.snapshot.surfaces.retain(|s| {
                !(s.extension == action.extension
                    && Some(s.kind) == action.event.surface
                    && Some(&s.id) == action.event.surface_id.as_ref())
            });
        } else {
            for s in &mut self.snapshot.surfaces {
                if s.extension == action.extension
                    && Some(s.kind) == action.event.surface
                    && Some(&s.id) == action.event.surface_id.as_ref()
                {
                    s.pending = false;
                    s.revision = next_revision();
                }
            }
        }
    }
    fn cancel(&mut self) {
        self.snapshot
            .surfaces
            .retain(|s| s.kind == SurfaceKind::Widget);
        for s in &mut self.snapshot.surfaces {
            s.revision = next_revision();
            s.pending = false;
        }
    }
}

enum Command {
    Action(UiAction),
    Cancel(oneshot::Sender<()>),
    Shutdown(oneshot::Sender<()>),
}
pub(super) struct Host {
    commands: mpsc::UnboundedSender<Command>,
    context: RequestContext,
    generation: Arc<AtomicU64>,
    task: JoinHandle<()>,
}
impl Drop for Host {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Host {
    pub fn start(
        session_id: String,
        extensions: Vec<Arc<dyn Extension>>,
        events: EventSender,
    ) -> Self {
        let (commands, rx) = mpsc::unbounded_channel();
        let (updates, incoming) = mpsc::unbounded_channel();
        let generation = Arc::new(AtomicU64::new(0));
        let context = RequestContext {
            session_id,
            ui: true,
            generation: 0,
            updates: Some(updates),
        };
        let task = tokio::spawn(run(
            rx,
            incoming,
            extensions,
            events,
            context.clone(),
            generation.clone(),
        ));
        Self {
            commands,
            context,
            generation,
            task,
        }
    }
    pub fn context(&self) -> RequestContext {
        RequestContext {
            generation: self.generation.load(Ordering::SeqCst),
            ..self.context.clone()
        }
    }
    pub fn action(&self, action: UiAction) {
        let _ = self.commands.send(Command::Action(action));
    }
    pub async fn cancel(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        let _ = self.commands.send(Command::Cancel(tx));
        let _ = rx.await;
    }
    pub async fn shutdown(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        let _ = self.commands.send(Command::Shutdown(tx));
        let _ = rx.await;
    }
}
struct Completion {
    action: UiAction,
    generation: u64,
    result: Result<Response, String>,
}
fn launch(
    tasks: &mut JoinSet<Completion>,
    extension: Arc<dyn Extension>,
    mut action: UiAction,
    context: RequestContext,
) {
    // event_id is generated by the host, never trusted from a frontend.
    action.event.event_id = uuid::Uuid::new_v4().to_string();
    tasks.spawn(async move {
        let generation = context.generation;
        let result = ui::scope(context, extension.ui_event(&action.event)).await;
        Completion {
            action,
            generation,
            result,
        }
    });
}
fn publish(state: &State, events: &EventSender) {
    let _ = events.send(UiEvent::ExtensionUi(state.snapshot.clone()));
}
async fn run(
    mut commands: mpsc::UnboundedReceiver<Command>,
    mut incoming: mpsc::UnboundedReceiver<UiDelivery>,
    extensions: Vec<Arc<dyn Extension>>,
    events: EventSender,
    context: RequestContext,
    generation: Arc<AtomicU64>,
) {
    let extensions: HashMap<_, _> = extensions
        .into_iter()
        .map(|e| (e.name().to_owned(), e))
        .collect();
    let mut state = State::default();
    let mut tasks = JoinSet::new();
    let mut queues: HashMap<String, VecDeque<UiAction>> = HashMap::new();
    for (name, extension) in &extensions {
        queues.insert(name.clone(), VecDeque::new());
        launch(
            &mut tasks,
            extension.clone(),
            UiAction {
                extension: name.clone(),
                event: UiInputEvent {
                    kind: UiEventType::Mount,
                    event_id: String::new(),
                    surface: None,
                    surface_id: None,
                    revision: 0,
                    control_id: None,
                    value: serde_json::Value::Null,
                },
            },
            context.clone(),
        );
    }
    loop {
        tokio::select! {
            biased;
            _ = events.closed() => break,
            command = commands.recv() => match command {
                None => break,
                Some(Command::Shutdown(done)) => { tasks.abort_all(); while tasks.join_next().await.is_some() {} let _ = done.send(()); break; }
                Some(Command::Cancel(done)) => {
                    tasks.abort_all(); while tasks.join_next().await.is_some() {} queues.clear();
                    state.cancel(); publish(&state, &events); let _ = done.send(());
                }
                Some(Command::Action(action)) => {
                    let Some(extension) = extensions.get(&action.extension) else { continue; };
                    let result = if queues.get(&action.extension).is_some_and(|q| q.len() >= 16) { Err("too many pending UI events".into()) } else { state.accept(&action) };
                    match result {
                        Err(error) => { let _ = events.send(UiEvent::Note(format!("[{}] {error}", action.extension))); }
                        Ok(()) => {
                            if let Some(queue) = queues.get_mut(&action.extension) { queue.push_back(action); }
                            else {
                                queues.insert(action.extension.clone(), VecDeque::new());
                                launch(&mut tasks, extension.clone(), action, RequestContext { generation: generation.load(Ordering::SeqCst), ..context.clone() });
                            }
                            publish(&state, &events);
                        }
                    }
                }
            },
            Some(batch) = incoming.recv() => {
                if batch.generation != generation.load(Ordering::SeqCst) || !extensions.contains_key(&batch.extension) { continue; }
                match state.apply(&batch.extension, batch.commands) {
                    Ok(notes) => for note in notes { let _ = events.send(UiEvent::Note(note)); },
                    Err(error) => { let _ = events.send(UiEvent::Note(format!("[{}] {error}", batch.extension))); }
                }
                publish(&state, &events);
            }
            Some(completed) = tasks.join_next(), if !tasks.is_empty() => {
                match completed {
                    Err(error) => { let _ = events.send(UiEvent::Note(format!("extension UI callback failed: {error}"))); tasks.abort_all(); while tasks.join_next().await.is_some() {} queues.clear(); state.cancel(); }
                    Ok(done) => {
                        let owner = done.action.extension.clone();
                        if done.generation == generation.load(Ordering::SeqCst) && (done.action.event.kind == UiEventType::Mount || state.current(&done.action)) {
                            let result = done.result.and_then(|r| state.apply(&owner, r.ui));
                            let succeeded = result.is_ok();
                            match result {
                                Ok(notes) => for note in notes { let _ = events.send(UiEvent::Note(note)); },
                                Err(error) => { let _ = events.send(UiEvent::Note(format!("[{owner}] {error}"))); }
                            }
                            state.finish(&done.action, succeeded);
                        }
                        let mut queue = queues.remove(&owner).unwrap_or_default();
                        while let Some(next) = queue.pop_front() {
                            if state.current(&next) {
                                launch(&mut tasks, extensions[&owner].clone(), next, RequestContext { generation: generation.load(Ordering::SeqCst), ..context.clone() });
                                queues.insert(owner, queue); break;
                            }
                        }
                    }
                }
                publish(&state, &events);
            }
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn widget() -> Vec<UiCommand> {
        serde_json::from_value(json!([{"type":"set_widget","id":"docs","placement":"above_editor","blocks":[{"type":"select","id":"doc","label":"Choose","options":[{"id":"a","label":"A"}]}]}])).unwrap()
    }
    fn select(state: &State, owner: &str) -> UiAction {
        let surface = state
            .snapshot
            .surfaces
            .iter()
            .find(|s| s.extension == owner)
            .unwrap();
        UiAction {
            extension: owner.into(),
            event: UiInputEvent {
                kind: UiEventType::Select,
                event_id: "click".into(),
                surface: Some(SurfaceKind::Widget),
                surface_id: Some("docs".into()),
                revision: surface.revision,
                control_id: Some("doc".into()),
                value: json!("a"),
            },
        }
    }
    #[test]
    fn routes_only_current_valid_controls_and_keeps_owners_isolated() {
        let mut state = State::default();
        state.apply("one", widget()).unwrap();
        state.apply("two", widget()).unwrap();
        let event = select(&state, "one");
        let mut forged = event.clone();
        forged.event.value = json!("missing");
        assert!(state.accept(&forged).is_err());
        state.accept(&event).unwrap();
        assert!(state.accept(&event).is_err(), "double click accepted");
        state.apply("one", widget()).unwrap();
        assert!(
            !state.current(&event),
            "old callback can overwrite new widget"
        );
        assert!(state.accept(&select(&state, "two")).is_ok());
    }
    #[test]
    fn resource_limit_rejects_entire_batch_without_losing_existing_widgets() {
        let mut state = State::default();
        state.apply("one", widget()).unwrap();
        let batch = (0..32)
            .map(|i| UiCommand::SetWidget {
                id: format!("w{i}"),
                placement: Placement::BelowEditor,
                blocks: vec![],
            })
            .collect();
        assert!(state.apply("one", batch).is_err());
        assert_eq!(state.snapshot.surfaces.len(), 1);
        assert_eq!(state.snapshot.surfaces[0].id, "docs");
    }
    #[test]
    fn old_session_action_cannot_target_new_session_with_same_component_ids() {
        let mut old = State::default();
        old.apply("one", widget()).unwrap();
        let stale = select(&old, "one");
        let mut new = State::default();
        new.apply("one", widget()).unwrap();
        assert!(
            new.accept(&stale).is_err(),
            "old session action accepted in new session"
        );
    }
    #[test]
    fn malformed_batch_is_atomic_and_cancel_invalidates_interactions() {
        let mut state = State::default();
        state.apply("one", widget()).unwrap();
        let old = select(&state, "one");
        let mut invalid = widget();
        invalid.push(UiCommand::RemoveWidget { id: "".into() });
        assert!(state.apply("one", invalid).is_err());
        assert!(state.current(&old));
        state.cancel();
        assert!(state.accept(&old).is_err());
        assert_eq!(state.snapshot.surfaces.len(), 1);
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use tokio::time::{Duration, timeout};
    async fn snapshot(
        rx: &mut mpsc::UnboundedReceiver<UiEvent>,
        predicate: impl Fn(&Snapshot) -> bool,
    ) -> Snapshot {
        timeout(Duration::from_secs(5), async {
            loop {
                if let Some(UiEvent::ExtensionUi(s)) = rx.recv().await
                    && predicate(&s)
                {
                    return s;
                }
            }
        })
        .await
        .expect("expected UI update")
    }
    fn action(
        s: &Surface,
        kind: UiEventType,
        control: Option<&str>,
        value: serde_json::Value,
    ) -> UiAction {
        UiAction {
            extension: s.extension.clone(),
            event: UiInputEvent {
                kind,
                event_id: String::new(),
                surface: Some(s.kind),
                surface_id: Some(s.id.clone()),
                revision: s.revision,
                control_id: control.map(str::to_owned),
                value,
            },
        }
    }
    #[tokio::test]
    async fn real_process_mount_select_input_confirm_and_cancel() {
        let extensions = crate::extensions::load(
            &ExtensionsConfig {
                manifests: vec![
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                        .join("examples/extensions/interactive/extension.toml"),
                ],
                ..Default::default()
            },
            Vec::new(),
        )
        .unwrap();
        let (events, mut rx) = mpsc::unbounded_channel();
        let host = Host::start("test-session".into(), extensions.ui_extensions(), events);
        let first = snapshot(&mut rx, |s| !s.surfaces.is_empty()).await;
        host.action(action(
            &first.surfaces[0],
            UiEventType::Select,
            Some("document"),
            serde_json::json!("develop"),
        ));
        let selected = snapshot(&mut rx, |s| {
            s.surfaces.iter().any(|s| {
                s.blocks
                    .iter()
                    .any(|b| matches!(b, UiBlock::Markdown { text } if text.contains("JSON")))
            })
        })
        .await;
        host.action(action(
            &selected.surfaces[0],
            UiEventType::Submit,
            Some("message"),
            serde_json::json!("hello 中文"),
        ));
        let submitted = snapshot(&mut rx, |s| s.surfaces.iter().any(|s| s.id == "message")).await;
        assert!(submitted.surfaces.iter().any(|s| {
            s.blocks
                .iter()
                .any(|b| matches!(b, UiBlock::Text { text } if text.contains("hello 中文")))
        }));
        let form = submitted.surfaces.iter().find(|s| s.id == "demo").unwrap();
        host.action(action(
            form,
            UiEventType::Click,
            Some("confirm"),
            serde_json::Value::Null,
        ));
        let opened = snapshot(&mut rx, |s| {
            s.surfaces.iter().any(|s| s.kind == SurfaceKind::Dialog)
        })
        .await;
        let dialog = opened
            .surfaces
            .iter()
            .find(|s| s.kind == SurfaceKind::Dialog)
            .unwrap();
        host.action(action(
            dialog,
            UiEventType::Confirm,
            None,
            serde_json::json!(true),
        ));
        let confirmed =
            snapshot(&mut rx, |s| s.statuses.iter().any(|s| s.text == "确认完成")).await;
        assert!(
            confirmed
                .surfaces
                .iter()
                .all(|s| s.kind != SurfaceKind::Dialog)
        );
        host.action(action(
            confirmed.surfaces.iter().find(|s| s.id == "demo").unwrap(),
            UiEventType::Click,
            Some("confirm"),
            serde_json::Value::Null,
        ));
        let opened = snapshot(&mut rx, |s| {
            s.surfaces.iter().any(|s| s.kind == SurfaceKind::Dialog)
        })
        .await;
        host.action(action(
            opened
                .surfaces
                .iter()
                .find(|s| s.kind == SurfaceKind::Dialog)
                .unwrap(),
            UiEventType::Cancel,
            None,
            serde_json::Value::Null,
        ));
        snapshot(&mut rx, |s| s.statuses.iter().any(|s| s.text == "操作取消")).await;
        host.shutdown().await;
    }
    struct Blocking {
        entered: Arc<tokio::sync::Notify>,
    }

    struct FailingConfirmation {
        attempts: std::sync::atomic::AtomicUsize,
    }
    impl Extension for FailingConfirmation {
        fn name(&self) -> &str {
            "confirm-retry"
        }
        fn ui_enabled(&self) -> bool {
            true
        }
        fn hook<'a>(&'a self, _: Stage, _: &'a serde_json::Value) -> ExtensionFuture<'a> {
            Box::pin(async { Ok(Response::default()) })
        }
        fn ui_event<'a>(&'a self, event: &'a UiInputEvent) -> ExtensionFuture<'a> {
            Box::pin(async move {
                if event.kind == UiEventType::Mount {
                    return Ok(Response {
                        ui: vec![UiCommand::OpenDialog {
                            id: "confirm".into(),
                            dialog: Dialog::Confirm {
                                title: "Confirm".into(),
                                text: "Continue?".into(),
                            },
                        }],
                        ..Default::default()
                    });
                }
                if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                    Err("temporary failure".into())
                } else {
                    Ok(Response::default())
                }
            })
        }
    }

    #[tokio::test]
    async fn failed_confirmation_stays_enabled_for_explicit_retry() {
        let mut extensions = Extensions::default();
        extensions
            .register(Arc::new(FailingConfirmation {
                attempts: std::sync::atomic::AtomicUsize::new(0),
            }))
            .unwrap();
        let (events, mut rx) = mpsc::unbounded_channel();
        let host = Host::start("retry".into(), extensions.ui_extensions(), events);
        let first = snapshot(&mut rx, |s| !s.surfaces.is_empty()).await;
        let original = action(
            &first.surfaces[0],
            UiEventType::Confirm,
            None,
            serde_json::json!(true),
        );
        host.action(original.clone());
        snapshot(&mut rx, |s| s.surfaces.iter().any(|s| s.pending)).await;
        let failed = snapshot(&mut rx, |s| s.surfaces.iter().all(|s| !s.pending)).await;
        assert_eq!(
            failed.surfaces.len(),
            1,
            "failed confirmation must remain retryable"
        );
        assert_ne!(failed.surfaces[0].revision, original.event.revision);
        host.action(action(
            &failed.surfaces[0],
            UiEventType::Confirm,
            None,
            serde_json::json!(true),
        ));
        snapshot(&mut rx, |s| s.surfaces.is_empty()).await;
        host.shutdown().await;
    }

    impl Extension for Blocking {
        fn name(&self) -> &str {
            "blocking"
        }
        fn ui_enabled(&self) -> bool {
            true
        }
        fn hook<'a>(&'a self, _: Stage, _: &'a serde_json::Value) -> ExtensionFuture<'a> {
            Box::pin(async { Ok(Response::default()) })
        }
        fn ui_event<'a>(&'a self, e: &'a UiInputEvent) -> ExtensionFuture<'a> {
            Box::pin(async move {
                if e.kind == UiEventType::Mount {
                    Ok(Response {
                        ui: vec![UiCommand::SetWidget {
                            id: "button".into(),
                            placement: Placement::AboveEditor,
                            blocks: vec![UiBlock::Button {
                                id: "run".into(),
                                label: "Run".into(),
                            }],
                        }],
                        ..Default::default()
                    })
                } else {
                    self.entered.notify_one();
                    std::future::pending().await
                }
            })
        }
    }
    #[tokio::test]
    async fn cancel_releases_pending_callbacks_and_rejects_old_generation_updates() {
        let entered = Arc::new(tokio::sync::Notify::new());
        let mut extensions = Extensions::default();
        extensions
            .register(Arc::new(Blocking {
                entered: entered.clone(),
            }))
            .unwrap();
        let (events, mut rx) = mpsc::unbounded_channel();
        let host = Host::start("cancel".into(), extensions.ui_extensions(), events);
        let old_context = host.context();
        let first = snapshot(&mut rx, |s| !s.surfaces.is_empty()).await;
        host.action(action(
            &first.surfaces[0],
            UiEventType::Click,
            Some("run"),
            serde_json::Value::Null,
        ));
        timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        timeout(Duration::from_secs(2), host.cancel())
            .await
            .unwrap();
        let cleared = snapshot(&mut rx, |s| {
            !s.surfaces.is_empty()
                && !s.surfaces[0].pending
                && s.surfaces[0].revision != first.surfaces[0].revision
        })
        .await;
        old_context
            .updates
            .unwrap()
            .send(UiDelivery {
                extension: "blocking".into(),
                generation: old_context.generation,
                commands: vec![UiCommand::RemoveWidget {
                    id: "button".into(),
                }],
            })
            .unwrap();
        // A new valid event still routes after cancel; stale delivery cannot remove it.
        host.action(action(
            &cleared.surfaces[0],
            UiEventType::Click,
            Some("run"),
            serde_json::Value::Null,
        ));
        timeout(Duration::from_secs(2), entered.notified())
            .await
            .unwrap();
        timeout(Duration::from_secs(2), host.shutdown())
            .await
            .unwrap();
    }
}
