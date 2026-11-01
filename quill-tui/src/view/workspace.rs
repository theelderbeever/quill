//! One open query: catalog tree, editor, results and statistics.
mod catalog;
mod editor;
mod layout;
mod results;
mod stats;

use std::{path::Path, time::Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    text::{Line, Span},
};

use crate::{
    action::Action,
    component::{Component, Context, KeyHint, KeyOutcome},
    connection::{ConnectionId, ConnectionState},
    event::Msg,
    format,
    store::queries::{QueryName, QueryStore},
    task::{RunMsg, run::Run, stats as stats_task},
    widget::{palette::Flash, theme},
};
use catalog::Catalog;
use editor::Editor;
pub use layout::Sidebar;
use layout::{Border, Sizes, Visibility, WorkspaceLayout};
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

/// A direction to move focus across the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Down,
    Up,
    Right,
}

pub struct Workspace {
    connection: ConnectionId,
    query: QueryName,
    store: QueryStore,
    focus: Panel,
    visibility: Visibility,
    sizes: Sizes,
    /// Border being dragged with the mouse.
    dragging: Option<Border>,
    /// Width at the last draw; sidebars hide or show based on it.
    width: u16,
    /// Panel areas at the last draw, for mouse hit testing.
    layout: Option<WorkspaceLayout>,
    run: Option<Run>,
    catalog: Catalog,
    editor: Editor,
    results: Results,
    stats: Stats,
}

impl Workspace {
    pub const HINTS: &[KeyHint] = &[
        KeyHint::new("F5", "run"),
        KeyHint::new("i", "edit"),
        KeyHint::new("tab", "focus"),
        KeyHint::new("ctrl-hjkl", "move focus"),
        KeyHint::new("alt-hjkl", "resize"),
        KeyHint::new("F2", "catalog"),
        KeyHint::new("F3", "stats"),
    ];

    /// Open `query`, resuming its saved text.
    pub fn open(connection: ConnectionId, query: QueryName, ctx: &Context) -> anyhow::Result<Self> {
        let store = QueryStore::new(ctx.connections().query_dir(&connection));
        let editor = Editor::new(store.read(&query)?)?;
        Ok(Self::new(connection, query, store, editor))
    }

    fn new(connection: ConnectionId, query: QueryName, store: QueryStore, editor: Editor) -> Self {
        Self {
            catalog: Catalog::new(connection.clone()),
            connection,
            query,
            store,
            focus: Panel::Editor,
            visibility: Visibility::AUTO,
            sizes: Sizes::DEFAULT,
            dragging: None,
            width: u16::MAX,
            layout: None,
            run: None,
            editor,
            results: Results::new(),
            stats: Stats::new(),
        }
    }

    pub const fn connection(&self) -> &ConnectionId {
        &self.connection
    }

    pub const fn query(&self) -> &QueryName {
        &self.query
    }

    /// Save unsaved text and stop any running query.
    pub fn close(&mut self) -> anyhow::Result<()> {
        self.run = None;
        self.save_if(|editor| editor.unsaved())
    }

    /// Autosave once edits have paused. Returns true while a query runs, so
    /// its progress keeps redrawing.
    pub fn tick(&mut self, ctx: &Context) -> anyhow::Result<bool> {
        let delay = ctx.options().autosave;
        self.save_if(|editor| editor.due(Instant::now(), delay))?;
        Ok(self.run.is_some())
    }

    fn save_if(&mut self, text: impl FnOnce(&mut Editor) -> Option<String>) -> anyhow::Result<()> {
        if let Some(text) = text(&mut self.editor) {
            self.store.write(&self.query, &text)?;
            self.editor.mark_saved(text);
        }
        Ok(())
    }

