//! Top-level state: routes keys, applies actions and draws the frame.
use std::collections::HashMap;

use crossterm::event::{
    Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
    MouseEventKind,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
};

use crate::{
    action::Action,
    command::Command,
    component::{Component, KeyHint, KeyOutcome, Shared},
    connection::{ConnectionId, ConnectionState, Target},
    event::{Event, Msg},
    filter::Filter,
    view::{
        Route, View, ViewStack,
        workspace::{Sidebar, Workspace},
    },
    widget::{
        header::{self, Header},
        palette::{self, Flash, Palette, PaletteEvent},
    },
};

const GLOBAL_HINTS: &[KeyHint] = &[
    KeyHint::new(":", "command"),
    KeyHint::new("/", "filter"),
    KeyHint::new("esc", "back"),
    KeyHint::new("ctrl-c", "quit"),
];

pub struct App {
    shared: Shared,
    /// Kept for the session: on X11 copied text lives only while its owner does.
    clipboard: Option<arboard::Clipboard>,
    views: ViewStack,
    /// Workspaces left for another view, kept with their runs and results
    /// until their connection is disconnected.
    sessions: HashMap<ConnectionId, Box<Workspace>>,
    palette: Palette,
    dirty: bool,
    quit: bool,
}

impl App {
    /// Start at `start`, or the connection list without one.
    pub fn new(shared: Shared, start: Option<Target>) -> Self {
        let mut app = Self {
            shared,
            clipboard: None,
            views: ViewStack::new(),
            sessions: HashMap::new(),
            palette: Palette::new(),
            dirty: true,
            quit: false,
        };
        if start.is_some() {
            app.apply(Action::Jump(Route::target(start)));
        }
        app
    }

    pub const fn should_quit(&self) -> bool {
        self.quit
    }

    /// Whether a redraw is due, clearing the flag.
    pub const fn take_dirty(&mut self) -> bool {
        std::mem::replace(&mut self.dirty, false)
    }

    pub fn handle(&mut self, event: Event) {
        match event {
            Event::Terminal(TerminalEvent::Key(key)) if key.kind == KeyEventKind::Press => {
                self.handle_key(key);
            }
            Event::Terminal(TerminalEvent::Mouse(mouse)) => {
                // Pointer motion arrives constantly and changes nothing.
                if mouse.kind == MouseEventKind::Moved {
                    return;
                }
                self.handle_mouse(mouse);
            }
            Event::Terminal(_) => {}
            Event::Msg(msg) => self.handle_msg(msg),
            Event::Tick => {
                let ctx = self.shared.context(true);
                match self.views.top_mut().tick(&ctx) {
                    Ok(redraw) => self.dirty |= redraw,
                    Err(e) => self.report(format!("{e:#}")),
                }
                return;
            }
        }
        self.dirty = true;
    }

    /// Save and leave the views on the way out, ending every session.
    pub fn close(&mut self) {
        self.leave_all(None);
        self.sessions.clear();
    }

    /// Save a view being left. A workspace is kept as its connection's session.
    fn retire(&mut self, mut view: View) {
        if let Err(e) = view.close() {
            self.report(format!("{e:#}"));
        }
        if let View::Workspace(workspace) = view {
            self.sessions
                .insert(workspace.connection().clone(), workspace);
        }
    }

    fn copy(&mut self, text: String) {
        let clipboard = match &mut self.clipboard {
            Some(clipboard) => Ok(clipboard),
            None => arboard::Clipboard::new().map(|c| self.clipboard.insert(c)),
        };
        let chars = text.chars().count();
        match clipboard.and_then(|c| c.set_text(text)) {
            Ok(()) => self.show(Flash::Info(format!("copied {chars} characters"))),
            Err(e) => self.report(format!("copy failed: {e}")),
        }
    }

    fn report(&mut self, message: String) {
        self.show(Flash::Error(message));
    }

    fn show(&mut self, flash: Flash) {
        self.palette.flash(flash);
        self.dirty = true;
    }

