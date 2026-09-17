pub use super::event::{TaskState as BgStatus, TaskView as BgTask};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::task::JoinHandle;

#[derive(Debug)]
struct ManagedTask {
    view: BgTask,
    started: Instant,
    handle: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone)]
pub struct BackgroundManager {
    inner: Arc<Mutex<Vec<ManagedTask>>>,
    count: tokio::sync::watch::Sender<usize>,
}

impl Default for BackgroundManager {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::new())),
            count: tokio::sync::watch::channel(0).0,
        }
    }
}

impl BackgroundManager {
    fn changed(&self, tasks: &[ManagedTask]) {
        self.count.send_replace(
            tasks
                .iter()
                .filter(|t| matches!(t.view.status, BgStatus::Running | BgStatus::Stopping))
                .count(),
        );
    }

    pub fn register(&self, kind: &str, description: &str) -> usize {
        let mut tasks = self.inner.lock().unwrap();
        let id = tasks.last().map(|t| t.view.id + 1).unwrap_or(1);
        tasks.push(ManagedTask {
            view: BgTask {
                id,
                kind: kind.into(),
                description: description.into(),
                status: BgStatus::Running,
                output: String::new(),
                elapsed_ms: 0,
            },
            started: Instant::now(),
            handle: None,
        });
        self.changed(&tasks);
        id
    }

    /// Attach before returning the task id to the caller. A fast task may have
    /// finished already; dropping its completed handle is then sufficient.
    pub fn attach(&self, id: usize, handle: JoinHandle<()>) {
        let mut tasks = self.inner.lock().unwrap();
        if let Some(task) = tasks.iter_mut().find(|t| t.view.id == id) {
            if task.view.status == BgStatus::Running {
                task.handle = Some(handle);
            } else if task.view.status == BgStatus::Stopping {
                handle.abort();
            }
        } else {
            handle.abort();
        }
    }

    pub fn finish(&self, id: usize, success: bool, output: String) -> bool {
        let mut tasks = self.inner.lock().unwrap();
        let Some(task) = tasks.iter_mut().find(|t| t.view.id == id) else {
            return false;
        };
        if task.view.status != BgStatus::Running {
            return false;
        }
        task.view.status = if success {
            BgStatus::Done
        } else {
            BgStatus::Failed
        };
        task.view.output = output;
        task.view.elapsed_ms = task.started.elapsed().as_millis() as u64;
        task.handle = None;
        self.changed(&tasks);
        true
    }

    /// Join cancellation before reporting stopped, so owned shell groups have
    /// been dropped and cannot continue side effects after acknowledgement.
    pub async fn stop(&self, id: usize) -> bool {
        let handle = {
            let mut tasks = self.inner.lock().unwrap();
            let Some(task) = tasks.iter_mut().find(|t| t.view.id == id) else {
                return false;
            };
            if task.view.status != BgStatus::Running {
                return false;
            }
            let Some(handle) = task.handle.take() else {
                return false;
            };
            task.view.status = BgStatus::Stopping;
            self.changed(&tasks);
            handle
        };
        handle.abort();
        let _ = handle.await;
        let mut tasks = self.inner.lock().unwrap();
        if let Some(task) = tasks.iter_mut().find(|t| t.view.id == id) {
            task.view.status = BgStatus::Stopped;
            task.view.elapsed_ms = task.started.elapsed().as_millis() as u64;
        }
        self.changed(&tasks);
        true
    }

    pub fn subscribe_count(&self) -> tokio::sync::watch::Receiver<usize> {
        self.count.subscribe()
    }

    pub fn list(&self) -> Vec<BgTask> {
        self.inner
            .lock()
            .unwrap()
            .iter()
            .map(|t| {
                let mut view = t.view.clone();
                if matches!(view.status, BgStatus::Running | BgStatus::Stopping) {
                    view.elapsed_ms = t.started.elapsed().as_millis() as u64;
                }
                view
            })
            .collect()
    }

    pub fn render(&self) -> String {
        self.list()
            .iter()
            .map(|t| {
                format!(
                    "{} #{} [{}] {}",
                    t.status.label(),
                    t.id,
                    t.kind,
                    t.description
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_lifecycle() {
        let mgr = BackgroundManager::default();
        assert!(mgr.render().is_empty());
        let counts = mgr.subscribe_count();
        assert_eq!(*counts.borrow(), 0);
        let id = mgr.register("bash", "sleep 1");
        assert_eq!(id, 1);
        let id2 = mgr.register("task", "research");
        assert_eq!(id2, 2);
        assert_eq!(*counts.borrow(), 2);
        mgr.finish(id, true, "ok".into());
        assert_eq!(*counts.borrow(), 1);
        let tasks = mgr.list();
        assert_eq!(tasks[0].status, BgStatus::Done);
        assert_eq!(tasks[0].output, "ok");
        assert_eq!(tasks[1].status, BgStatus::Running);
        let rendered = mgr.render();
        assert!(rendered.contains("#1 [bash]"));
        assert!(rendered.contains("#2 [task]"));
    }
    #[tokio::test]
    async fn stop_joins_task_before_acknowledgement_and_blocks_late_finish() {
        struct Dropped(std::sync::Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let mgr = BackgroundManager::default();
        let id = mgr.register("task", "pending");
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = dropped.clone();
        let (ready, wait_ready) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            let _guard = Dropped(flag);
            let _ = ready.send(());
            std::future::pending::<()>().await;
        });
        mgr.attach(id, handle);
        wait_ready.await.unwrap();
        assert!(mgr.stop(id).await);
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(mgr.list()[0].status, BgStatus::Stopped);
        assert_eq!(*mgr.subscribe_count().borrow(), 0);
        assert!(!mgr.finish(id, true, "late success".into()));
        assert!(!mgr.stop(id).await);
    }
}