    /// Apply a background update. Returns a message to show, if any.
    pub fn handle_msg(&mut self, msg: Msg, ctx: &Context) -> Option<Flash> {
        let msg = match msg {
            Msg::Catalog(msg) => {
                self.catalog.handle_msg(msg, ctx);
                return None;
            }
            Msg::Run(msg) => msg,
            Msg::Connected { .. } => return None,
        };
        match msg {
            RunMsg::Started {
                run,
                query,
                control,
            } => {
                if let Some(current) = self.run.as_mut().filter(|r| r.id() == run) {
                    current.attach(query, control);
                }
            }
            RunMsg::Finished { run, result } => {
                if self.run.as_ref().is_some_and(|r| r.id() == run)
                    && let Some(finished) = self.run.take()
                {
                    let (client, sink) = finished.metrics();
                    let pool = ctx.connections().pool(&self.connection);
                    let lookup = finished.query().zip(pool);
                    self.stats.finished(run, client, sink, lookup.is_some());
                    if let Some((query, pool)) = lookup {
                        stats_task::spawn_server_metrics(
                            pool,
                            query,
                            run,
                            ctx.shared.messages.clone(),
                        );
                    }
                    self.results.finish(run, finished.dir(), result, ctx);
                }
            }
            RunMsg::Page { run, index, result } => return self.results.page(run, index, result),
            RunMsg::Profile { run, result } => self.stats.profile(run, result),
            RunMsg::ServerMetrics { run, result } => self.stats.server(run, result),
            RunMsg::Derived { base, view, result } => {
                return self.results.derived(base, view, result, ctx);
            }
            RunMsg::Saved { path, result } => {
                return Some(match result {
                    Ok(()) => Flash::Info(format!("saved {}", path.display())),
                    Err(e) => Flash::Error(format!("save failed: {e:#}")),
                });
            }
        }
        None
    }

    /// Whether `/` should edit a results condition: the results have focus
    /// and hold rows. Returns the current condition.
    pub fn filter(&self) -> Option<&str> {
        (self.focus == Panel::Results && self.results.has_grid()).then(|| self.results.filter())
    }

    pub fn set_filter(&mut self, filter: String, ctx: &Context) -> Option<Flash> {
        self.results.set_filter(filter, ctx)
    }

    pub fn save(&self, path: &Path, ctx: &Context) -> Option<Flash> {
        self.results.save(path, ctx)
    }

