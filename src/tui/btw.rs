use super::*;

/// The main App stays live underneath; its transcript, draft and cursor never move.
pub(super) struct Conversation {
    pub app: Box<App>,
    events: Option<mpsc::UnboundedReceiver<UiEvent>>,
}

impl Drop for Conversation {
    fn drop(&mut self) {
        self.app.session.send(SessionCommand::Shutdown);
    }
}

pub(super) enum SideEvent {
    Agent(UiEvent),
    Clipboard(Result<clipboard::Paste, String>),
}

pub(super) async fn next_event(side: &mut Option<Conversation>) -> Option<SideEvent> {
    let Some(side) = side else {
        return std::future::pending().await;
    };
    tokio::select! {
        event = async {
            match &mut side.events {
                Some(events) => events.recv().await,
                None => std::future::pending().await,
            }
        } => {
            if event.is_none() { side.events = None; }
            event.map(SideEvent::Agent)
        }
        result = clipboard::next(&mut side.app.clipboard_pending) => Some(SideEvent::Clipboard(result))
    }
}

pub(super) fn open(app: &mut App, question: &str) {
    if app.btw.is_some() || app.restarting {
        return;
    }
    let (handle, events) = app.session.open_btw();
    let mut side = App::new(handle);
    side.temporary = true;
    side.set_lang(app.lang);
    side.theme = app.theme;
    side.directory = app.directory.clone();
    side.model = app.model.clone();
    side.push(EntryKind::Info(i18n::text(app.lang, Key::BtwIntro).into()));
    if !question.is_empty() {
        submit(&mut side, question.to_owned());
    }
    app.btw = Some(Conversation {
        app: Box::new(side),
        events: Some(events),
    });
}

pub(super) fn submit(app: &mut App, text: String) {
    if app.session.is_closed() {
        app.finish();
        app.hint = Some(i18n::text(app.lang, Key::BtwClosed).into());
        return;
    }
    if app.busy {
        app.hint = Some(i18n::text(app.lang, Key::NoteBusyDraftKept).into());
        return;
    }
    // Temporary input never enters the persistent input history, including /btw args.
    app.input = new_input(app.lang);
    app.transcript.scroll(Scroll::End);
    send_draft(app, text);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn disconnected_btw_reports_closure_once_instead_of_staying_busy() {
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = App::new(session);
        open(&mut app, "question");
        let SessionCommand::OpenBtw { events, .. } = commands.try_recv().unwrap() else {
            panic!("open")
        };
        drop(events);
        let event = tokio::time::timeout(Duration::from_millis(100), next_event(&mut app.btw))
            .await
            .expect("closed channels must surface disconnection");
        assert!(event.is_none());
        assert!(
            tokio::time::timeout(Duration::from_millis(10), next_event(&mut app.btw))
                .await
                .is_err(),
            "closed channels must not cause a busy loop"
        );
    }
}
