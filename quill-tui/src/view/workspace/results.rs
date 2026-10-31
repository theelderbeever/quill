use crossterm::event::KeyEvent;
use ratatui::{Frame, layout::Rect};

use crate::{
    component::{Component, Context, KeyHint, KeyOutcome},
    widget::panel,
};

pub struct Results;

impl Component for Results {
    fn handle_key(&mut self, _key: KeyEvent, _ctx: &Context) -> KeyOutcome {
        KeyOutcome::Ignored
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Context) {
        panel::placeholder(
            frame,
            area,
            "results",
            ctx.focused,
            "Results are not available yet.",
        );
    }

    fn hints(&self) -> &'static [KeyHint] {
        &[]
    }
}