    /// Run progress, shown on the palette line.
    pub fn status(&self) -> Option<Line<'_>> {
        let (sink, elapsed) = self.run.as_ref()?.progress();
        Some(Line::from(vec![
            Span::styled("running ", theme::PENDING),
            Span::raw(format!(
                "{} rows · {} · {}",
                format::count(sink.rows_written),
                format::bytes(sink.bytes_written),
                format::duration(elapsed),
            )),
            Span::styled("  esc cancel", theme::DIM),
        ]))
    }

    fn start_run(&mut self, ctx: &Context) -> KeyOutcome {
        let report = |message: String| KeyOutcome::Action(Action::Report(message));
        if self.run.is_some() {
            return report("a query is already running; esc cancels it".into());
        }
        let connections = ctx.connections();
        let Some(pool) = connections.pool(&self.connection) else {
            return report(match connections.state(&self.connection) {
                Some(ConnectionState::Failed(error)) => format!("{}: {error}", self.connection),
                _ => format!("{} is still connecting", self.connection),
            });
        };
        let sql = self.editor.sql_to_run();
        if sql.trim().is_empty() {
            return report("nothing to run".into());
        }
        let run = Run::start(
            pool,
            sql,
            &ctx.shared.results_dir,
            ctx.options().byte_limit,
            ctx.shared.messages.clone(),
        );
        self.stats.started(run.id());
        self.run = Some(run);
        self.results.running();
        KeyOutcome::Consumed
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
    /// Run keys work from every panel, insert mode included. Esc cancels a
    /// running query and `i` starts editing, once the focused panel has
    /// released input.
    fn handle_key(&mut self, key: KeyEvent, ctx: &Context) -> KeyOutcome {
        let run = key.code == KeyCode::F(5)
            || (key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::CONTROL));
        if run {
            return self.start_run(ctx);
        }
        let capturing = self.panel(self.focus).captures_input();
        if key.code == KeyCode::Esc
            && !capturing
            && let Some(run) = &self.run
        {
            run.cancel();
            return KeyOutcome::Consumed;
        }
        if key.modifiers.contains(KeyModifiers::ALT) && !capturing {
            let (wider, taller) = match key.code {
                KeyCode::Char('h') | KeyCode::Left => (-1, 0),
                KeyCode::Char('l') | KeyCode::Right => (1, 0),
                KeyCode::Char('j') | KeyCode::Down => (0, 1),
                KeyCode::Char('k') | KeyCode::Up => (0, -1),
                _ => (0, 0),
            };
            if (wider, taller) != (0, 0) {
                self.sizes.resize(self.focus, wider, taller);
                return KeyOutcome::Consumed;
            }
        }
        if key.code == KeyCode::Char('i') && key.modifiers.is_empty() && !capturing {
            self.focus = Panel::Editor;
            self.editor.insert();
            return KeyOutcome::Consumed;
        }
        let outcome = self.panel_mut(self.focus).handle_key(key, ctx);
        if let Some(sql) = self.catalog.take_insert() {
            self.editor.insert_text(&sql);
            self.focus = Panel::Editor;
        }
        outcome
    }

    /// Dragging a border between panels resizes them. Otherwise a left
    /// click focuses the panel under it, and the event goes to that panel.
    fn handle_mouse(&mut self, event: MouseEvent, ctx: &Context) -> KeyOutcome {
        let position = Position::new(event.column, event.row);
        match (event.kind, self.dragging) {
            (MouseEventKind::Drag(MouseButton::Left), Some(border)) => {
                if let Some(layout) = &self.layout {
                    self.sizes.drag(border, position, layout);
                }
                return KeyOutcome::Consumed;
            }
            (MouseEventKind::Up(MouseButton::Left), Some(_)) => {
                self.dragging = None;
                return KeyOutcome::Consumed;
            }
            (MouseEventKind::Down(MouseButton::Left), _) => {
                self.dragging = self.layout.as_ref().and_then(|l| l.border_at(position));
                if self.dragging.is_some() {
                    return KeyOutcome::Consumed;
                }
            }
            _ => {}
        }
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
        let layout = WorkspaceLayout::split(area, self.visibility, self.sizes);
        let layout = self.layout.insert(layout);
        let areas = Panel::ORDER.map(|panel| layout.get(panel));
        for (panel, area) in Panel::ORDER.into_iter().zip(areas) {
            if let Some(area) = area {
                let focused = ctx.focused && panel == self.focus;
                self.panel_mut(panel)
                    .render(frame, area, &ctx.with_focus(focused));
            }
        }
        self.results.render_overlay(frame, area);
    }

    fn hints(&self) -> &'static [KeyHint] {
        self.panel(self.focus).hints()
    }

    fn captures_input(&self) -> bool {
        self.panel(self.focus).captures_input()
    }

    fn copy_text(&mut self) -> Option<String> {
        self.panel_mut(self.focus).copy_text()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::testing::{connections, id};

    fn workspace(width: u16) -> Workspace {
        let connections = connections(&["a"]);
        let store = QueryStore::new(std::env::temp_dir().join("quill-test-none"));
        let editor = Editor::new(String::new()).unwrap();
        let query = QueryName::parse("q").unwrap();
        let mut workspace = Workspace::new(id(&connections, "a"), query, store, editor);
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
        workspace.layout = Some(WorkspaceLayout::split(
            area,
            Visibility::AUTO,
            Sizes::DEFAULT,
        ));
        let (shared, _rx) = crate::component::testing::shared(connections(&[]));
        let ctx = shared.context(true);
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

    #[test]
    fn i_edits_from_any_panel() {
        let (shared, _rx) = crate::component::testing::shared(connections(&[]));
        let ctx = shared.context(true);
        let i = KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE);
        for panel in Panel::ORDER {
            let mut workspace = workspace(u16::MAX);
            workspace.focus = panel;
            assert!(matches!(
                workspace.handle_key(i, &ctx),
                KeyOutcome::Consumed
            ));
            assert_eq!(workspace.focus, Panel::Editor);
            assert!(workspace.captures_input(), "{panel:?}");
            // Already editing: `i` is text, not a command.
            workspace.handle_key(i, &ctx);
            assert!(workspace.captures_input());
        }
    }
}
