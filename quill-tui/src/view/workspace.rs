//! A connection's query session: a tab per saved query, each with its own
//! editor, results and statistics, beside a shared catalog tree.
mod catalog;
mod editor;
mod layout;
mod results;
mod stats;
mod tab;

use std::{ops::Range, path::Path, time::Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    text::Line,
};

use crate::{
    action::Action,
    component::{Component, Context, KeyHint, KeyOutcome},
    connection::ConnectionId,
    event::Msg,
    store::queries::{QueryName, QueryStore},
    widget::{
        input::{InputEvent, TextInput},
        palette::Flash,
        prompt,
    },
};
use catalog::Catalog;
use editor::Editor;
pub use layout::Sidebar;
use layout::{Border, Sizes, Visibility, WorkspaceLayout};
use tab::{QueryTab, Tabs};

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

const PROMPT_HINTS: &[KeyHint] = &[
    KeyHint::new("enter", "confirm"),
    KeyHint::new("esc", "cancel"),
];
const HELP_HINTS: &[KeyHint] = &[KeyHint::new("?/esc", "close help")];
/// Keys listed only in the `?` dialog, to keep the header short.
const HELP: &[KeyHint] = &[
    KeyHint::new("hjkl", "move"),
    KeyHint::new("alt-hjkl", "resize"),
    KeyHint::new("ctrl-b", "catalog"),
    KeyHint::new("ctrl-shift-b", "stats"),
];

/// A dialog over the workspace, taking every key until it closes.
enum Prompt {
    New(TextInput),
    Delete(QueryName),
    Help,
}

pub struct Workspace {
    connection: ConnectionId,
    store: QueryStore,
    tabs: Tabs,
    focus: Panel,
    visibility: Visibility,
    sizes: Sizes,
    /// Border being dragged with the mouse.
    dragging: Option<Border>,
    /// Width at the last draw; sidebars hide or show based on it.
    width: u16,
    /// Panel areas at the last draw, for mouse hit testing.
    layout: Option<WorkspaceLayout>,
    /// Columns of each tab title at the last draw, for mouse hit testing.
    tab_hits: Vec<(Range<u16>, usize)>,
    prompt: Option<Prompt>,
    catalog: Catalog,
}

impl Workspace {
    pub const HINTS: &[KeyHint] = &[
        KeyHint::new("F5", "run"),
        KeyHint::new("e", "edit"),
        KeyHint::new("tab", "focus"),
        KeyHint::new("[/]", "last/next query"),
        KeyHint::new("n", "new query"),
        KeyHint::new("d", "delete query"),
        KeyHint::new("?", "help"),
    ];

    /// Open every saved query on `connection` as a tab, selecting `query`.
    pub fn open(connection: ConnectionId, query: QueryName, ctx: &Context) -> anyhow::Result<Self> {
        let store = QueryStore::new(ctx.connections().query_dir(&connection));
        let mut tabs = Tabs::new(QueryTab::open(query.clone(), &store)?);
        tabs.sync(&store.list()?, &query, &store)?;
        Ok(Self::new(connection, store, tabs))
    }

    fn new(connection: ConnectionId, store: QueryStore, tabs: Tabs) -> Self {
        Self {
            catalog: Catalog::new(connection.clone()),
            connection,
            store,
            tabs,
            focus: Panel::Editor,
            visibility: Visibility::AUTO,
            sizes: Sizes::DEFAULT,
            dragging: None,
            width: u16::MAX,
            layout: None,
            tab_hits: Vec::new(),
            prompt: None,
        }
    }

    /// Return to the session at `query`, picking up queries added, renamed
    /// or deleted since it was left.
    pub fn reopen(&mut self, query: &QueryName) -> anyhow::Result<()> {
        self.tabs.sync(&self.store.list()?, query, &self.store)
    }

    pub const fn connection(&self) -> &ConnectionId {
        &self.connection
    }

