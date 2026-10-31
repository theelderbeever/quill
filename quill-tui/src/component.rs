//! The shape shared by views and workspace panels.
use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Frame, layout::Rect};

use crate::{action::Action, connection::Connections};

/// One entry in the header's hotkey column.
pub struct KeyHint {
    pub key: &'static str,
    pub action: &'static str,
}

impl KeyHint {
    pub const fn new(key: &'static str, action: &'static str) -> Self {
        Self { key, action }
    }
}

pub enum KeyOutcome {
    Consumed,
    /// Not handled; the app may apply its fallback binding.
    Ignored,
    Action(Action),
}

/// App state a component reads while handling input or drawing.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub connections: &'a Connections,
    pub focused: bool,
}

pub trait Component {
    fn handle_key(&mut self, key: KeyEvent, ctx: &Context) -> KeyOutcome;

    /// Takes `&mut self` so stateful widgets can update scroll offsets.
    /// Mouse events over this component's last drawn area.
    fn handle_mouse(&mut self, _event: MouseEvent, _ctx: &Context) -> KeyOutcome {
        KeyOutcome::Ignored
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Context);

    /// Keys this component handles, shown in the header while it has focus.
    fn hints(&self) -> &'static [KeyHint];

    /// While true, every key goes to this component, including the app's
    /// reserved and fallback bindings. Esc must release capture.
    fn captures_input(&self) -> bool {
        false
    }
}
