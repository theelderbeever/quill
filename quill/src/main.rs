use std::path::PathBuf;

use anyhow::{anyhow, bail};
use clap::{CommandFactory, FromArgMatches, Parser, parser::ValueSource};
use quill::{connections::ConnectionFile, home::QuillHome, settings::Settings};
use quill_io::feather::ByteLimit;
use quill_tui::{Connections, Options};

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Project connections file, read alongside `$QUILL_HOME/connections.toml`.
    #[arg(
        short = 'f',
        long = "config",
        value_name = "PATH",
        default_value = ".quill.toml"
    )]
    config: PathBuf,
    /// Open this connection's queries at start.
    connection: Option<String>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let matches = Cli::command().get_matches();
    // The default project file may be absent; one named explicitly must exist.
    let explicit = matches.value_source("config") == Some(ValueSource::CommandLine);
    let cli = Cli::from_arg_matches(&matches)?;

    let home = QuillHome::resolve()?;
    let settings = Settings::load(&home)?;
    let mut files: Vec<ConnectionFile> = ConnectionFile::load(&home.connections())?
        .into_iter()
        .collect();
    match ConnectionFile::load(&cli.config)? {
        Some(project) if files.iter().any(|f| f.path() == project.path()) => {}
        Some(project) => files.push(project),
        None if explicit => bail!("{} does not exist", cli.config.display()),
        None => {}
    }
    let connections = Connections::new(files.into_iter().map(|f| f.into_group(&home)));
    let start = cli
        .connection
        .map(|name| {
            connections
                .lookup(&name)
                .ok_or_else(|| anyhow!("no connection named `{name}` in config"))
        })
        .transpose()?;
    let options = Options {
        redraw: settings.ui.redraw_interval(),
        autosave: settings.ui.autosave_delay(),
        results_dir: settings.results.dir(),
        byte_limit: settings.results.max_bytes.map(ByteLimit::from_non_zero),
        page_rows: settings.ui.page_rows,
        max_column_width: settings.ui.max_column_width,
    };
    quill_tui::run(connections, start, options).await
}
