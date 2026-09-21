//! Editable image markers. Keep a draft-local registry so editor undo can restore
//! deleted markers, but only markers still in the text become sent attachments.
use crate::llm::ImageAttachment;
use std::collections::{BTreeMap, HashSet};
use std::ops::Range;
use std::sync::LazyLock;
use tui_textarea::{CursorMove, TextArea};

static MARKER: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\[image ([1-9][0-9]*)\]").unwrap());

fn markers(text: &str) -> impl Iterator<Item = (usize, Range<usize>)> + '_ {
    MARKER
        .captures_iter(text)
        .filter_map(|capture| Some((capture[1].parse().ok()?, capture.get(0)?.range())))
}

#[derive(Default)]
pub(super) struct DraftImages {
    stored: BTreeMap<usize, ImageAttachment>,
    active: Vec<usize>,
    next_id: usize,
}

impl DraftImages {
    pub fn len(&self) -> usize {
        self.active.len()
    }
    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn sync(&mut self, editor: &TextArea<'_>) {
        self.active = self.ids(&editor.lines().join("\n"));
    }

    fn ids(&self, text: &str) -> Vec<usize> {
        let mut seen = HashSet::new();
        markers(text)
            .filter_map(|(id, _)| (self.stored.contains_key(&id) && seen.insert(id)).then_some(id))
            .collect()
    }

    pub fn insert(&mut self, editor: &mut TextArea<'_>, image: ImageAttachment) {
        // Never bind a new image to an old or user-typed marker.
        let existing: HashSet<_> = markers(&editor.lines().join("\n"))
            .map(|(id, _)| id)
            .collect();
        self.next_id += 1;
        while existing.contains(&self.next_id) {
            self.next_id += 1;
        }
        let (row, col) = editor.cursor();
        let prefix = if col > 0
            && editor.lines()[row]
                .chars()
                .nth(col - 1)
                .is_some_and(|c| !c.is_whitespace())
        {
            " "
        } else {
            ""
        };
        let suffix = if editor.lines()[row]
            .chars()
            .nth(col)
            .is_some_and(|c| !c.is_whitespace())
        {
            " "
        } else {
            ""
        };
        self.stored.insert(self.next_id, image);
        editor.insert_str(format!("{prefix}[image {}]{suffix}", self.next_id));
        self.sync(editor);
    }

    /// Markers are UI controls, not part of the user's model prompt or text history.
    pub fn text(&self, source: &str) -> String {
        let mut text = source.to_owned();
        let ranges: Vec<_> = markers(source)
            .filter_map(|(id, range)| self.stored.contains_key(&id).then_some(range))
            .collect();
        for range in ranges.into_iter().rev() {
            text.replace_range(range, "");
        }
        text.trim().to_owned()
    }

    pub fn take_message(&mut self, source: &str) -> (String, Vec<ImageAttachment>) {
        let mut draft = std::mem::take(self);
        let text = draft.text(source);
        let images = draft
            .ids(source)
            .into_iter()
            .filter_map(|id| draft.stored.remove(&id))
            .collect();
        (text, images)
    }

    /// Historical messages store real text plus images. Recreate editable markers.
    pub fn restore(&mut self, editor: &mut TextArea<'_>, images: Vec<ImageAttachment>) {
        self.clear();
        editor.move_cursor(CursorMove::Bottom);
        editor.move_cursor(CursorMove::End);
        for image in images {
            self.insert(editor, image);
        }
    }

    /// Backspace/Delete adjacent to or inside a marker removes it in one edit.
    /// Selections use the editor's normal deletion and undo behavior.
    pub fn select_for_delete(&self, editor: &mut TextArea<'_>, backwards: bool) {
        if editor.is_selecting() {
            return;
        }
        let (row, col) = editor.cursor();
        let line = &editor.lines()[row];
        let span = markers(line)
            .filter(|(id, _)| self.stored.contains_key(id))
            .find_map(|(_, range)| {
                let start = line[..range.start].chars().count();
                let end = start + line[range].chars().count();
                let hit = if backwards {
                    col > start && col <= end
                } else {
                    col >= start && col < end
                };
                hit.then_some((start, end))
            });
        if let Some((start, end)) = span {
            // CursorMove::Jump is limited to u16. At longer lines, normal editor
            // deletion still removes the marker's attachment through sync().
            if let (Ok(row), Ok(start), Ok(end)) =
                (u16::try_from(row), u16::try_from(start), u16::try_from(end))
            {
                editor.move_cursor(CursorMove::Jump(row, start));
                editor.start_selection();
                editor.move_cursor(CursorMove::Jump(row, end));
            }
        }
    }
}