    /// Precedence: open palette, reserved keys, focused view, then fallbacks
    /// the view left unhandled.
    fn handle_key(&mut self, key: KeyEvent) {
        if self.palette.is_open() {
            match self.palette.handle_key(key) {
                PaletteEvent::None => {}
                PaletteEvent::Command(input) => self.run_command(&input),
                PaletteEvent::Filter(text) => {
                    self.views.top_mut().set_filter(Filter::contains(&text));
                }
                PaletteEvent::FilterCancelled => {
                    self.views.top_mut().set_filter(None);
                }
                PaletteEvent::Where(text) => {
                    let ctx = self.shared.context(true);
                    let flash = match self.views.top_mut() {
                        View::Workspace(workspace) => workspace.set_filter(text, &ctx),
                        _ => None,
                    };
                    if let Some(flash) = flash {
                        self.show(flash);
                    }
                }
            }
            return;
        }
        // Copying is not text input, so it works while a panel captures keys.
        if is_copy(key) {
            match self.views.top_mut().copy_text() {
                Some(text) => self.copy(text),
                None => self.report("nothing selected to copy".into()),
            }
            return;
        }
        let capturing = self.views.top().captures_input();
        if !capturing && self.handle_reserved(key) {
            return;
        }
        let ctx = self.shared.context(true);
        match self.views.top_mut().handle_key(key, &ctx) {
            KeyOutcome::Consumed => {}
            KeyOutcome::Action(action) => self.apply(action),
            KeyOutcome::Ignored if capturing => {}
            KeyOutcome::Ignored => match key.code {
                KeyCode::Char(':') => self.palette.open(palette::Mode::Command),
                KeyCode::Char('/') => {
                    if let Some((mode, text)) = self.views.top().filter_mode() {
                        let text = text.to_owned();
                        if mode == palette::Mode::Filter {
                            self.views.top_mut().set_filter(None);
                        }
                        self.palette.open_with(mode, &text);
                    }
                }
                KeyCode::Esc => self.apply(Action::Back),
                _ => {}
            },
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let ctx = self.shared.context(true);
        if let KeyOutcome::Action(action) = self.views.top_mut().handle_mouse(mouse, &ctx) {
            self.apply(action);
        }
    }

    /// Keys no view may override.
    fn handle_reserved(&mut self, key: KeyEvent) -> bool {
        let workspace = match self.views.top_mut() {
            View::Workspace(workspace) => Some(workspace),
            _ => None,
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Tab => workspace.into_iter().for_each(|w| w.cycle_focus(true)),
            KeyCode::BackTab => workspace.into_iter().for_each(|w| w.cycle_focus(false)),
            KeyCode::Char('b' | 'B') if ctrl => {
                let sidebar = if is_shifted(key) {
                    Sidebar::Stats
                } else {
                    Sidebar::Catalog
                };
                workspace.into_iter().for_each(|w| w.toggle(sidebar));
            }
            _ => return false,
        }
        true
    }

    fn run_command(&mut self, input: &str) {
        let action = Command::parse(input, &self.shared.connections)
            .and_then(|command| command.into_action(self.views.top().connection()));
        match action {
            Ok(action) => self.apply(action),
            Err(error) => self.report(error.to_string()),
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Open(Route::Connections(filter)) | Action::Jump(Route::Connections(filter)) => {
                self.leave_all(filter);
            }
            Action::Open(route) => self.open(route),
            Action::Jump(route) => {
                self.leave_all(None);
                self.open(route);
            }
            Action::Back => {
                if let Some(view) = self.views.pop() {
                    self.retire(view);
                }
            }
            Action::Disconnect(id) => {
                self.shared.connections.disconnect(&id);
                // Views under a closed connection would reconnect on the next key.
                if self.views.top().connection() == Some(&id) {
                    self.leave_all(None);
                }
                // Dropping the session cancels its runs and removes their results.
                self.sessions.remove(&id);
            }
            Action::Report(message) => self.report(message),
            Action::Copy(text) => self.copy(text),
            Action::Save(path) => {
                let ctx = self.shared.context(true);
                let flash = match self.views.top() {
                    View::Workspace(workspace) => workspace.save(&path, &ctx),
                    _ => Some(Flash::Error("open a query's results to save them".into())),
                };
                if let Some(flash) = flash {
                    self.show(flash);
                }
            }
            Action::Quit => self.quit = true,
        }
    }

    fn leave_all(&mut self, filter: Option<Filter>) {
        for view in self.views.reset(filter) {
            self.retire(view);
        }
    }

    fn open(&mut self, route: Route) {
        self.connect(&route);
        if let Route::Workspace { connection, query } = &route
            && let Some(mut workspace) = self.sessions.remove(connection)
        {
            match workspace.reopen(query) {
                Ok(()) => self.views.push(View::Workspace(workspace)),
                Err(e) => {
                    self.sessions.insert(connection.clone(), workspace);
                    self.report(format!("{e:#}"));
                }
            }
            return;
        }
        let ctx = self.shared.context(true);
        match View::open(route, &ctx) {
            Ok(view) => self.views.push(view),
            Err(e) => self.report(format!("{e:#}")),
        }
    }

    /// Connect in the background the first time a route needs a connection.
    fn connect(&mut self, route: &Route) {
        let Some(id) = route.connection() else {
            return;
        };
        let Some(source) = self.shared.connections.begin_connect(id) else {
            return;
        };
        let id = id.clone();
        let messages = self.shared.messages.clone();
        tokio::spawn(async move {
            let result = source.connect().await;
            let _ = messages.send(Msg::Connected { id, result });
        });
    }

    fn handle_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Connected { id, result } => {
                self.shared.connections.finish_connect(&id, result);
                if let Some(ConnectionState::Failed(message)) = self.shared.connections.state(&id) {
                    let message = format!("{id}: {message}");
                    self.report(message);
                }
            }
            msg => {
                let ctx = self.shared.context(true);
                let top = self.views.top_mut();
                let on_top = matches!(top, View::Workspace(w) if w.owns(&msg));
                let session = (!on_top)
                    .then(|| self.sessions.values_mut().find(|w| w.owns(&msg)))
                    .flatten();
                let flash = match session {
                    Some(workspace) => workspace.handle_msg(msg, &ctx),
                    None => top.handle_msg(msg, &ctx),
                };
                if let Some(flash) = flash {
                    self.show(flash);
                }
            }
        }
    }

    pub fn render(&mut self, frame: &mut Frame) {
        let [header_area, palette_area, body] = Layout::vertical([
            Constraint::Length(header::HEIGHT),
            Constraint::Length(palette::HEIGHT),
            Constraint::Min(0),
        ])
        .areas(frame.area());

        // The open palette takes every key, so it alone shows focus.
        let ctx = self.shared.context(!self.palette.is_open());
        self.views.top_mut().render(frame, body, &ctx);

        let view = self.views.top();
        let view_hints = match view {
            View::Workspace(_) => Workspace::HINTS,
            _ => &[],
        };
        let header = Header {
            connections: &self.shared.connections,
            view,
            hints: [view.hints(), view_hints, GLOBAL_HINTS],
        };
        frame.render_widget(header, header_area);
        self.palette.render(frame, palette_area, view.status());
    }
}

