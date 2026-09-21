use super::{App, Scroll};
use crate::i18n::{self, Key};
use std::collections::VecDeque;

/// Composer-owned inputs are only sent to the session when it is ready.
#[derive(Default)]
pub(super) struct Queue {
    pub pending: VecDeque<Draft>,
    immediate: Option<Draft>,
    pub cancelling: bool,
}

pub(super) struct Draft {
    pub text: String,
    pub images: super::attachments::DraftImages,
}

fn start(app: &mut App, draft: Draft) {
    app.transcript.scroll(Scroll::End);
    // send_draft consumes attachments; preserve the unrelated editor draft.
    let editor_images = std::mem::replace(&mut app.images, draft.images);
    super::send_draft(app, draft.text);
    app.images = editor_images;
}

pub(super) fn submit(app: &mut App, text: String, immediate: bool) {
    let text = Draft {
        text,
        images: std::mem::take(&mut app.images),
    };
    if !app.busy {
        start(app, text);
    } else if immediate {
        // Preserve repeated immediate submissions while cancellation is pending.
        if let Some(previous) = app.queue.immediate.replace(text) {
            app.queue.pending.push_front(previous);
        }
        app.cancel();
    } else {
        app.queue.pending.push_back(text);
    }
}

pub(super) fn completed(app: &mut App) {
    app.finish();
    if app.queue.cancelling {
        // Done may precede the cancellation acknowledgement. Do not dispatch yet.
        app.busy = true;
        app.status = i18n::text(app.lang, Key::StatusInterrupting).into();
    } else if !app.restarting
        && let Some(text) = app.queue.pending.pop_front()
    {
        start(app, text);
    }
}

pub(super) fn cancelled(app: &mut App) {
    app.queue.cancelling = false;
    if !app.restarting
        && let Some(text) = app.queue.immediate.take()
    {
        start(app, text);
    }
    // An ordinary cancellation pauses pending inputs; they remain editable.
}
