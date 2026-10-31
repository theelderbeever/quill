//! Configured connections and their live pool state.
use std::{borrow::Borrow, error::Error as _, fmt, sync::Arc};

use indexmap::IndexMap;
use quill_core::error::Result;
use quill_drivers::{ConnectionPool, PoolFuture};

/// How the TUI reaches one configured connection. The application owns the
/// mapping from configuration to drivers; the TUI only asks for a pool.
///
/// ```
/// use std::sync::Arc;
/// use quill_core::error::Error;
/// use quill_drivers::{ConnectionPool, PoolFuture};
/// use quill_tui::ConnectionSource;
///
/// struct Offline;
///
/// impl ConnectionSource for Offline {
///     fn kind(&self) -> &'static str {
///         "offline"
///     }
///     fn connect(&self) -> PoolFuture<'_, Arc<dyn ConnectionPool>> {
///         Box::pin(async { Err(Error::UnsupportedOperation("connect")) })
///     }
/// }
/// ```
pub trait ConnectionSource: Send + Sync {
    /// Driver name shown beside the connection, e.g. `postgres`.
    fn kind(&self) -> &'static str;

    fn connect(&self) -> PoolFuture<'_, Arc<dyn ConnectionPool>>;
}

/// A connection name present in config. Only [`Connections::resolve`] creates
/// one, so holding a name means the connection exists.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConnectionName(Arc<str>);

impl ConnectionName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Borrow<str> for ConnectionName {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ConnectionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

pub enum ConnectionState {
    Idle,
    Connecting,
    Connected(Arc<dyn ConnectionPool>),
    /// The error and its sources, formatted once when the attempt failed.
    Failed(String),
}

impl ConnectionState {
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Connecting => "connecting",
            Self::Connected(_) => "connected",
            Self::Failed(_) => "failed",
        }
    }
}

struct Entry {
    source: Arc<dyn ConnectionSource>,
    state: ConnectionState,
}

/// Connections in config order.
///
/// ```
/// # use std::sync::Arc;
/// # use quill_core::error::Error;
/// # use quill_drivers::{ConnectionPool, PoolFuture};
/// # use quill_tui::{ConnectionSource, Connections};
/// # struct Offline;
/// # impl ConnectionSource for Offline {
/// #     fn kind(&self) -> &'static str { "offline" }
/// #     fn connect(&self) -> PoolFuture<'_, Arc<dyn ConnectionPool>> {
/// #         Box::pin(async { Err(Error::UnsupportedOperation("connect")) })
/// #     }
/// # }
/// let connections = Connections::new([("pg_prod".to_owned(), Offline)]);
/// assert_eq!(connections.resolve("pg_prod").map(|n| n.as_str()), Some("pg_prod"));
/// assert!(connections.resolve("missing").is_none());
/// ```
pub struct Connections {
    entries: IndexMap<ConnectionName, Entry>,
}

impl Connections {
    pub fn new<S: ConnectionSource + 'static>(
        sources: impl IntoIterator<Item = (String, S)>,
    ) -> Self {
        let entries = sources
            .into_iter()
            .map(|(name, source)| {
                let entry = Entry {
                    source: Arc::new(source),
                    state: ConnectionState::Idle,
                };
                (ConnectionName(name.into()), entry)
            })
            .collect();
        Self { entries }
    }

    pub fn resolve(&self, name: &str) -> Option<&ConnectionName> {
        self.entries.get_key_value(name).map(|(name, _)| name)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get_index(&self, index: usize) -> Option<&ConnectionName> {
        self.entries.get_index(index).map(|(name, _)| name)
    }

    pub fn kind(&self, name: &ConnectionName) -> &'static str {
        self.entries.get(name).map_or("", |e| e.source.kind())
    }

    pub fn state(&self, name: &ConnectionName) -> Option<&ConnectionState> {
        self.entries.get(name).map(|e| &e.state)
    }

    /// Name, kind and state of each connection, in config order.
    pub fn iter(&self) -> impl Iterator<Item = (&ConnectionName, &'static str, &ConnectionState)> {
        self.entries
            .iter()
            .map(|(name, e)| (name, e.source.kind(), &e.state))
    }

    /// Mark `name` as connecting and hand back its source, unless it is
    /// already connected or connecting.
    pub(crate) fn begin_connect(
        &mut self,
        name: &ConnectionName,
    ) -> Option<Arc<dyn ConnectionSource>> {
        let entry = self.entries.get_mut(name)?;
        match entry.state {
            ConnectionState::Idle | ConnectionState::Failed(_) => {
                entry.state = ConnectionState::Connecting;
                Some(entry.source.clone())
            }
            ConnectionState::Connecting | ConnectionState::Connected(_) => None,
        }
    }

    pub(crate) fn finish_connect(
        &mut self,
        name: &ConnectionName,
        result: Result<Arc<dyn ConnectionPool>>,
    ) {
        let Some(entry) = self.entries.get_mut(name) else {
            return;
        };
        // Disconnected while connecting; drop the late result.
        if !matches!(entry.state, ConnectionState::Connecting) {
            return;
        }
        entry.state = match result {
            Ok(pool) => ConnectionState::Connected(pool),
            Err(error) => {
                let mut message = error.to_string();
                let mut source = error.source();
                while let Some(cause) = source {
                    message.push_str(": ");
                    message.push_str(&cause.to_string());
                    source = cause.source();
                }
                ConnectionState::Failed(message)
            }
        };
    }
}

impl Connections {
    /// Drop the pool, or abandon an attempt in flight. Returns whether
    /// anything was open.
    pub(crate) fn disconnect(&mut self, name: &ConnectionName) -> bool {
        let Some(entry) = self.entries.get_mut(name) else {
            return false;
        };
        let open = matches!(
            entry.state,
            ConnectionState::Connecting | ConnectionState::Connected(_)
        );
        entry.state = ConnectionState::Idle;
        open
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use quill_core::error::Error;

    /// A source whose connection attempts always fail.
    pub(crate) struct Offline;

    impl ConnectionSource for Offline {
        fn kind(&self) -> &'static str {
            "offline"
        }
        fn connect(&self) -> PoolFuture<'_, Arc<dyn ConnectionPool>> {
            Box::pin(async { Err(Error::UnsupportedOperation("connect")) })
        }
    }

    pub(crate) fn connections(names: &[&str]) -> Connections {
        Connections::new(names.iter().map(|n| ((*n).to_owned(), Offline)))
    }
}

#[cfg(test)]
mod tests {
    use super::{testing::connections, *};
    use quill_core::error::Error;

    #[test]
    fn connects_once_and_retries_after_failure() {
        let mut connections = connections(&["a"]);
        let name = connections.resolve("a").cloned().unwrap();
        assert!(connections.begin_connect(&name).is_some());
        assert!(connections.begin_connect(&name).is_none());

        connections.finish_connect(&name, Err(Error::UnsupportedOperation("connect")));
        let Some(ConnectionState::Failed(message)) = connections.state(&name) else {
            panic!("expected failure");
        };
        assert_eq!(message, "operation is not supported: connect");
        assert!(connections.begin_connect(&name).is_some());
    }

    #[test]
    fn disconnect_drops_a_late_connect_result() {
        let mut connections = connections(&["a"]);
        let name = connections.resolve("a").cloned().unwrap();
        connections.begin_connect(&name);
        assert!(connections.disconnect(&name));
        connections.finish_connect(&name, Err(Error::UnsupportedOperation("connect")));
        assert!(matches!(
            connections.state(&name),
            Some(ConnectionState::Idle)
        ));
        assert!(!connections.disconnect(&name));
    }
}