    pub fn query(&self) -> &QueryName {
        self.tabs.active().query()
    }

    /// Save unsaved text in every tab. Runs and results live on.
    pub fn close(&mut self) -> anyhow::Result<()> {
        for tab in self.tabs.iter_mut() {
            tab.save_if(&self.store, Editor::unsaved)?;
        }
        Ok(())
    }

    /// Autosave once edits have paused. Returns true while a query runs, so
    /// its progress keeps redrawing.
    pub fn tick(&mut self, ctx: &Context) -> anyhow::Result<bool> {
        let (now, delay) = (Instant::now(), ctx.options().autosave);
        for tab in self.tabs.iter_mut() {
            tab.save_if(&self.store, |editor| editor.due(now, delay))?;
        }
        Ok(self.tabs.iter().any(QueryTab::is_running))
    }

    /// Whether `msg` is for this session.
    pub fn owns(&self, msg: &Msg) -> bool {
        match msg {
            Msg::Catalog(msg) => msg.connection == self.connection,
            Msg::Run(msg) => msg
                .run()
                .is_none_or(|run| self.tabs.iter().any(|t| t.owns(run))),
            Msg::Connected { .. } => false,
        }
    }

    /// Apply a background update. Returns a message to show, if any.
    pub fn handle_msg(&mut self, msg: Msg, ctx: &Context) -> Option<Flash> {
        match msg {
            Msg::Catalog(msg) => {
                self.catalog.handle_msg(msg, ctx);
                None
            }
            Msg::Run(msg) => {
                let tab = match msg.run() {
                    Some(run) => self.tabs.find_run_mut(run)?,
                    None => self.tabs.active_mut(),
                };
                tab.handle_msg(msg, &self.connection, ctx)
            }
            Msg::Connected { .. } => None,
        }
    }

    /// Whether `/` should edit a results condition: the results have focus
    /// and hold rows. Returns the current condition.
    pub fn filter(&self) -> Option<&str> {
        let results = &self.tabs.active().results;
        (self.focus == Panel::Results && results.has_grid()).then(|| results.filter())
    }

    pub fn set_filter(&mut self, filter: String, ctx: &Context) -> Option<Flash> {
        self.tabs.active_mut().results.set_filter(filter, ctx)
    }

    pub fn save(&self, path: &Path, ctx: &Context) -> Option<Flash> {
        self.tabs.active().results.save(path, ctx)
    }

