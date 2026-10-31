//! The query editor. Normal mode leaves keys to the app's bindings; insert
//! mode captures every key so typing and editing bindings are unrestricted.
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{Frame, layout::Rect};

use crate::{
    component::{Component, Context, KeyHint, KeyOutcome},
    widget::panel,
};

const NORMAL_HINTS: &[KeyHint] = &[KeyHint::new("i/e", "edit")];
const INSERT_HINTS: &[KeyHint] = &[KeyHint::new("esc", "stop editing")];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Normal,
    Insert,
}

pub struct Editor {
    mode: Mode,
}

impl Editor {
    pub const fn new() -> Self {
        Self { mode: Mode::Normal }
    }
}

impl Component for Editor {
    fn handle_key(&mut self, key: KeyEvent, _ctx: &Context) -> KeyOutcome {
        match (self.mode, key.code) {
            (Mode::Normal, KeyCode::Char('i' | 'e')) => self.mode = Mode::Insert,
            (Mode::Normal, _) => return KeyOutcome::Ignored,
            (Mode::Insert, KeyCode::Esc) => self.mode = Mode::Normal,
            // Text editing arrives with the editor widget.
            (Mode::Insert, _) => {}
        }
        KeyOutcome::Consumed
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Context) {
        let title = match self.mode {
            Mode::Normal => "editor",
            Mode::Insert => "editor -- INSERT --",
        };
        panel::placeholder(
            frame,
            area,
            title,
            ctx.focused,
            "The query editor is not available yet.",
        );
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
}
