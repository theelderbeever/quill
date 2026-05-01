use thiserror::Error;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("config: {0}")]
    Config(String),

    #[error("driver connect failed for {connection}")]
    DriverConnect {
        connection: String,
        #[source]
        source: anyhow::Error,
    },

    #[error("driver execute failed")]
    DriverExecute(#[source] anyhow::Error),

    #[error("federation")]
    Federation(#[source] anyhow::Error),

    #[error("gui channel closed")]
    GuiChannel,
}
