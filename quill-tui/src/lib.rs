//! A k9s-style terminal UI over Quill's connections.
mod action;
mod app;
mod command;
mod component;
mod connection;
mod event;
mod view;
mod widget;

use std::time::Duration;

use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use tokio::sync::mpsc;

pub use connection::{ConnectionName, ConnectionSource, Connections};

use app::App;
use event::{Event, Events};

pub struct Options {
    /// How often the screen redraws while something has changed.
    pub redraw: Duration,
}

/// Take over the terminal until the user quits, restoring it on return or panic.
///
/// ```no_run
/// # use std::{sync::Arc, time::Duration};
/// # use quill_core::error::Error;
/// # use quill_drivers::{ConnectionPool, PoolFuture};
/// # use quill_tui::{ConnectionSource, Connections, Options};
/// # struct Offline;
/// # impl ConnectionSource for Offline {
/// #     fn kind(&self) -> &'static str { "offline" }
/// #     fn connect(&self) -> PoolFuture<'_, Arc<dyn ConnectionPool>> {
/// #         Box::pin(async { Err(Error::UnsupportedOperation("connect")) })
/// #     }
/// # }
/// # #[tokio::main]
/// # async fn main() -> anyhow::Result<()> {
/// let connections = Connections::new([("pg_prod".to_owned(), Offline)]);
/// let start = connections.resolve("pg_prod").cloned();
/// quill_tui::run(connections, start, Options { redraw: Duration::from_millis(100) }).await
/// # }
/// ```
pub async fn run(
    connections: Connections,
    start: Option<ConnectionName>,
    options: Options,
) -> anyhow::Result<()> {
    let mut terminal = ratatui::try_init()?;
    let _restore = Restore;
    crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
    let (messages, received) = mpsc::unbounded_channel();
    let mut app = App::new(connections, start, messages);
    let mut events = Events::new(received, options.redraw);
    while !app.should_quit() {
        match events.next().await? {
            Event::Tick if app.take_dirty() => {
                terminal.draw(|frame| app.render(frame))?;
            }
            event => app.handle(event),
        }
    }
    Ok(())
}

/// Releases the mouse and leaves raw mode and the alternate screen on every exit path.
struct Restore;

impl Drop for Restore {
    fn drop(&mut self) {
        // Also runs while unwinding a panic, after ratatui's own panic hook.
        let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
        ratatui::restore();
    }
}
