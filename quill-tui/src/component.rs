//! The shape shared by views and workspace panels.
use std::path::PathBuf;

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Frame, layout::Rect};
use tokio::sync::mpsc::UnboundedSender;

use crate::{Options, action::Action, connection::Connections, event::Msg};

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

/// Session state every component may read.
pub struct Shared {
    pub connections: Connections,
    pub options: Options,
    /// Per-session directory for drained results, removed on exit.
    pub results_dir: PathBuf,
    /// Background tasks report back through this.
    pub messages: UnboundedSender<Msg>,
}

impl Shared {
    pub const fn context(&self, focused: bool) -> Context<'_> {
        Context {
            shared: self,
            focused,
        }
    }
}

#[derive(Clone, Copy)]
pub struct Context<'a> {
    pub shared: &'a Shared,
    pub focused: bool,
}

impl Context<'_> {
    pub const fn connections(&self) -> &Connections {
        &self.shared.connections
    }

    pub const fn options(&self) -> &Options {
        &self.shared.options
    }

    pub const fn with_focus(self, focused: bool) -> Self {
        Self { focused, ..self }
    }
}

pub trait Component {
    fn handle_key(&mut self, key: KeyEvent, ctx: &Context) -> KeyOutcome;

    /// Mouse events over this component's last drawn area.
    fn handle_mouse(&mut self, _event: MouseEvent, _ctx: &Context) -> KeyOutcome {
        KeyOutcome::Ignored
    }

    /// Takes `&mut self` so stateful widgets can update scroll offsets.
    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Context);

    /// Keys this component handles, shown in the header while it has focus.
    fn hints(&self) -> &'static [KeyHint];

    /// Text the user has highlighted here, for Ctrl-Shift-C.
    fn copy_text(&mut self) -> Option<String> {
        None
    }

    /// While true, every key goes to this component, including the app's
    /// reserved and fallback bindings. Esc must release capture.
    fn captures_input(&self) -> bool {
        false
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use std::{num::NonZeroU64, time::Duration};

    use tokio::sync::mpsc::{self, UnboundedReceiver};

    use super::*;

    pub(crate) fn options() -> Options {
        Options {
            redraw: Duration::from_millis(100),
            autosave: Duration::from_millis(750),
            results_dir: std::env::temp_dir(),
            byte_limit: None,
            page_rows: NonZeroU64::MIN.saturating_add(99),
            max_column_width: 40,
        }
    }

    pub(crate) fn shared(connections: Connections) -> (Shared, UnboundedReceiver<Msg>) {
        let (messages, received) = mpsc::unbounded_channel();
        let shared = Shared {
            connections,
            options: options(),
            results_dir: std::env::temp_dir(),
            messages,
        };
        (shared, received)
    }
}
