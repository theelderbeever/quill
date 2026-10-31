//! One open query: catalog tree, editor, results and statistics.
mod catalog;
mod editor;
mod layout;
mod results;
mod stats;

use crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
};

use crate::{
    component::{Component, Context, KeyHint, KeyOutcome},
    connection::ConnectionName,
};
use catalog::Catalog;
use editor::Editor;
pub use layout::Sidebar;
use layout::{Visibility, WorkspaceLayout};
use results::Results;
use stats::Stats;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    Catalog,
    Editor,
    Results,
    Stats,
}

impl Panel {
    /// Tab order.
    const ORDER: [Self; 4] = [Self::Catalog, Self::Editor, Self::Results, Self::Stats];

    const fn sidebar(self) -> Option<Sidebar> {
        match self {
            Self::Catalog => Some(Sidebar::Catalog),
            Self::Stats => Some(Sidebar::Stats),
            Self::Editor | Self::Results => None,
        }
    }
}

/// A direction to move focus across the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Down,
    Up,
    Right,
}

impl Panel {
    /// The panel drawn next to this one, by the layout in `layout.rs`.
    /// The panel to focus from this one. Catalog and stats span the full
    /// height, so up and down always mean editor and results.
    const fn neighbour(self, direction: Direction) -> Option<Self> {
        use Direction::*;
        match (self, direction) {
            (_, Up) | (Self::Catalog, Right) | (Self::Stats, Left) => Some(Self::Editor),
            (_, Down) => Some(Self::Results),
            (Self::Editor | Self::Results, Left) => Some(Self::Catalog),
            (Self::Editor | Self::Results, Right) => Some(Self::Stats),
            _ => None,
        }
    }
}

pub struct Workspace {
    connection: ConnectionName,
    query: String,
    focus: Panel,
    visibility: Visibility,
    /// Width at the last draw; sidebars hide or show based on it.
    width: u16,
    /// Panel areas at the last draw, for mouse hit testing.
    layout: Option<WorkspaceLayout>,
    catalog: Catalog,
    editor: Editor,
    results: Results,
    stats: Stats,
}

impl Workspace {
    pub const HINTS: &[KeyHint] = &[
        KeyHint::new("tab", "focus"),
        KeyHint::new("ctrl-hjkl", "move focus"),
        KeyHint::new("F2", "catalog"),
        KeyHint::new("F3", "stats"),
    ];

    pub const fn new(connection: ConnectionName, query: String) -> Self {
        Self {
            connection,
            query,
            focus: Panel::Editor,
            visibility: Visibility::AUTO,
            width: u16::MAX,
            layout: None,
            catalog: Catalog,
            editor: Editor::new(),
            results: Results,
            stats: Stats,
        }
    }

    pub const fn connection(&self) -> &ConnectionName {
        &self.connection
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    fn shows(&self, panel: Panel) -> bool {
        panel
            .sidebar()
            .is_none_or(|sidebar| self.visibility.shows(sidebar, self.width))
    }

    /// Move focus to the next or previous visible panel.
    pub fn cycle_focus(&mut self, forward: bool) {
        let visible: Vec<Panel> = Panel::ORDER
            .into_iter()
            .filter(|p| self.shows(*p))
            .collect();
        let Some(at) = visible.iter().position(|p| *p == self.focus) else {
            self.focus = Panel::Editor;
            return;
        };
        let len = visible.len();
        let next = if forward { at + 1 } else { at + len - 1 };
        self.focus = visible[next % len];
    }

    /// Focus the visible panel next to the focused one, if any.
    pub fn move_focus(&mut self, direction: Direction) {
        if let Some(panel) = self.focus.neighbour(direction).filter(|p| self.shows(*p)) {
            self.focus = panel;
        }
    }

    pub fn toggle(&mut self, sidebar: Sidebar) {
        self.visibility.toggle(sidebar, self.width);
        if !self.shows(self.focus) {
            self.focus = Panel::Editor;
        }
    }

    fn panel(&self, panel: Panel) -> &dyn Component {
        match panel {
            Panel::Catalog => &self.catalog,
            Panel::Editor => &self.editor,
            Panel::Results => &self.results,
            Panel::Stats => &self.stats,
        }
    }

    fn panel_mut(&mut self, panel: Panel) -> &mut dyn Component {
        match panel {
            Panel::Catalog => &mut self.catalog,
            Panel::Editor => &mut self.editor,
            Panel::Results => &mut self.results,
            Panel::Stats => &mut self.stats,
        }
    }
}

impl Component for Workspace {
    fn handle_key(&mut self, key: KeyEvent, ctx: &Context) -> KeyOutcome {
        self.panel_mut(self.focus).handle_key(key, ctx)
    }

