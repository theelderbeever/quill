//! Top-level state: routes keys, applies actions and draws the frame.
use crossterm::event::{
    Event as TerminalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent,
    MouseEventKind,
};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
};
use tokio::sync::mpsc::UnboundedSender;

use crate::{
    action::Action,
    command::Command,
    component::{Component, Context, KeyHint, KeyOutcome},
    connection::{ConnectionName, ConnectionState, Connections},
    event::{Event, Msg},
    view::{
        Route, View, ViewStack,
        workspace::{Direction, Sidebar, Workspace},
    },
    widget::{
        header::{self, Header},
        palette::{self, Flash, Palette},
    },
};

const GLOBAL_HINTS: &[KeyHint] = &[
    KeyHint::new(":", "command"),
    KeyHint::new("esc", "back"),
    KeyHint::new("ctrl-c", "quit"),
];

pub struct App {
    connections: Connections,
    views: ViewStack,
    palette: Palette,
    messages: UnboundedSender<Msg>,
    dirty: bool,
    quit: bool,
}

impl App {
    /// Start at `start`'s query list, or the connection list without one.
    pub fn new(
        connections: Connections,
        start: Option<ConnectionName>,
        messages: UnboundedSender<Msg>,
    ) -> Self {
        let mut app = Self {
            connections,
            views: ViewStack::new(),
            palette: Palette::new(),
            messages,
            dirty: true,
            quit: false,
        };
        if let Some(name) = start {
            app.apply(Action::Jump(Route::Queries(name)));
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
            Event::Tick => return,
        }
        self.dirty = true;
    }

