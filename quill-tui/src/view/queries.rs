//! Saved queries for one connection. Until saved queries exist, this lists a
//! single scratch query so the workspace is reachable.
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    widgets::{List, ListState},
};

use super::Route;
use crate::{
    action::Action,
    component::{Component, Context, KeyHint, KeyOutcome},
    connection::ConnectionName,
    widget::{panel, theme},
};

const HINTS: &[KeyHint] = &[KeyHint::new("enter", "open")];
const SCRATCH: &str = "scratch";

pub struct QueriesView {
    connection: ConnectionName,
    list: ListState,
}

impl QueriesView {
    pub fn new(connection: ConnectionName) -> Self {
        Self {
            connection,
            list: ListState::default().with_selected(Some(0)),
        }
    }

    pub const fn connection(&self) -> &ConnectionName {
        &self.connection
    }
}

impl Component for QueriesView {
    fn handle_key(&mut self, key: KeyEvent, _ctx: &Context) -> KeyOutcome {
        match key.code {
            KeyCode::Enter => KeyOutcome::Action(Action::Open(Route::Workspace {
                connection: self.connection.clone(),
                query: SCRATCH.to_owned(),
            })),
            _ => KeyOutcome::Ignored,
        }
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Context) {
        let list = List::new([SCRATCH])
            .highlight_style(theme::SELECTED)
            .block(panel::block(
                &format!("{} queries", self.connection),
                ctx.focused,
            ));
        frame.render_stateful_widget(list, area, &mut self.list);
    }

    fn hints(&self) -> &'static [KeyHint] {
        HINTS
    }
}
