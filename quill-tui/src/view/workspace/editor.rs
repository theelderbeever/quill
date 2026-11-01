//! The query editor. Normal mode moves the cursor with hjkl and leaves other
//! keys to the app; insert mode captures every key until Esc.
use std::{
    sync::Once,
    time::{Duration, Instant},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    text::Line,
};
use ratatui_code_editor::{
    actions::{Action as EditAction, Delete, InsertText, MoveDown, MoveLeft, MoveRight, MoveUp},
    editor::Editor as CodeEditor,
};

use crate::{
    component::{Component, Context, KeyHint, KeyOutcome},
    widget::{panel, theme},
};

const LANGUAGE: &str = "sql";
const INSERT_MARKER: &str = " -- INSERT -- ";
/// Border columns the insert marker takes from the title.
pub const INSERT_MARKER_WIDTH: u16 = INSERT_MARKER.len() as u16;
const NORMAL_HINTS: &[KeyHint] = &[];
const INSERT_HINTS: &[KeyHint] = &[
    KeyHint::new("esc", "stop editing"),
    KeyHint::new("ctrl-z/y", "undo/redo"),
];

/// Delete the selection, or the character after the cursor.
struct DeleteForward;

impl EditAction for DeleteForward {
    fn apply(&mut self, editor: &mut CodeEditor) {
        if editor.get_selection().is_some_and(|s| !s.is_empty()) {
            Delete.apply(editor);
            return;
        }
        let cursor = editor.get_cursor();
        let code = editor.code_mut();
        let next = code.next_grapheme_boundary(cursor);
        if next == cursor {
            return;
        }
        code.tx();
        code.set_state_before(cursor, None);
        code.remove(cursor, next);
        code.set_state_after(cursor, None);
        code.commit();
        editor.set_cursor(cursor);
        editor.reset_highlight_cache();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Insert,
}

pub struct Editor {
    inner: CodeEditor,
    mode: Mode,
    /// Text area inside the border at the last draw; input maps against it.
    area: Rect,
    /// Last edit not yet saved.
    touched: Option<Instant>,
    saved: String,
}

impl Editor {
    pub fn new(text: String) -> anyhow::Result<Self> {
        static REGISTER: Once = Once::new();
        REGISTER.call_once(|| {
            ratatui_code_editor::register_language(
                LANGUAGE,
                tree_sitter_sequel::LANGUAGE.into(),
                tree_sitter_sequel::HIGHLIGHTS_QUERY,
            );
        });
        Ok(Self {
            inner: CodeEditor::new(LANGUAGE, &text, theme::SQL.to_vec())?,
            mode: Mode::Normal,
            area: Rect::default(),
            touched: None,
            saved: text,
        })
    }

    pub fn insert(&mut self) {
        self.mode = Mode::Insert;
    }

    /// Put `text` at the cursor, or replace the buffer if it is blank.
    pub fn insert_text(&mut self, text: &str) {
        if self.inner.get_content().trim().is_empty() {
            self.inner.set_content(text);
        } else {
            self.inner.apply(InsertText {
                text: format!("\n{text}"),
            });
        }
        self.inner.focus(&self.area);
        self.touched = Some(Instant::now());
    }

    /// The selection if there is one, else the whole buffer.
    pub fn sql_to_run(&mut self) -> String {
        self.inner
            .get_selection_text()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| self.inner.get_content())
    }

    /// Text to save once `delay` has passed since the last edit.
    pub fn due(&mut self, now: Instant, delay: Duration) -> Option<String> {
        let touched = self.touched?;
        (now.duration_since(touched) >= delay)
            .then(|| self.unsaved())
            .flatten()
    }

    /// Text differing from what was last saved, regardless of timing.
    pub fn unsaved(&mut self) -> Option<String> {
        self.touched = None;
        let text = self.inner.get_content();
        (text != self.saved).then_some(text)
    }

    pub fn mark_saved(&mut self, text: String) {
        self.saved = text;
    }

