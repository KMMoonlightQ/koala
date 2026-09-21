//! Read and encode off the UI thread. Results belong to the original draft,
//! including when a side conversation opens while the clipboard is loading.
use super::*;

pub(super) enum Paste {
    Text(String),
    Image(crate::llm::ImageAttachment),
}
pub(super) type Pending = oneshot::Receiver<Result<Paste, String>>;

fn read() -> Result<Paste, String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    match clipboard.get_image() {
        Ok(image) => {
            crate::images::from_rgba(image.width, image.height, &image.bytes).map(Paste::Image)
        }
        Err(arboard::Error::ContentNotAvailable) => clipboard
            .get_text()
            .map(Paste::Text)
            .map_err(|e| e.to_string()),
        Err(error) => Err(error.to_string()),
    }
}

pub(super) fn start(app: &mut App) {
    if app.clipboard_pending.is_some() {
        return;
    }
    let (send, receive) = oneshot::channel();
    app.clipboard_pending = Some(receive);
    app.hint = Some(i18n::text(app.lang, Key::ClipboardLoading).into());
    tokio::task::spawn_blocking(move || {
        let _ = send.send(read());
    });
}

pub(super) async fn next(pending: &mut Option<Pending>) -> Result<Paste, String> {
    match pending {
        Some(receiver) => receiver
            .await
            .unwrap_or_else(|_| Err("Clipboard worker stopped".into())),
        None => std::future::pending().await,
    }
}

pub(super) fn apply(app: &mut App, result: Result<Paste, String>) {
    app.clipboard_pending = None;
    app.images.sync(&app.input);
    let result = match result {
        Ok(Paste::Image(_)) if app.images.len() >= crate::images::MAX_IMAGES => {
            Err(i18n::text(app.lang, Key::ImageLimit).into())
        }
        other => other,
    };
    match result {
        Ok(Paste::Image(image)) => {
            app.images.insert(&mut app.input, image);
            app.hint = Some(i18n::text(app.lang, Key::ImageKeys).into());
        }
        Ok(Paste::Text(text)) => {
            app.input
                .insert_str(text::clean(&text.replace("\r\n", "\n").replace('\r', "\n")));
            app.history.reset_navigation();
            app.menu_dismissed = true;
            app.hint = None;
        }
        Err(error) => {
            app.hint = Some(i18n::fill(
                app.lang,
                Key::ClipboardFailed,
                &[("error", &error)],
            ))
        }
    }
    app.images.sync(&app.input);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paste_result_limits_attachments_without_changing_text_or_sending() {
        let (session, mut commands) = SessionHandle::test_channel();
        let mut app = App::new(session);
        app.input.insert_str("draft");
        let image = crate::images::from_rgba(1, 1, &[0, 0, 0, 255]).unwrap();
        for _ in 0..5 {
            apply(&mut app, Ok(Paste::Image(image.clone())));
        }
        assert_eq!(app.images.len(), 4);
        assert_eq!(
            app.input.lines(),
            &["draft [image 1] [image 2] [image 3] [image 4]"]
        );
        assert!(commands.try_recv().is_err());
        apply(&mut app, Err("clipboard unavailable".into()));
        assert_eq!(app.images.len(), 4);
        assert!(app.hint.as_ref().unwrap().contains("clipboard unavailable"));
        apply(&mut app, Ok(Paste::Text("\r\nsecond".into())));
        assert_eq!(
            app.input.lines(),
            &["draft [image 1] [image 2] [image 3] [image 4]", "second"]
        );
    }
}