/// Ctrl-Shift-C, however the terminal reports the shifted letter.
fn is_copy(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c' | 'C'))
        && is_shifted(key)
}

/// Whether a letter key was shifted, as an uppercase letter or a modifier.
fn is_shifted(key: KeyEvent) -> bool {
    key.modifiers.contains(KeyModifiers::SHIFT)
        || matches!(key.code, KeyCode::Char(c) if c.is_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        component::testing::shared,
        connection::{
            Connections,
            testing::{connections, group, id},
        },
    };
    use ratatui::{Terminal, backend::TestBackend};
    use tokio::sync::mpsc::UnboundedReceiver;

    fn app_with(connections: Connections, start: Option<Target>) -> (App, UnboundedReceiver<Msg>) {
        let (shared, rx) = shared(connections);
        (App::new(shared, start), rx)
    }

    fn app() -> (App, UnboundedReceiver<Msg>) {
        app_with(connections(&["pg", "ch"]), None)
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn type_text(app: &mut App, text: &str) {
        text.chars().for_each(|c| press(app, KeyCode::Char(c)));
    }

    fn type_command(app: &mut App, command: &str) {
        press(app, KeyCode::Char(':'));
        type_text(app, command);
        press(app, KeyCode::Enter);
    }

    fn screen(app: &mut App, width: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal.draw(|frame| app.render(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        buffer
            .content
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[tokio::test]
    async fn enter_descends_and_esc_returns() {
        let (mut app, mut rx) = app();
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);

        let Some(msg) = rx.recv().await else {
            panic!("expected a connection result");
        };
        app.handle(Event::Msg(msg));
        let ch = id(&app.shared.connections, "ch");
        assert!(matches!(
            app.shared.connections.state(&ch),
            Some(ConnectionState::Failed(_))
        ));
        press(&mut app, KeyCode::Esc);
    }

    #[tokio::test]
    async fn palette_jumps_and_reports_errors() {
        let (mut app, _rx) = app();
        type_command(&mut app, "conn pg");

        type_command(&mut app, "conn nope");
        assert!(screen(&mut app, 120).contains("no connection named `nope`"));

        type_command(&mut app, "conn");
        type_command(&mut app, "q");
        assert!(app.should_quit());
    }

    #[tokio::test]
    async fn ambiguous_name_opens_the_filtered_connection_list() {
        let connections = Connections::new([
            group("~/.config/quill/connections.toml", &["pg", "ch"]),
            group("/work/app/.quill.toml", &["pg"]),
        ]);
        let start = connections.lookup("pg");
        let (mut app, _rx) = app_with(connections, start);
        let screen = screen(&mut app, 120);
        assert!(screen.contains("connections (2) = pg"));
        assert!(screen.contains("~/.config/quill/connections.toml"));
        assert!(screen.contains("/work/app/.quill.toml"));
        assert!(!screen.contains("  ch"));
    }

    #[tokio::test]
    async fn slash_filters_the_list_live_and_esc_clears() {
        let (mut app, _rx) = app();
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "c");
        assert!(screen(&mut app, 120).contains("connections (1) /c"));
        press(&mut app, KeyCode::Enter);
        assert!(screen(&mut app, 120).contains("connections (1) /c"));
        press(&mut app, KeyCode::Char('/'));
        press(&mut app, KeyCode::Esc);
        assert!(screen(&mut app, 120).contains("connections (2)"));
    }

    #[tokio::test]
    async fn disconnect_closes_and_leaves_the_connection() {
        let (mut app, _rx) = app();
        type_command(&mut app, "conn pg");
        let pg = id(&app.shared.connections, "pg");
        assert!(matches!(
            app.shared.connections.state(&pg),
            Some(ConnectionState::Connecting)
        ));

        type_command(&mut app, "disconnect");
        assert!(matches!(
            app.shared.connections.state(&pg),
            Some(ConnectionState::Idle)
        ));

        type_command(&mut app, "close");
        assert!(screen(&mut app, 120).contains("no connection is open here"));
    }

    #[tokio::test]
    async fn queries_are_added_opened_edited_and_saved_on_leave() {
        let dir = tempfile::tempdir().unwrap();
        let source: Box<dyn crate::ConnectionSource> =
            Box::new(crate::connection::testing::Offline(dir.path().join("pg")));
        let connections = Connections::new([crate::ConnectionGroup {
            label: "home".into(),
            connections: vec![("pg".into(), source)],
        }]);
        let start = connections.lookup("pg");
        let (mut app, _rx) = app_with(connections, start);

        press(&mut app, KeyCode::Char('a'));
        assert!(app.views.top().captures_input());
        type_text(&mut app, "daily");
        press(&mut app, KeyCode::Enter);
        assert!(dir.path().join("pg/daily.sql").is_file());

        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('e'));
        // Insert mode takes every key, including the palette and Tab.
        type_text(&mut app, "SELECT 1 :");
        press(&mut app, KeyCode::Tab);
        assert!(!app.palette.is_open());
        assert!(screen(&mut app, 140).contains("INSERT"));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        let saved = std::fs::read_to_string(dir.path().join("pg/daily.sql")).unwrap();
        // Tab reached the editor, which indents the line.
        assert!(saved.starts_with("    SELECT 1 :"), "{saved:?}");
    }

    #[tokio::test]
    async fn reserved_keys_win_over_the_view() {
        let (mut app, _rx) = app();
        type_command(&mut app, "conn pg");
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit());
    }

    /// Receive background messages until `done` holds, redrawing between them.
    async fn pump(
        app: &mut App,
        rx: &mut UnboundedReceiver<Msg>,
        done: impl Fn(&str) -> bool,
    ) -> String {
        let deadline = std::time::Duration::from_secs(10);
        tokio::time::timeout(deadline, async {
            loop {
                let screen = screen(app, 160);
                if done(&screen) {
                    return screen;
                }
                let msg = rx.recv().await.expect("channel open");
                app.handle(Event::Msg(msg));
            }
        })
        .await
        .expect("condition before timeout")
    }

    #[tokio::test]
    async fn runs_a_query_and_pages_through_the_results() {
        use quill_drivers::{
            ConnectionPool, settings::SqliteSettings, sqlite::SqliteConnectionPool,
        };

        let dir = tempfile::tempdir().unwrap();
        let queries = dir.path().join("queries");
        std::fs::create_dir_all(&queries).unwrap();
        std::fs::write(
            queries.join("count.sql"),
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM n WHERE x < 1000) \
             SELECT x, 'row ' || x AS label FROM n",
        )
        .unwrap();
        let source: Box<dyn crate::ConnectionSource> =
            Box::new(crate::connection::testing::Offline(queries));
        let connections = Connections::new([crate::ConnectionGroup {
            label: "home".into(),
            connections: vec![("lite".into(), source)],
        }]);
        let start = connections.lookup("lite");
        let (mut shared, mut rx) = shared(connections);
        shared.results_dir = dir.path().join("results");
        let mut app = App::new(shared, start);

        // Swap the offline attempt for a real in-memory SQLite pool.
        let lite = id(&app.shared.connections, "lite");
        let settings = SqliteSettings {
            path: ":memory:".into(),
            params: Default::default(),
        };
        let pool = SqliteConnectionPool::connect(&settings).await.unwrap();
        app.shared
            .connections
            .finish_connect(&lite, Ok(std::sync::Arc::new(pool)));

        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::F(5));
        let first = pump(&mut app, &mut rx, |s| s.contains("row 1 ")).await;
        assert!(first.contains("results · 1,000 rows"), "{first}");
        assert!(first.contains("label"));

        // Focus results, jump to the end; the last page loads in the background.
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Char('G'));
        let last = pump(&mut app, &mut rx, |s| s.contains("row 1000")).await;
        assert!(!last.contains("row 1 "), "{last}");

        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Enter);
        assert!(screen(&mut app, 160).contains("label · row 1000"));
        press(&mut app, KeyCode::Esc);
        assert!(!screen(&mut app, 160).contains("label · row 1000"));

        // Statistics profile the whole result once the drain finishes.
        let stats = pump(&mut app, &mut rx, |s| s.contains("scanned 1,000 rows")).await;
        assert!(stats.contains("nulls"), "{stats}");

        // Sort descending on `x`: the largest value comes first.
        press(&mut app, KeyCode::Char('0'));
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('s'));
        press(&mut app, KeyCode::Char('s'));
        let sorted = pump(&mut app, &mut rx, |s| {
            s.contains("sort x ▼") && s.contains("row 1000")
        })
        .await;
        let first_row = sorted
            .lines()
            .find(|l| l.contains("┃1 "))
            .unwrap_or_default();
        assert!(first_row.contains("row 1000"), "{sorted}");

        // A results condition re-drains just the matching rows.
        press(&mut app, KeyCode::Char('/'));
        type_text(&mut app, "x <= 3");
        press(&mut app, KeyCode::Enter);
        pump(&mut app, &mut rx, |s| s.contains("results · 3 rows")).await;

        // The cursor cell's full value is what Ctrl-Shift-C or `y` copies.
        press(&mut app, KeyCode::Char('g'));
        press(&mut app, KeyCode::Char('l'));
        pump(&mut app, &mut rx, |s| s.contains("row 3")).await;
        assert_eq!(app.views.top_mut().copy_text().as_deref(), Some("row 3"));

        let out = dir.path().join("out.csv");
        type_command(&mut app, &format!("save {}", out.display()));
        pump(&mut app, &mut rx, |s| s.contains("saved ")).await;
        let saved = std::fs::read_to_string(&out).unwrap();
        assert_eq!(
            saved.lines().collect::<Vec<_>>(),
            ["x,label", "3,row 3", "2,row 2", "1,row 1"]
        );

        // Leaving the workspace keeps the run's result files; disconnecting
        // removes them.
        let runs = || {
            std::fs::read_dir(dir.path().join("results"))
                .unwrap()
                .count()
        };
        press(&mut app, KeyCode::Esc);
        assert_eq!(runs(), 1);
        type_command(&mut app, "disconnect");
        assert_eq!(runs(), 0);
    }

    #[tokio::test]
    async fn tabs_keep_their_results_until_disconnect() {
        use quill_drivers::{
            ConnectionPool, settings::SqliteSettings, sqlite::SqliteConnectionPool,
        };

        let dir = tempfile::tempdir().unwrap();
        let queries = dir.path().join("queries");
        std::fs::create_dir_all(&queries).unwrap();
        std::fs::write(queries.join("alpha.sql"), "SELECT 'from alpha' AS a").unwrap();
        std::fs::write(queries.join("beta.sql"), "SELECT 2").unwrap();
        let source: Box<dyn crate::ConnectionSource> =
            Box::new(crate::connection::testing::Offline(queries));
        let connections = Connections::new([crate::ConnectionGroup {
            label: "home".into(),
            connections: vec![("lite".into(), source)],
        }]);
        let start = connections.lookup("lite");
        let (mut shared, mut rx) = shared(connections);
        shared.results_dir = dir.path().join("results");
        let mut app = App::new(shared, start);
        let lite = id(&app.shared.connections, "lite");
        let settings = SqliteSettings {
            path: ":memory:".into(),
            params: Default::default(),
        };
        let pool = SqliteConnectionPool::connect(&settings).await.unwrap();
        app.shared
            .connections
            .finish_connect(&lite, Ok(std::sync::Arc::new(pool)));
        let ctrl_k = |app: &mut App, then| {
            app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
            press(app, KeyCode::Char(then));
        };

        press(&mut app, KeyCode::Enter);
        let top = screen(&mut app, 160);
        let border = top.lines().find(|l| l.contains("┏")).unwrap_or_default();
        assert!(border.contains(" alpha │ beta "), "{top}");

        press(&mut app, KeyCode::F(5));
        pump(&mut app, &mut rx, |s| s.contains("results · 1 rows")).await;
        ctrl_k(&mut app, ']');
        assert!(!screen(&mut app, 160).contains("results · 1 rows"));
        ctrl_k(&mut app, '[');
        assert!(screen(&mut app, 160).contains("results · 1 rows"));

        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Enter);
        assert!(screen(&mut app, 160).contains("results · 1 rows"));

        let runs = || {
            std::fs::read_dir(dir.path().join("results"))
                .unwrap()
                .count()
        };
        assert_eq!(runs(), 1);
        type_command(&mut app, "disconnect");
        assert_eq!(runs(), 0);
    }

    #[tokio::test]
    async fn catalog_puts_a_select_into_the_editor() {
        use futures::StreamExt;
        use quill_core::query::QueryRequest;
        use quill_drivers::{
            ConnectionPool, settings::SqliteSettings, sqlite::SqliteConnectionPool,
        };

        let dir = tempfile::tempdir().unwrap();
        let queries = dir.path().join("queries");
        std::fs::create_dir_all(&queries).unwrap();
        std::fs::write(queries.join("blank.sql"), "").unwrap();
        let source: Box<dyn crate::ConnectionSource> =
            Box::new(crate::connection::testing::Offline(queries));
        let connections = Connections::new([crate::ConnectionGroup {
            label: "home".into(),
            connections: vec![("lite".into(), source)],
        }]);
        let start = connections.lookup("lite");
        let (shared, mut rx) = shared(connections);
        let mut app = App::new(shared, start);

        let settings = SqliteSettings {
            path: ":memory:".into(),
            params: Default::default(),
        };
        let pool = SqliteConnectionPool::connect(&settings).await.unwrap();
        let mut created = pool
            .execute(QueryRequest::new("CREATE TABLE people (x INTEGER)"))
            .await
            .unwrap();
        while created.events.next().await.is_some() {}
        let lite = id(&app.shared.connections, "lite");
        app.shared
            .connections
            .finish_connect(&lite, Ok(std::sync::Arc::new(pool)));

        press(&mut app, KeyCode::Enter);
        // Focus the catalog; its single catalog opens to show schemas.
        press(&mut app, KeyCode::BackTab);
        pump(&mut app, &mut rx, |s| s.contains("▸ main")).await;
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('l'));
        pump(&mut app, &mut rx, |s| s.contains("▸ people")).await;
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        let screen = screen(&mut app, 160);
        assert!(
            screen.contains(r#"SELECT * FROM "main"."people" LIMIT 100"#),
            "{screen}"
        );
    }

    #[test]
    fn ctrl_shift_c_is_copy_however_shift_is_reported() {
        let ctrl = KeyModifiers::CONTROL;
        assert!(is_copy(KeyEvent::new(
            KeyCode::Char('C'),
            ctrl | KeyModifiers::SHIFT
        )));
        assert!(is_copy(KeyEvent::new(
            KeyCode::Char('c'),
            ctrl | KeyModifiers::SHIFT
        )));
        assert!(!is_copy(KeyEvent::new(KeyCode::Char('c'), ctrl)));
        assert!(is_shifted(KeyEvent::new(KeyCode::Char('B'), ctrl)));
        assert!(!is_shifted(KeyEvent::new(KeyCode::Char('b'), ctrl)));
    }
}
