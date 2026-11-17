//! Watch a connection's query directory so edits made in another editor
//! reach open tabs.
use std::path::Path;

use anyhow::Context as _;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher as _};
use tokio::sync::mpsc::UnboundedSender;

use crate::{connection::ConnectionId, event::Msg};

/// Something in `connection`'s query directory changed on disk.
pub struct QueriesChanged {
    pub connection: ConnectionId,
}

/// Watches until dropped.
pub struct QueryWatch {
    _watcher: RecommendedWatcher,
}

impl QueryWatch {
    /// Watch `dir`, which must exist, reporting changes for `connection`.
    pub fn start(
        dir: &Path,
        connection: ConnectionId,
        messages: UnboundedSender<Msg>,
    ) -> anyhow::Result<Self> {
        let mut watcher =
            notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                let Ok(event) = event else { return };
                if matches!(event.kind, EventKind::Access(_)) {
                    return;
                }
                let connection = connection.clone();
                let _ = messages.send(Msg::Queries(QueriesChanged { connection }));
            })
            .context("start query watcher")?;
        watcher
            .watch(dir, RecursiveMode::NonRecursive)
            .with_context(|| format!("watch {}", dir.display()))?;
        Ok(Self { _watcher: watcher })
    }
}
