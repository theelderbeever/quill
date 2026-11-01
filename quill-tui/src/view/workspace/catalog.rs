//! Catalogs, schemas, tables and columns of the workspace's connection.
mod tree;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::Rect,
    text::{Line as TextLine, Span},
    widgets::{List, ListItem, ListState, Paragraph},
};

use crate::{
    component::{Component, Context, KeyHint, KeyOutcome},
    connection::ConnectionId,
    task::catalog::{self, CatalogMsg, Load},
    widget::{panel, theme},
};
use tree::{Item, Line, Tree};

const HINTS: &[KeyHint] = &[
    KeyHint::new("l/h", "open/close"),
    KeyHint::new("enter", "select into editor"),
    KeyHint::new("r", "refresh"),
];
const SELECT_LIMIT: u32 = 100;

pub struct Catalog {
    connection: ConnectionId,
    tree: Tree,
    generation: u64,
    selected: usize,
    list: ListState,
    /// SQL for the workspace to put into the editor.
    insert: Option<String>,
}

impl Catalog {
    pub fn new(connection: ConnectionId) -> Self {
        Self {
            connection,
            tree: Tree::new(),
            generation: 0,
            selected: 0,
            list: ListState::default(),
            insert: None,
        }
    }

    /// SQL requested with Enter on a table, taken once.
    pub const fn take_insert(&mut self) -> Option<String> {
        self.insert.take()
    }

    pub fn handle_msg(&mut self, msg: CatalogMsg, ctx: &Context) {
        if msg.connection != self.connection || msg.generation != self.generation {
            return;
        }
        if let Some(load) = self.tree.loaded(&msg.path, msg.result) {
            // The single root catalog expanded itself.
            self.spawn(vec![0], load, ctx);
        }
    }

    fn spawn(&self, path: Vec<usize>, load: Load, ctx: &Context) {
        if let Some(pool) = ctx.connections().pool(&self.connection) {
            let (connection, messages) = (self.connection.clone(), ctx.shared.messages.clone());
            catalog::spawn(pool, connection, self.generation, path, load, messages);
        }
    }

    fn selected_path(&self) -> Option<Vec<usize>> {
        match self.tree.lines().into_iter().nth(self.selected)? {
            Line::Node { path, .. } => Some(path),
            Line::Status { .. } => None,
        }
    }
}

impl Component for Catalog {
    fn handle_key(&mut self, key: KeyEvent, ctx: &Context) -> KeyOutcome {
        let count = self.tree.lines().len();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Char('l') | KeyCode::Right => {
                if let Some(path) = self.selected_path()
                    && let Some(load) = self.tree.expand(&path)
                {
                    self.spawn(path, load, ctx);
                }
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if let Some(mut path) = self.selected_path()
                    && !self.tree.collapse(&path)
                {
                    // Already closed: jump to the parent.
                    path.pop();
                    if let Some(index) =
                        self.tree.lines().iter().position(
                            |line| matches!(line, Line::Node { path: p, .. } if *p == path),
                        )
                    {
                        self.selected = index;
                    }
                }
            }
            KeyCode::Enter => {
                let Some(path) = self.selected_path() else {
                    return KeyOutcome::Consumed;
                };
                match self.tree.node(&path).map(|n| &n.item) {
                    Some(Item::Table(table)) => {
                        let reference = ctx.connections().table_reference(&self.connection, table);
                        self.insert =
                            Some(format!("SELECT * FROM {reference} LIMIT {SELECT_LIMIT}"));
                    }
                    Some(_) => {
                        if let Some(load) = self.tree.expand(&path) {
                            self.spawn(path, load, ctx);
                        } else if self.tree.node(&path).is_some_and(|n| n.expanded) {
                            self.tree.collapse(&path);
                        }
                    }
                    None => {}
                }
            }
            KeyCode::Char('r') => {
                self.generation += 1;
                self.tree = Tree::new();
                self.selected = 0;
            }
            _ => return KeyOutcome::Ignored,
        }
        KeyOutcome::Consumed
    }

    fn render(&mut self, frame: &mut Frame, area: Rect, ctx: &Context) {
        let block = panel::block("catalog", ctx.focused);
        if ctx.connections().pool(&self.connection).is_none() {
            let message = Paragraph::new("Waiting for the connection…").style(theme::DIM);
            frame.render_widget(message.block(block), area);
            return;
        }
        if self.tree.begin_roots() {
            self.spawn(Vec::new(), Load::Catalogs, ctx);
        }
        let lines = self.tree.lines();
        self.selected = self.selected.min(lines.len().saturating_sub(1));
        let items = lines.into_iter().map(|line| {
            let text = match line {
                Line::Node {
                    depth,
                    label,
                    expanded,
                    leaf,
                    ..
                } => {
                    let marker = match (leaf, expanded) {
                        (true, _) => "  ",
                        (false, true) => "▾ ",
                        (false, false) => "▸ ",
                    };
                    let style = if leaf {
                        theme::DIM
                    } else {
                        ratatui::style::Style::new()
                    };
                    TextLine::from(vec![
                        Span::raw("  ".repeat(depth)),
                        Span::styled(marker, theme::DIM),
                        Span::styled(label, style),
                    ])
                }
                Line::Status {
                    depth,
                    text,
                    failed,
                } => TextLine::from(vec![
                    Span::raw("  ".repeat(depth + 1)),
                    Span::styled(text, if failed { theme::ERROR } else { theme::DIM }),
                ]),
            };
            ListItem::new(text)
        });
        self.list.select(Some(self.selected));
        let list = List::new(items)
            .highlight_style(theme::SELECTED)
            .block(block);
        frame.render_stateful_widget(list, area, &mut self.list);
    }

    fn hints(&self) -> &'static [KeyHint] {
        HINTS
    }
}