    /// The active tab's run progress, shown on the palette line.
    pub fn status(&self) -> Option<Line<'_>> {
        self.tabs.active().status()
    }

    /// Query and help keys: `[` and `]` step through tabs, `n` adds a
    /// query, `d` deletes the active one and `?` opens help.
    fn query_key(&mut self, key: KeyEvent) -> Option<KeyOutcome> {
        match key.code {
            KeyCode::Char('[') => self.tabs.prev(),
            KeyCode::Char(']') => self.tabs.next(),
            KeyCode::Char('n') => self.prompt = Some(Prompt::New(TextInput::default())),
            KeyCode::Char('d') if self.tabs.len() == 1 => {
                let message = "the last open query is deleted from the query list".into();
                return Some(KeyOutcome::Action(Action::Report(message)));
            }
            KeyCode::Char('d') => self.prompt = Some(Prompt::Delete(self.query().clone())),
            KeyCode::Char('?') => self.prompt = Some(Prompt::Help),
            _ => return None,
        }
        Some(KeyOutcome::Consumed)
    }

    fn handle_prompt(&mut self, key: KeyEvent) -> anyhow::Result<()> {
        let Some(prompt) = &mut self.prompt else {
            return Ok(());
        };
        let submitted = match prompt {
            Prompt::New(input) => match input.handle_key(key) {
                InputEvent::Submit => true,
                InputEvent::Cancel => false,
                InputEvent::Edited | InputEvent::Ignored => return Ok(()),
            },
            Prompt::Delete(_) => match key.code {
                KeyCode::Char('y') | KeyCode::Enter => true,
                KeyCode::Char('n') | KeyCode::Esc => false,
                _ => return Ok(()),
            },
            Prompt::Help => match key.code {
                KeyCode::Char('?') | KeyCode::Esc => false,
                _ => return Ok(()),
            },
        };
        if submitted {
            // Keep the prompt open on invalid input so it can be corrected.
            match prompt {
                Prompt::New(input) => {
                    let name = QueryName::parse(input.text())?;
                    self.store.create(&name)?;
                    self.tabs.add(name, &self.store)?;
                    self.focus = Panel::Editor;
                }
                Prompt::Delete(name) => {
                    self.store.delete(name)?;
                    self.tabs.remove(name);
                }
                Prompt::Help => {}
            }
        }
        self.prompt = None;
        Ok(())
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

    pub fn toggle(&mut self, sidebar: Sidebar) {
        self.visibility.toggle(sidebar, self.width);
        if !self.shows(self.focus) {
            self.focus = Panel::Editor;
        }
    }

    fn tab_at(&self, position: Position) -> Option<usize> {
        let editor = self.layout.as_ref()?.get(Panel::Editor)?;
        let x = position.x.checked_sub(editor.x + 1)?;
        (position.y == editor.y)
            .then(|| self.tab_hits.iter().find(|(span, _)| span.contains(&x)))
            .flatten()
            .map(|(_, index)| *index)
    }

    /// The editor, with the tab titles in its top border.
    fn render_editor(&mut self, frame: &mut Frame, area: Rect, ctx: &Context) {
        let editor = &self.tabs.active().editor;
        let marker = if editor.captures_input() {
            editor::INSERT_MARKER_WIDTH
        } else {
            0
        };
        let (title, hits) = self.tabs.title(area.width.saturating_sub(2 + marker));
        self.tab_hits = hits;
        self.tabs
            .active_mut()
            .editor
            .render_titled(frame, area, ctx, title);
    }

    fn panel(&self, panel: Panel) -> &dyn Component {
        match panel {
            Panel::Catalog => &self.catalog,
            Panel::Editor => &self.tabs.active().editor,
            Panel::Results => &self.tabs.active().results,
            Panel::Stats => &self.tabs.active().stats,
        }
    }

    fn panel_mut(&mut self, panel: Panel) -> &mut dyn Component {
        match panel {
            Panel::Catalog => &mut self.catalog,
            Panel::Editor => &mut self.tabs.active_mut().editor,
            Panel::Results => &mut self.tabs.active_mut().results,
            Panel::Stats => &mut self.tabs.active_mut().stats,
        }
    }
}

