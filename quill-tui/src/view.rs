//! Navigation levels. Enter pushes a view, Esc pops it.
mod connections;
mod queries;
pub mod workspace;

use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Frame, layout::Rect};

use crate::{
    component::{Component, Context, KeyHint, KeyOutcome},
    connection::ConnectionName,
};
use connections::ConnectionsView;
use queries::QueriesView;
use workspace::Workspace;

/// Where a view sits in the navigation hierarchy.
pub enum Route {
    Connections,
    Queries(ConnectionName),
    Workspace {
        connection: ConnectionName,
        query: String,
    },
}

impl Route {
    pub const fn connection(&self) -> Option<&ConnectionName> {
        match self {
            Self::Connections => None,
            Self::Queries(connection) | Self::Workspace { connection, .. } => Some(connection),
        }
    }
}

pub enum View {
    Connections(ConnectionsView),
    Queries(QueriesView),
    Workspace(Workspace),
}

impl View {
    pub fn open(route: Route) -> Self {
        match route {
            Route::Connections => Self::Connections(ConnectionsView::new()),
            Route::Queries(connection) => Self::Queries(QueriesView::new(connection)),
            Route::Workspace { connection, query } => {
                Self::Workspace(Workspace::new(connection, query))
            }
        }
    }

    pub fn connection(&self) -> Option<&ConnectionName> {
        match self {
            Self::Connections(_) => None,
            Self::Queries(view) => Some(view.connection()),
            Self::Workspace(view) => Some(view.connection()),
        }
    }

    /// Path to this view, e.g. `pg_prod › daily_revenue`.
    pub fn breadcrumb(&self) -> impl Iterator<Item = &str> {
        let query = match self {
            Self::Workspace(view) => Some(view.query()),
            _ => None,
        };
        self.connection()
            .map(ConnectionName::as_str)
            .into_iter()
            .chain(query)
    }

    fn component(&mut self) -> &mut dyn Component {
        match self {
            Self::Connections(view) => view,
            Self::Queries(view) => view,
            Self::Workspace(view) => view,
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
            root: View::open(Route::Connections),
            above: Vec::new(),
        }
    }

    pub fn top(&self) -> &View {
        self.above.last().unwrap_or(&self.root)
    }

    pub fn top_mut(&mut self) -> &mut View {
        self.above.last_mut().unwrap_or(&mut self.root)
    }

    pub fn push(&mut self, route: Route) {
        match route {
            Route::Connections => self.above.clear(),
            route => self.above.push(View::open(route)),
        }
    }

    /// Pop the top view; the connection list stays.
    pub fn pop(&mut self) {
        self.above.pop();
    }

    pub fn jump(&mut self, route: Route) {
        self.above.clear();
        self.push(route);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::testing::connections;

    fn crumbs(stack: &ViewStack) -> Vec<&str> {
        stack.top().breadcrumb().collect()
    }

    #[test]
    fn navigates_down_back_and_jumps() {
        let connections = connections(&["a", "b"]);
        let a = connections.resolve("a").cloned().unwrap();
        let b = connections.resolve("b").cloned().unwrap();
        let mut stack = ViewStack::new();
        assert!(crumbs(&stack).is_empty());

        stack.pop();
        assert!(matches!(stack.top(), View::Connections(_)));

        stack.push(Route::Queries(a.clone()));
        stack.push(Route::Workspace {
            connection: a,
            query: "daily".into(),
        });
        assert_eq!(crumbs(&stack), ["a", "daily"]);

        stack.jump(Route::Queries(b));
        assert_eq!(crumbs(&stack), ["b"]);
        stack.pop();
        assert!(matches!(stack.top(), View::Connections(_)));
    }
}
