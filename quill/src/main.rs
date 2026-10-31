use std::path::PathBuf;

use anyhow::anyhow;
use clap::Parser;
use quill::settings::Settings;
use quill_tui::{Connections, Options};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Config file loaded after the default locations; repeatable.
    #[arg(long = "config", value_name = "PATH")]
    config: Vec<PathBuf>,
    /// Open this connection's queries at start.
    connection: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let settings = Settings::load(cli.config)?;
    let connections = Connections::new(settings.connection);
    let start = cli
        .connection
        .map(|name| {
            connections
                .resolve(&name)
                .cloned()
                .ok_or_else(|| anyhow!("no connection named `{name}` in config"))
        })
        .transpose()?;
    let options = Options {
        redraw: settings.ui.redraw_interval(),
    };
    quill_tui::run(connections, start, options).await
}
