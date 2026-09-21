//! Scoped request metadata and validated, source-attributed UI delivery.
use crate::*;
use std::future::Future;
use tokio::sync::mpsc;

#[derive(Debug)]
pub struct UiDelivery {
    pub extension: String,
    pub commands: Vec<UiCommand>,
    pub generation: u64,
}
#[derive(Clone, Default)]
pub struct RequestContext {
    pub session_id: String,
    pub ui: bool,
    pub generation: u64,
    pub updates: Option<mpsc::UnboundedSender<UiDelivery>>,
}
tokio::task_local! { static CONTEXT: RequestContext; }
pub async fn scope<T>(context: RequestContext, future: impl Future<Output = T>) -> T {
    CONTEXT.scope(context, future).await
}
pub fn current_context() -> RequestContext {
    CONTEXT.try_with(Clone::clone).unwrap_or_default()
}

pub(crate) struct Hosted(pub Arc<dyn Extension>);
impl Hosted {
    fn response(
        &self,
        response: Response,
        stage: Option<Stage>,
        callback: bool,
    ) -> Result<Response, String> {
        if !callback && stage.is_none() && response.content.is_none() {
            return Err("extension tool returned no content".into());
        }
        if let Some(reason) = &response.block {
            return Err(format!("{} blocked: {reason}", self.name()));
        }
        if response
            .arguments
            .as_ref()
            .is_some_and(|a| stage != Some(Stage::PreToolUse) || !a.is_object())
        {
            return Err("arguments only allowed as object in pre_tool_use".into());
        }
        if response.content.is_some()
            && (callback || stage.is_some_and(|s| s != Stage::PostToolUse))
        {
            return Err("content only allowed in tool or post_tool_use response".into());
        }
        if callback && (response.context.is_some() || response.is_error) {
            return Err("ui_event response may only contain UI commands".into());
        }
        if !response.ui.is_empty() && !self.ui_enabled() {
            return Err("extension has not enabled v2 UI".into());
        }
        validate_ui(&response.ui)?;
        if !callback && !response.ui.is_empty() {
            let context = current_context();
            if context.ui
                && let Some(updates) = context.updates
            {
                let _ = updates.send(UiDelivery {
                    extension: self.name().into(),
                    commands: response.ui.clone(),
                    generation: context.generation,
                });
            }
        }
        Ok(response)
    }
}
impl Extension for Hosted {
    fn name(&self) -> &str {
        self.0.name()
    }
    fn ui_enabled(&self) -> bool {
        self.0.ui_enabled()
    }
    fn tools(&self) -> Vec<Tool> {
        self.0.tools()
    }
    fn read_only(&self, name: &str) -> bool {
        self.0.read_only(name)
    }
    fn hook<'a>(&'a self, stage: Stage, payload: &'a Value) -> ExtensionFuture<'a> {
        Box::pin(
            async move { self.response(self.0.hook(stage, payload).await?, Some(stage), false) },
        )
    }
    fn execute<'a>(&'a self, name: &'a str, args: &'a Value) -> ExtensionFuture<'a> {
        Box::pin(async move { self.response(self.0.execute(name, args).await?, None, false) })
    }
    fn ui_event<'a>(&'a self, event: &'a UiInputEvent) -> ExtensionFuture<'a> {
        Box::pin(async move {
            if !self.ui_enabled() {
                return Err("extension has not enabled UI".into());
            }
            self.response(self.0.ui_event(event).await?, None, true)
        })
    }
}
