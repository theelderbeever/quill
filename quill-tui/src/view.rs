//! Navigation levels. Enter pushes a view, Esc pops it.
mod connections;
mod queries;
pub mod workspace;

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Frame, layout::Rect, text::Line};

use crate::{
    component::{Component, Context, KeyHint, KeyOutcome},
    connection::{ConnectionId, Target},
    event::Msg,
    filter::Filter,
    store::queries::QueryName,
    widget::palette::{Flash, Mode},
};
use connections::ConnectionsView;
use queries::QueriesView;
use workspace::Workspace;

/// Where a view sits in the navigation hierarchy.
pub enum Route {
    Connections(Option<Filter>),
    Queries(ConnectionId),
    Workspace {
        connection: ConnectionId,
        query: QueryName,
    },
}

impl Route {
    /// Where a resolved bare name leads: its queries, or the connection
    /// list narrowed to every match.
    pub fn target(target: Option<Target>) -> Self {
        match target {
            None => Self::Connections(None),
            Some(Target::One(id)) => Self::Queries(id),
            Some(Target::Many(filter)) => Self::Connections(Some(filter)),
        }
    }

    pub const fn connection(&self) -> Option<&ConnectionId> {
        match self {
            Self::Connections(_) => None,
            Self::Queries(connection) | Self::Workspace { connection, .. } => Some(connection),
        }
    }
}

pub enum View {
    Connections(ConnectionsView),
    Queries(QueriesView),
    Workspace(Box<Workspace>),
}

impl View {
    pub fn open(route: Route, ctx: &Context) -> anyhow::Result<Self> {
        Ok(match route {
            Route::Connections(filter) => Self::Connections(ConnectionsView::new(filter)),
            Route::Queries(connection) => Self::Queries(QueriesView::open(connection, ctx)?),
            Route::Workspace { connection, query } => {
                Self::Workspace(Box::new(Workspace::open(connection, query, ctx)?))
            }
        })
    }

    pub fn connection(&self) -> Option<&ConnectionId> {
        match self {
            Self::Connections(_) => None,
            Self::Queries(view) => Some(view.connection()),
            Self::Workspace(view) => Some(view.connection()),
        }
    }

    /// Path to this view, e.g. `pg_prod › daily_revenue`.
    pub fn breadcrumb(&self) -> impl Iterator<Item = &str> {
        let query = match self {
            Self::Workspace(view) => Some(view.query().as_str()),
            _ => None,
        };
        self.connection()
            .map(ConnectionId::name)
            .into_iter()
            .chain(query)
    }

    /// What `/` edits here, with the current text: a live list filter or
    /// a results condition.
    pub fn filter_mode(&self) -> Option<(Mode, &str)> {
        match self {
            Self::Connections(_) | Self::Queries(_) => Some((Mode::Filter, "")),
            Self::Workspace(view) => view.filter().map(|text| (Mode::Where, text)),
        }
    }

    /// Narrow a list view. Returns false for views without a list.
    pub fn set_filter(&mut self, filter: Option<Filter>) -> bool {
        match self {
            Self::Connections(view) => view.set_filter(filter),
            Self::Queries(view) => view.set_filter(filter),
            Self::Workspace(_) => return false,
        }
        true
    }

    /// Flush state before the view is left: saves an open query.
    pub fn close(&mut self) -> anyhow::Result<()> {
        match self {
            Self::Workspace(view) => view.close(),
            _ => Ok(()),
        }
    }

    /// Called on each redraw tick. Returns whether the screen should redraw.
    pub fn tick(&mut self, ctx: &Context) -> anyhow::Result<bool> {
        match self {
            Self::Workspace(view) => view.tick(ctx),
            _ => Ok(false),
        }
    }

    /// Apply a background update. Returns a message to show, if any.
    pub fn handle_msg(&mut self, msg: Msg, ctx: &Context) -> Option<Flash> {
        match self {
            Self::Workspace(view) => view.handle_msg(msg, ctx),
            _ => None,
        }
    }

    /// Shown on the palette line while the palette is closed.
    pub fn status(&self) -> Option<Line<'_>> {
        match self {
            Self::Workspace(view) => view.status(),
            _ => None,
        }
    }

    fn component(&mut self) -> &mut dyn Component {
        match self {
            Self::Connections(view) => view,
            Self::Queries(view) => view,
            Self::Workspace(view) => view.as_mut(),
        }
    }
}

impl Component for View {
    fn handle_key(&mut self, key: KeyEvent, ctx: &Context) -> KeyOutcome {
        self.component().handle_key(key, ctx)
    }

    fn handle_mouse(&mut self, event: MouseEvent, ctx: &Context) -> KeyOutcome {
        self.component().handle_mouse(event, ctx)
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Context) {
        self.component().render(frame, area, ctx);
    }

    fn hints(&self) -> &'static [KeyHint] {
        match self {
            Self::Connections(view) => view.hints(),
            Self::Queries(view) => view.hints(),
            Self::Workspace(view) => view.hints(),
        }
    }

    fn copy_text(&mut self) -> Option<String> {
        self.component().copy_text()
    }

    fn captures_input(&self) -> bool {
        match self {
            Self::Connections(view) => view.captures_input(),
            Self::Queries(view) => view.captures_input(),
            Self::Workspace(view) => view.captures_input(),
        }
    }
}

/// Open views, bottom to top. The connection list is always at the bottom.
pub struct ViewStack {
    root: View,
    above: Vec<View>,
}

impl ViewStack {
    pub fn new() -> Self {
        Self {
            root: View::Connections(ConnectionsView::new(None)),
            above: Vec::new(),
        }
    }

    pub fn top(&self) -> &View {
        self.above.last().unwrap_or(&self.root)
    }

    pub fn top_mut(&mut self) -> &mut View {
        self.above.last_mut().unwrap_or(&mut self.root)
    }

    pub fn push(&mut self, view: View) {
        self.above.push(view);
    }

    /// Remove the top view, leaving the connection list.
    pub fn pop(&mut self) -> Option<View> {
        self.above.pop()
    }

    /// Back to the connection list, narrowed by `filter`. Returns the
    /// removed views, top first, so the caller can close them.
    pub fn reset(&mut self, filter: Option<Filter>) -> Vec<View> {
        self.root.set_filter(filter);
        let mut removed = std::mem::take(&mut self.above);
        removed.reverse();
        removed
    }
}