impl Component for Workspace {
    /// Run keys work from every panel, insert mode included. Once the
    /// focused panel has released input, Esc cancels a running query, `e`
    /// starts editing, and query and help keys apply.
    fn handle_key(&mut self, key: KeyEvent, ctx: &Context) -> KeyOutcome {
        if self.prompt.is_some() {
            return match self.handle_prompt(key) {
                Ok(()) => KeyOutcome::Consumed,
                Err(e) => KeyOutcome::Action(Action::Report(format!("{e:#}"))),
            };
        }
        let run = key.code == KeyCode::F(5)
            || (key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::CONTROL));
        if run {
            return self.tabs.active_mut().start_run(&self.connection, ctx);
        }
        let capturing = self.panel(self.focus).captures_input();
        if key.code == KeyCode::Esc && !capturing && self.tabs.active().cancel() {
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
        if !capturing && key.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            if key.code == KeyCode::Char('e') {
                self.focus = Panel::Editor;
                self.tabs.active_mut().editor.insert();
                return KeyOutcome::Consumed;
            }
            if let Some(outcome) = self.query_key(key) {
                return outcome;
            }
        }
        let outcome = self.panel_mut(self.focus).handle_key(key, ctx);
        if let Some(sql) = self.catalog.take_insert() {
            self.tabs.active_mut().editor.insert_text(&sql);
            self.focus = Panel::Editor;
        }
        outcome
    }

    /// A left click on a tab title selects it. Dragging a border between
    /// panels resizes them. Otherwise a left click focuses the panel under
    /// it, and the event goes to that panel.
    fn handle_mouse(&mut self, event: MouseEvent, ctx: &Context) -> KeyOutcome {
        if self.prompt.is_some() {
            return KeyOutcome::Consumed;
        }
        if self.tabs.active_mut().results.handle_overlay_mouse(event) {
            return KeyOutcome::Consumed;
        }
        let position = Position::new(event.column, event.row);
        if event.kind == MouseEventKind::Down(MouseButton::Left)
            && let Some(index) = self.tab_at(position)
        {
            self.tabs.select_index(index);
            self.focus = Panel::Editor;
            return KeyOutcome::Consumed;
        }
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
            let Some(area) = area else { continue };
            let ctx = ctx.with_focus(ctx.focused && panel == self.focus);
            if panel == Panel::Editor {
                self.render_editor(frame, area, &ctx);
            } else {
                self.panel_mut(panel).render(frame, area, &ctx);
            }
        }
        self.tabs.active().results.render_overlay(frame, area);
        match &self.prompt {
            Some(Prompt::New(input)) => prompt::render_input(frame, area, "new query", input),
            Some(Prompt::Delete(name)) => {
                prompt::render_confirm(frame, area, "delete", &format!("Delete {name}.sql?"));
            }
            Some(Prompt::Help) => prompt::render_keys(frame, area, "help", HELP),
            None => {}
        }
    }

    fn hints(&self) -> &'static [KeyHint] {
        match self.prompt {
            Some(Prompt::Help) => return HELP_HINTS,
            Some(_) => return PROMPT_HINTS,
            None => {}
        }
        self.panel(self.focus).hints()
    }

    fn captures_input(&self) -> bool {
        self.prompt.is_some() || self.panel(self.focus).captures_input()
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
        let names = ["a", "b", "c", "d"].map(|n| QueryName::parse(n).unwrap());
        let mut tabs = Tabs::new(QueryTab::open(names[0].clone(), &store).unwrap());
        tabs.sync(&names, &names[0], &store).unwrap();
        let mut workspace = Workspace::new(id(&connections, "a"), store, tabs);
        workspace.width = width;
        workspace
    }

    fn press(workspace: &mut Workspace, code: KeyCode, ctx: &Context) -> KeyOutcome {
        workspace.handle_key(KeyEvent::new(code, KeyModifiers::NONE), ctx)
    }

    #[test]
    fn brackets_switch_queries_from_any_panel_but_not_while_editing() {
        let (shared, _rx) = crate::component::testing::shared(connections(&[]));
        let ctx = shared.context(true);
        for panel in Panel::ORDER {
            let mut workspace = workspace(u16::MAX);
            workspace.focus = panel;
            press(&mut workspace, KeyCode::Char('['), &ctx);
            assert_eq!(workspace.query().as_str(), "d");
            press(&mut workspace, KeyCode::Char(']'), &ctx);
            assert_eq!(workspace.query().as_str(), "a");
        }
        let mut workspace = workspace(u16::MAX);
        press(&mut workspace, KeyCode::Char('e'), &ctx);
        press(&mut workspace, KeyCode::Char(']'), &ctx);
        assert_eq!(workspace.query().as_str(), "a");
        assert!(workspace.tabs.active_mut().editor.unsaved().is_some());
    }

    #[test]
    fn question_mark_toggles_help() {
        let (shared, _rx) = crate::component::testing::shared(connections(&[]));
        let ctx = shared.context(true);
        let mut workspace = workspace(u16::MAX);
        press(&mut workspace, KeyCode::Char('?'), &ctx);
        assert!(matches!(workspace.prompt, Some(Prompt::Help)));
        // Help takes keys until it closes.
        press(&mut workspace, KeyCode::Char(']'), &ctx);
        assert_eq!(workspace.query().as_str(), "a");
        press(&mut workspace, KeyCode::Char('?'), &ctx);
        assert!(workspace.prompt.is_none());
        press(&mut workspace, KeyCode::Char('?'), &ctx);
        press(&mut workspace, KeyCode::Esc, &ctx);
        assert!(workspace.prompt.is_none());
    }

    #[test]
    fn n_and_d_add_and_delete_queries() {
        let dir = tempfile::tempdir().unwrap();
        let connections = connections(&["a"]);
        let store = QueryStore::new(dir.path());
        let only = QueryName::parse("only").unwrap();
        store.create(&only).unwrap();
        let tabs = Tabs::new(QueryTab::open(only, &store).unwrap());
        let mut workspace = Workspace::new(id(&connections, "a"), store, tabs);
        let (shared, _rx) = crate::component::testing::shared(connections);
        let ctx = shared.context(true);

        // The only query cannot be deleted here.
        press(&mut workspace, KeyCode::Char('d'), &ctx);
        assert!(workspace.prompt.is_none());

        press(&mut workspace, KeyCode::Char('n'), &ctx);
        assert!(workspace.captures_input());
        for c in "fresh".chars() {
            press(&mut workspace, KeyCode::Char(c), &ctx);
        }
        press(&mut workspace, KeyCode::Enter, &ctx);
        assert_eq!(workspace.query().as_str(), "fresh");
        assert!(dir.path().join("fresh.sql").is_file());

        press(&mut workspace, KeyCode::Char('d'), &ctx);
        press(&mut workspace, KeyCode::Char('n'), &ctx);
        assert!(dir.path().join("fresh.sql").is_file());
        press(&mut workspace, KeyCode::Char('d'), &ctx);
        press(&mut workspace, KeyCode::Char('y'), &ctx);
        assert!(!dir.path().join("fresh.sql").exists());
        assert_eq!(workspace.query().as_str(), "only");
        assert!(!workspace.captures_input());
    }

    #[test]
    fn clicking_a_tab_selects_it() {
        let (shared, _rx) = crate::component::testing::shared(connections(&[]));
        let ctx = shared.context(true);
        let mut workspace = workspace(u16::MAX);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 40)).unwrap();
        terminal
            .draw(|frame| workspace.render(frame, frame.area(), &ctx))
            .unwrap();
        let editor = workspace
            .layout
            .as_ref()
            .unwrap()
            .get(Panel::Editor)
            .unwrap();
        let top: String = (editor.x..editor.right())
            .map(|x| {
                terminal.backend().buffer()[(x, editor.y)]
                    .symbol()
                    .to_owned()
            })
            .collect();
        assert!(top.contains(" a │ b │ c │ d "), "{top}");

        workspace.focus = Panel::Results;
        let c = top.chars().position(|c| c == 'c').unwrap();
        let column = editor.x + u16::try_from(c).unwrap();
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row: editor.y,
            modifiers: KeyModifiers::NONE,
        };
        workspace.handle_mouse(click, &ctx);
        assert_eq!(workspace.query().as_str(), "c");
        assert_eq!(workspace.focus, Panel::Editor);
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
    fn e_edits_from_any_panel() {
        let (shared, _rx) = crate::component::testing::shared(connections(&[]));
        let ctx = shared.context(true);
        let e = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE);
        for panel in Panel::ORDER {
            let mut workspace = workspace(u16::MAX);
            workspace.focus = panel;
            assert!(matches!(
                workspace.handle_key(e, &ctx),
                KeyOutcome::Consumed
            ));
            assert_eq!(workspace.focus, Panel::Editor);
            assert!(workspace.captures_input(), "{panel:?}");
            // Already editing: `e` is text, not a command.
            workspace.handle_key(e, &ctx);
            assert!(workspace.captures_input());
        }
    }
}
