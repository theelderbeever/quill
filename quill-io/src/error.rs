use std::{io, path::PathBuf};

use arrow::error::ArrowError;

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o on {path}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error(transparent)]
    Arrow(#[from] ArrowError),

    /// The query stream failed; kept so callers can show it beside the truncated result.
    #[error(transparent)]
    Query(#[from] quill_core::error::Error),

    #[error("result already has a column named {0}")]
    ReservedColumn(&'static str),

    #[error("result set {result_set} sent data before its schema")]
    UnexpectedEvent { result_set: usize },
}

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>) -> impl FnOnce(io::Error) -> Self {
        let path = path.into();
        move |source| Self::Io { path, source }
    }
}
