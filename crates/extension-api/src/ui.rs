//! Declarative UI wire types. No frontend or executor dependencies.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

pub const MAX_TEXT: usize = 16 * 1024;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    AboveEditor,
    BelowEditor,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceKind {
    Widget,
    Dialog,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Info,
    Warning,
    Error,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectOption {
    pub id: String,
    pub label: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiBlock {
    Text {
        text: String,
    },
    Markdown {
        text: String,
    },
    Button {
        id: String,
        label: String,
    },
    Select {
        id: String,
        label: String,
        options: Vec<SelectOption>,
    },
    Input {
        id: String,
        label: String,
        #[serde(default)]
        value: String,
    },
}
impl UiBlock {
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Button { id, .. } | Self::Select { id, .. } | Self::Input { id, .. } => Some(id),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Dialog {
    Confirm { title: String, text: String },
    Form { title: String, blocks: Vec<UiBlock> },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiCommand {
    Notify {
        level: Level,
        text: String,
    },
    SetStatus {
        id: String,
        text: String,
    },
    RemoveStatus {
        id: String,
    },
    SetWidget {
        id: String,
        placement: Placement,
        blocks: Vec<UiBlock>,
    },
    RemoveWidget {
        id: String,
    },
    OpenDialog {
        id: String,
        dialog: Dialog,
    },
    CloseDialog {
        id: String,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UiEventType {
    Mount,
    Click,
    Select,
    Submit,
    Confirm,
    Cancel,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UiInputEvent {
    #[serde(rename = "type")]
    pub kind: UiEventType,
    pub event_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<SurfaceKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface_id: Option<String>,
    pub revision: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub control_id: Option<String>,
    #[serde(default)]
    pub value: Value,
}

pub fn validate_ui(commands: &[UiCommand]) -> Result<(), String> {
    if commands.len() > 64
        || serde_json::to_vec(commands)
            .map_err(|e| e.to_string())?
            .len()
            > 256 * 1024
    {
        return Err("UI batch exceeds limit".into());
    }
    for command in commands {
        match command {
            UiCommand::Notify { text, .. } => check_text(text)?,
            UiCommand::SetStatus { id, text } => {
                check_id(id)?;
                check_text(text)?;
            }
            UiCommand::RemoveStatus { id }
            | UiCommand::RemoveWidget { id }
            | UiCommand::CloseDialog { id } => check_id(id)?,
            UiCommand::SetWidget { id, blocks, .. } => {
                check_id(id)?;
                check_blocks(blocks)?;
            }
            UiCommand::OpenDialog { id, dialog } => {
                check_id(id)?;
                match dialog {
                    Dialog::Confirm { title, text } => {
                        check_text(title)?;
                        check_text(text)?;
                    }
                    Dialog::Form { title, blocks } => {
                        check_text(title)?;
                        check_blocks(blocks)?;
                    }
                }
            }
        }
    }
    Ok(())
}
fn check_text(text: &str) -> Result<(), String> {
    if text.len() > MAX_TEXT {
        Err("UI text exceeds 16 KiB".into())
    } else {
        Ok(())
    }
}
fn check_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 128 || id.chars().any(char::is_control) {
        Err("invalid UI id".into())
    } else {
        Ok(())
    }
}
fn check_blocks(blocks: &[UiBlock]) -> Result<(), String> {
    if blocks.len() > 64 {
        return Err("too many UI blocks".into());
    }
    let mut ids = HashSet::new();
    for block in blocks {
        if let Some(id) = block.id() {
            check_id(id)?;
            if !ids.insert(id) {
                return Err("duplicate UI control id".into());
            }
        }
        match block {
            UiBlock::Text { text } | UiBlock::Markdown { text } => check_text(text)?,
            UiBlock::Button { label, .. } => check_text(label)?,
            UiBlock::Input { label, value, .. } => {
                check_text(label)?;
                check_text(value)?;
            }
            UiBlock::Select { label, options, .. } => {
                check_text(label)?;
                if options.is_empty() || options.len() > 256 {
                    return Err("select requires 1..256 options".into());
                }
                let mut options_seen = HashSet::new();
                for option in options {
                    check_id(&option.id)?;
                    check_text(&option.label)?;
                    if !options_seen.insert(&option.id) {
                        return Err("duplicate option id".into());
                    }
                }
            }
        }
    }
    Ok(())
}
