//! A k9s-style terminal UI over Quill's connections.
mod action;
mod app;
mod command;
mod component;
mod connection;
mod event;
mod filter;
mod format;
mod path;
mod store;
mod task;
mod view;
mod widget;

use std::{num::NonZeroU64, path::PathBuf, time::Duration};

use anyhow::Context as _;
use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    terminal::supports_keyboard_enhancement,
};
use quill_io::feather::ByteLimit;
use tokio::sync::mpsc;

pub use connection::{ConnectionGroup, ConnectionId, ConnectionSource, Connections, Target, quote};

use app::App;
use component::Shared;
use event::{Event, Events};

pub struct Options {
    /// How often the screen redraws while something has changed.
    pub redraw: Duration,
    /// Pause after the last edit before a query is saved.
    pub autosave: Duration,
    /// Parent of the per-session results directory.
    pub results_dir: PathBuf,
    /// Bytes drained per run before it stops; unlimited when `None`.
    pub byte_limit: Option<ByteLimit>,
    /// Result rows fetched per page.
    pub page_rows: NonZeroU64,
    /// Widest an auto-sized result column grows, in cells.
    pub max_column_width: u16,
}

/// Take over the terminal until the user quits, restoring it on return or panic.
///
/// ```no_run
/// # use std::{num::NonZeroU64, path::Path, sync::Arc, time::Duration};
/// # use quill_core::error::Error;
/// # use quill_drivers::{ConnectionPool, PoolFuture};
/// # use quill_tui::{ConnectionGroup, ConnectionSource, Connections, Options};
/// # struct Offline;
/// # impl ConnectionSource for Offline {
/// #     fn user(&self) -> Option<&str> { None }
/// #     fn kind(&self) -> &'static str { "offline" }
/// #     fn connect(&self) -> PoolFuture<'_, Arc<dyn ConnectionPool>> {
/// #         Box::pin(async { Err(Error::UnsupportedOperation("connect")) })
/// #     }
/// #     fn query_dir(&self) -> &Path { Path::new("queries/pg_prod") }
/// # }
/// # #[tokio::main]
/// # async fn main() -> anyhow::Result<()> {
/// let connections = Connections::new([ConnectionGroup {
///     label: "~/.config/quill/connections.toml".into(),
///     connections: vec![("pg_prod".into(), Box::new(Offline) as Box<dyn ConnectionSource>)],
/// }]);
/// let start = connections.lookup("pg_prod");
/// let options = Options {
///     redraw: Duration::from_millis(100),
///     autosave: Duration::from_millis(750),
///     results_dir: std::env::temp_dir(),
///     byte_limit: None,
///     page_rows: NonZeroU64::new(500).unwrap(),
///     max_column_width: 40,
/// };
/// quill_tui::run(connections, start, options).await
/// # }
/// ```
pub async fn run(
    connections: Connections,
    start: Option<Target>,
    options: Options,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(&options.results_dir)
        .with_context(|| format!("create {}", options.results_dir.display()))?;
    let session = tempfile::Builder::new()
        .prefix("quill-")
        .tempdir_in(&options.results_dir)
        .with_context(|| {
            format!(
                "create a session directory in {}",
                options.results_dir.display()
            )
        })?;

    let mut terminal = ratatui::try_init()?;
    let restore = Restore {
        keyboard: supports_keyboard_enhancement().unwrap_or(false),
    };
    crossterm::execute!(std::io::stdout(), EnableMouseCapture)?;
    if restore.keyboard {
        // Lets Ctrl-Enter arrive distinct from Enter.
        let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
        crossterm::execute!(std::io::stdout(), PushKeyboardEnhancementFlags(flags))?;
    }

    let (messages, received) = mpsc::unbounded_channel();
    let redraw = options.redraw;
    let shared = Shared {
        connections,
        options,
        results_dir: session.path().to_path_buf(),
        messages,
    };
    let mut app = App::new(shared, start);
    let mut events = Events::new(received, redraw);
    let result = async {
        while !app.should_quit() {
            match events.next().await? {
                Event::Tick => {
                    app.handle(Event::Tick);
                    if app.take_dirty() {
                        terminal.draw(|frame| app.render(frame))?;
                    }
                }
                event => app.handle(event),
            }
        }
        anyhow::Ok(())
    }
    .await;
    app.close();
    drop(restore);
    result
}

/// Releases the mouse and keyboard modes and leaves raw mode and the
/// alternate screen on every exit path.
struct Restore {
    keyboard: bool,
}

impl Drop for Restore {
    fn drop(&mut self) {
        // Also runs while unwinding a panic, after ratatui's own panic hook.
        if self.keyboard {
            let _ = crossterm::execute!(std::io::stdout(), PopKeyboardEnhancementFlags);
        }
        let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
        ratatui::restore();
    }
}