    /// Draw with `title` in the top border; the insert marker sits at its right.
    pub fn render_titled(&mut self, frame: &mut Frame, area: Rect, ctx: &Context, title: Line) {
        let mut block = panel::frame(ctx.focused).title(title);
        if self.mode == Mode::Insert {
            block = block.title_top(Line::styled(INSERT_MARKER, theme::TITLE).right_aligned());
        }
        self.area = block.inner(area);
        frame.render_widget(block, area);
        frame.render_widget(&self.inner, self.area);
        if ctx.focused
            && let Some((x, y)) = self.inner.get_visible_cursor(&self.area)
        {
            frame.set_cursor_position(Position::new(x, y));
        }
    }
}

impl Component for Editor {
    fn handle_key(&mut self, key: KeyEvent, _ctx: &Context) -> KeyOutcome {
        match (self.mode, key.code) {
            (Mode::Normal, KeyCode::Char('h') | KeyCode::Left) => {
                self.inner.apply(MoveLeft { shift: false });
            }
            (Mode::Normal, KeyCode::Char('j') | KeyCode::Down) => {
                self.inner.apply(MoveDown { shift: false });
            }
            (Mode::Normal, KeyCode::Char('k') | KeyCode::Up) => {
                self.inner.apply(MoveUp { shift: false });
            }
            (Mode::Normal, KeyCode::Char('l') | KeyCode::Right) => {
                self.inner.apply(MoveRight { shift: false });
            }
            (Mode::Normal, _) => return KeyOutcome::Ignored,
            (Mode::Insert, KeyCode::Esc) => self.mode = Mode::Normal,
            (Mode::Insert, KeyCode::Delete) => {
                self.inner.apply(DeleteForward);
                self.touched = Some(Instant::now());
            }
            // Copy and cut go through the app's clipboard, which outlives the
            // copy; the widget's own clipboard handle may drop the text on X11.
            (Mode::Insert, KeyCode::Char(c @ ('c' | 'x')))
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let Some(text) = self.copy_text() else {
                    return KeyOutcome::Consumed;
                };
                if c == 'x' {
                    self.inner.apply(Delete);
                    self.touched = Some(Instant::now());
                }
                self.inner.focus(&self.area);
                return KeyOutcome::Action(crate::action::Action::Copy(text));
            }
            (Mode::Insert, _) => {
                if let Err(e) = self.inner.input(key, &self.area) {
                    return KeyOutcome::Action(crate::action::Action::Report(e.to_string()));
                }
                self.touched = Some(Instant::now());
            }
        }
        self.inner.focus(&self.area);
        KeyOutcome::Consumed
    }

    fn handle_mouse(&mut self, event: MouseEvent, _ctx: &Context) -> KeyOutcome {
        match self.inner.mouse(event, &self.area) {
            Ok(()) => KeyOutcome::Consumed,
            Err(e) => KeyOutcome::Action(crate::action::Action::Report(e.to_string())),
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Context) {
        self.render_titled(frame, area, ctx, Line::styled(" editor ", theme::TITLE));
    }

    fn hints(&self) -> &'static [KeyHint] {
        match self.mode {
            Mode::Normal => NORMAL_HINTS,
            Mode::Insert => INSERT_HINTS,
        }
    }

    fn captures_input(&self) -> bool {
        self.mode == Mode::Insert
    }

    fn copy_text(&mut self) -> Option<String> {
        self.inner.get_selection_text().filter(|s| !s.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_removes_the_character_after_the_cursor() {
        let mut editor = Editor::new("    ab".into()).unwrap();
        editor.inner.set_cursor(4);
        editor.inner.apply(DeleteForward);
        assert_eq!(editor.inner.get_content(), "    b");
        assert_eq!(editor.inner.get_cursor(), 4);
        // At the end there is nothing to delete.
        editor.inner.set_cursor(5);
        editor.inner.apply(DeleteForward);
        assert_eq!(editor.inner.get_content(), "    b");
        // Inside indentation, one space goes, not a whole indent.
        editor.inner.set_cursor(0);
        editor.inner.apply(DeleteForward);
        assert_eq!(editor.inner.get_content(), "   b");
    }
}
