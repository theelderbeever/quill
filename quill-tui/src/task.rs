//! Background work that reports back to the UI loop as [`Msg`](crate::event::Msg)s.
pub mod catalog;
pub mod derive;
pub mod page;
pub mod run;
pub mod stats;

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use quill_core::query::{ExecutionControl, QueryId, ServerMetrics};
use quill_io::feather::StoredQuery;

/// Identifies one query run, so updates for a superseded run are dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RunId(u64);

impl std::fmt::Display for RunId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl RunId {
    pub fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

pub enum RunMsg {
    /// The driver accepted the query; cancellation now goes through `control`.
    Started {
        run: RunId,
        query: QueryId,
        control: Arc<dyn ExecutionControl>,
    },
    Finished {
        run: RunId,
        result: anyhow::Result<StoredQuery>,
    },
    Page {
        run: RunId,
        index: u64,
        result: anyhow::Result<page::Page>,
    },
    /// Column profile of a finished run's result.
    Profile {
        run: RunId,
        result: anyhow::Result<stats::Profile>,
    },
    /// The server's measurements; `None` when it recorded none.
    ServerMetrics {
        run: RunId,
        result: quill_core::error::Result<Option<ServerMetrics>>,
    },
    /// A sorted or filtered view of `base`, drained to disk.
    Derived {
        base: RunId,
        view: RunId,
        result: anyhow::Result<StoredQuery>,
    },
    Saved {
        path: std::path::PathBuf,
        result: anyhow::Result<()>,
    },
}

impl RunMsg {
    /// The run this update belongs to; `None` for one tied to no run.
    pub const fn run(&self) -> Option<RunId> {
        match self {
            Self::Started { run, .. }
            | Self::Finished { run, .. }
            | Self::Page { run, .. }
            | Self::Profile { run, .. }
            | Self::ServerMetrics { run, .. } => Some(*run),
            Self::Derived { base, .. } => Some(*base),
            Self::Saved { .. } => None,
        }
    }
}