    /// Precedence: open palette, reserved keys, focused view, then fallbacks
    /// the view left unhandled.
    fn handle_key(&mut self, key: KeyEvent) {
        if self.palette.is_open() {
            if let Some(input) = self.palette.handle_key(key) {
                self.run_command(&input);
            }
            return;
        }
        if !self.views.top().captures_input() && self.handle_reserved(key) {
            return;
        }
        let ctx = Context {
            connections: &self.connections,
            focused: true,
        };
        match self.views.top_mut().handle_key(key, &ctx) {
            KeyOutcome::Consumed => {}
            KeyOutcome::Action(action) => self.apply(action),
            KeyOutcome::Ignored if self.views.top().captures_input() => {}
            KeyOutcome::Ignored => match key.code {
                KeyCode::Char(':') => self.palette.open(),
                KeyCode::Esc => self.apply(Action::Back),
                _ => {}
            },
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let ctx = Context {
            connections: &self.connections,
            focused: true,
        };
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
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.quit = true;
            }
            KeyCode::Char(c @ ('h' | 'j' | 'k' | 'l'))
                if key.modifiers.contains(KeyModifiers::CONTROL) && workspace.is_some() =>
            {
                let direction = match c {
                    'h' => Direction::Left,
                    'j' => Direction::Down,
                    'k' => Direction::Up,
                    _ => Direction::Right,
                };
                workspace.into_iter().for_each(|w| w.move_focus(direction));
            }
            KeyCode::Tab => workspace.into_iter().for_each(|w| w.cycle_focus(true)),
            KeyCode::BackTab => workspace.into_iter().for_each(|w| w.cycle_focus(false)),
            KeyCode::F(2) => workspace
                .into_iter()
                .for_each(|w| w.toggle(Sidebar::Catalog)),
            KeyCode::F(3) => workspace.into_iter().for_each(|w| w.toggle(Sidebar::Stats)),
            _ => return false,
        }
        true
    }

    fn run_command(&mut self, input: &str) {
        let action = Command::parse(input, &self.connections)
            .and_then(|command| command.into_action(self.views.top().connection()));
        match action {
            Ok(action) => self.apply(action),
            Err(error) => self.palette.flash(Flash::Error(error.to_string())),
        }
    }

    fn apply(&mut self, action: Action) {
        match action {
            Action::Open(route) => {
                self.connect(&route);
                self.views.push(route);
            }
            Action::Jump(route) => {
                self.connect(&route);
                self.views.jump(route);
            }
            Action::Back => self.views.pop(),
            Action::Disconnect(name) => {
                self.connections.disconnect(&name);
                // Views under a closed connection would reconnect on the next key.
                if self.views.top().connection() == Some(&name) {
                    self.views.jump(Route::Connections);
                }
            }
            Action::Quit => self.quit = true,
        }
    }

    /// Connect in the background the first time a route needs a connection.
    fn connect(&mut self, route: &Route) {
        let Some(name) = route.connection() else {
            return;
        };
        let Some(source) = self.connections.begin_connect(name) else {
            return;
        };
        let name = name.clone();
        let messages = self.messages.clone();
        tokio::spawn(async move {
            let result = source.connect().await;
            let _ = messages.send(Msg::Connected { name, result });
        });
    }

    fn handle_msg(&mut self, msg: Msg) {
        match msg {
            Msg::Connected { name, result } => {
                self.connections.finish_connect(&name, result);
                if let Some(ConnectionState::Failed(message)) = self.connections.state(&name) {
                    self.palette
                        .flash(Flash::Error(format!("{name}: {message}")));
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
        let ctx = Context {
            connections: &self.connections,
            focused: !self.palette.is_open(),
        };
        self.views.top_mut().render(frame, body, &ctx);

        let view = self.views.top();
        let view_hints = match view {
            View::Workspace(_) => Workspace::HINTS,
            _ => &[],
        };
        let header = Header {
            connections: &self.connections,
            view,
            hints: [view.hints(), view_hints, GLOBAL_HINTS],
        };
        frame.render_widget(header, header_area);
        frame.render_widget(&self.palette, palette_area);
        if let Some(cursor) = self.palette.cursor(palette_area) {
            frame.set_cursor_position(cursor);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::testing::connections;
    use ratatui::{Terminal, backend::TestBackend};
    use tokio::sync::mpsc;

    fn app() -> (App, mpsc::UnboundedReceiver<Msg>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (App::new(connections(&["pg", "ch"]), None, tx), rx)
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    fn type_command(app: &mut App, command: &str) {
        press(app, KeyCode::Char(':'));
        command.chars().for_each(|c| press(app, KeyCode::Char(c)));
        press(app, KeyCode::Enter);
    }

    fn crumbs(app: &App) -> Vec<&str> {
        app.views.top().breadcrumb().collect()
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
        assert_eq!(crumbs(&app), ["ch"]);

        let Some(msg) = rx.recv().await else {
            panic!("expected a connection result");
        };
        app.handle(Event::Msg(msg));
        let ch = app.connections.resolve("ch").unwrap();
        assert!(matches!(
            app.connections.state(ch),
            Some(ConnectionState::Failed(_))
        ));

        press(&mut app, KeyCode::Enter);
        assert_eq!(crumbs(&app), ["ch", "scratch"]);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        assert!(crumbs(&app).is_empty());
    }

    #[tokio::test]
    async fn palette_jumps_and_reports_errors() {
        let (mut app, _rx) = app();
        type_command(&mut app, "conn pg");
        assert_eq!(crumbs(&app), ["pg"]);

        type_command(&mut app, "conn nope");
        assert_eq!(crumbs(&app), ["pg"]);
        assert!(screen(&mut app, 120).contains("no connection named `nope`"));

        type_command(&mut app, "conn");
        assert!(crumbs(&app).is_empty());
        type_command(&mut app, "q");
        assert!(app.should_quit());
    }

    #[tokio::test]
    async fn header_shows_breadcrumb_and_focused_hints() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let connections = connections(&["pg"]);
        let start = connections.resolve("pg").cloned();
        let mut app = App::new(connections, start, tx);
        let queries = screen(&mut app, 120);
        assert!(queries.contains("<enter>  open"));
        assert!(queries.contains("pg · offline"));

        press(&mut app, KeyCode::Enter);
        let screen = screen(&mut app, 140);
        assert!(screen.contains("pg › scratch"));
        assert!(screen.contains("<tab>"));
        assert!(screen.contains("catalog"));
        assert!(screen.contains("stats"));
    }

    #[tokio::test]
    async fn disconnect_closes_and_leaves_the_connection() {
        let (mut app, _rx) = app();
        type_command(&mut app, "conn pg");
        let pg = app.connections.resolve("pg").cloned().unwrap();
        assert!(matches!(
            app.connections.state(&pg),
            Some(ConnectionState::Connecting)
        ));

        type_command(&mut app, "disconnect");
        assert!(crumbs(&app).is_empty());
        assert!(matches!(
            app.connections.state(&pg),
            Some(ConnectionState::Idle)
        ));

        type_command(&mut app, "close");
        assert!(screen(&mut app, 120).contains("no connection is open here"));
    }

    #[tokio::test]
    async fn insert_mode_captures_every_key_until_esc() {
        let (mut app, _rx) = app();
        type_command(&mut app, "conn pg");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('i'));
        assert!(app.views.top().captures_input());

        press(&mut app, KeyCode::Char(':'));
        press(&mut app, KeyCode::Tab);
        assert!(!app.palette.is_open());
        assert!(screen(&mut app, 140).contains("INSERT"));

        press(&mut app, KeyCode::Esc);
        assert!(!app.views.top().captures_input());
        assert_eq!(crumbs(&app), ["pg", "scratch"]);
        press(&mut app, KeyCode::Esc);
        assert_eq!(crumbs(&app), ["pg"]);
    }

    #[tokio::test]
    async fn reserved_keys_win_over_the_view() {
        let (mut app, _rx) = app();
        type_command(&mut app, "conn pg");
        press(&mut app, KeyCode::Enter);
        let View::Workspace(_) = app.views.top() else {
            panic!("expected the workspace");
        };
        press(&mut app, KeyCode::Tab);
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL));
        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(app.should_quit());
    }
}