    /// A left click focuses the panel under it; the event then goes to that panel.
    fn handle_mouse(&mut self, event: MouseEvent, ctx: &Context) -> KeyOutcome {
        let position = Position::new(event.column, event.row);
        let Some(panel) = self.layout.as_ref().and_then(|l| l.panel_at(position)) else {
            return KeyOutcome::Ignored;
        };
        if event.kind == MouseEventKind::Down(MouseButton::Left) {
            self.focus = panel;
        }
        self.panel_mut(panel).handle_mouse(event, ctx)
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Context) {
        self.width = area.width;
        if !self.shows(self.focus) {
            self.focus = Panel::Editor;
        }
        let layout = WorkspaceLayout::split(area, self.visibility);
        let layout = self.layout.insert(layout);
        let areas = Panel::ORDER.map(|panel| layout.get(panel));
        for (panel, area) in Panel::ORDER.into_iter().zip(areas) {
            if let Some(area) = area {
                let ctx = Context {
                    focused: ctx.focused && panel == self.focus,
                    ..*ctx
                };
                self.panel_mut(panel).render(frame, area, &ctx);
            }
        }
    }

    fn hints(&self) -> &'static [KeyHint] {
        self.panel(self.focus).hints()
    }

    fn captures_input(&self) -> bool {
        self.panel(self.focus).captures_input()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::testing::connections;

    fn workspace(width: u16) -> Workspace {
        let connection = connections(&["a"]).resolve("a").cloned().unwrap();
        let mut workspace = Workspace::new(connection, "q".into());
        workspace.width = width;
        workspace
    }

    #[test]
    fn focus_cycles_through_visible_panels() {
        let mut wide = workspace(u16::MAX);
        let mut order = Vec::new();
        for _ in 0..4 {
            wide.cycle_focus(true);
            order.push(wide.focus);
        }
        assert_eq!(
            order,
            [Panel::Results, Panel::Stats, Panel::Catalog, Panel::Editor]
        );

        let mut narrow = workspace(layout::HIDE_CATALOG_BELOW - 1);
        narrow.cycle_focus(false);
        assert_eq!(narrow.focus, Panel::Results);
        narrow.cycle_focus(false);
        assert_eq!(narrow.focus, Panel::Editor);
    }

    #[test]
    fn ctrl_hjkl_moves_to_visible_neighbours() {
        let mut wide = workspace(u16::MAX);
        wide.move_focus(Direction::Down);
        assert_eq!(wide.focus, Panel::Results);
        wide.move_focus(Direction::Down);
        assert_eq!(wide.focus, Panel::Results);
        wide.move_focus(Direction::Right);
        assert_eq!(wide.focus, Panel::Stats);
        wide.move_focus(Direction::Left);
        assert_eq!(wide.focus, Panel::Editor);
        wide.move_focus(Direction::Left);
        assert_eq!(wide.focus, Panel::Catalog);

        wide.move_focus(Direction::Down);
        assert_eq!(wide.focus, Panel::Results);
        wide.move_focus(Direction::Right);
        wide.move_focus(Direction::Up);
        assert_eq!(wide.focus, Panel::Editor);

        let mut narrow = workspace(layout::HIDE_CATALOG_BELOW - 1);
        narrow.move_focus(Direction::Left);
        assert_eq!(narrow.focus, Panel::Editor);
    }

    #[test]
    fn left_click_focuses_the_panel_under_it() {
        let mut workspace = workspace(u16::MAX);
        let area = Rect::new(0, 0, 160, 40);
        workspace.layout = Some(WorkspaceLayout::split(area, Visibility::AUTO));
        let ctx = Context {
            connections: &connections(&[]),
            focused: true,
        };
        let click = |column, row| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: crossterm::event::KeyModifiers::NONE,
        };
        workspace.handle_mouse(click(1, 1), &ctx);
        assert_eq!(workspace.focus, Panel::Catalog);
        workspace.handle_mouse(click(80, 39), &ctx);
        assert_eq!(workspace.focus, Panel::Results);
        workspace.handle_mouse(click(159, 0), &ctx);
        assert_eq!(workspace.focus, Panel::Stats);
    }

    #[test]
    fn hiding_the_focused_sidebar_moves_focus_to_the_editor() {
        let mut workspace = workspace(u16::MAX);
        workspace.focus = Panel::Stats;
        workspace.toggle(Sidebar::Stats);
        assert_eq!(workspace.focus, Panel::Editor);
        workspace.toggle(Sidebar::Stats);
        assert!(workspace.shows(Panel::Stats));
    }
}
