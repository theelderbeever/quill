//! Configured connections, grouped by source file, and their live pool state.
use std::{
    borrow::Cow,
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
};

use indexmap::IndexMap;
use quill_core::{error::Result, metadata::TableRef};
use quill_drivers::{ConnectionPool, PoolFuture};

use crate::filter::Filter;

/// How the TUI reaches one configured connection. The application owns the
/// mapping from configuration to drivers and query directories.
///
/// ```
/// use std::{path::Path, sync::Arc};
/// use quill_core::error::Error;
/// use quill_drivers::{ConnectionPool, PoolFuture};
/// use quill_tui::ConnectionSource;
///
/// struct Offline;
///
/// impl ConnectionSource for Offline {
///     fn user(&self) -> Option<&str> {
///         None
///     }
///     fn kind(&self) -> &'static str {
///         "offline"
///     }
///     fn connect(&self) -> PoolFuture<'_, Arc<dyn ConnectionPool>> {
///         Box::pin(async { Err(Error::UnsupportedOperation("connect")) })
///     }
///     fn query_dir(&self) -> &Path {
///         Path::new("queries/offline")
///     }
/// }
/// ```
pub trait ConnectionSource: Send + Sync {
    /// Username shown in details, e.g. `user`.
    fn user(&self) -> Option<&str>;

    /// Driver name shown details, e.g. `postgres`.
    fn kind(&self) -> &'static str;

    /// Where the data lives, shown in details, e.g. `db.internal:5432`.
    fn host(&self) -> Option<Cow<'_, str>> {
        None
    }

    /// The database or its equivalent, shown in details, e.g. `analytics`.
    fn database(&self) -> Option<&str> {
        None
    }

    fn connect(&self) -> PoolFuture<'_, Arc<dyn ConnectionPool>>;

    /// Directory of this connection's saved `.sql` files; need not exist yet.
    fn query_dir(&self) -> &Path;

    /// How to name `table` in this connection's SQL, quoted. Defaults to
    /// `"schema"."table"`.
    fn table_reference(&self, table: &TableRef) -> String {
        format!(
            "{}.{}",
            quote(&table.schema.name, '"'),
            quote(&table.name, '"')
        )
    }
}

/// `name` wrapped in `mark`, with embedded marks doubled.
///
/// ```
/// assert_eq!(quill_tui::quote("a\"b", '"'), "\"a\"\"b\"");
/// assert_eq!(quill_tui::quote("t", '`'), "`t`");
/// ```
pub fn quote(name: &str, mark: char) -> String {
    let doubled = format!("{mark}{mark}");
    format!("{mark}{}{mark}", name.replace(mark, &doubled))
}

/// Connections defined in one file.
pub struct ConnectionGroup {
    /// Shown as the group heading; the file's path.
    pub label: String,
    pub connections: Vec<(String, Box<dyn ConnectionSource>)>,
}

/// A configured connection: its group plus its name within that group.
/// Only [`Connections`] creates one, so holding an id means it exists.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConnectionId {
    group: usize,
    name: Arc<str>,
}

impl ConnectionId {
    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

/// What a bare connection name refers to.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    One(ConnectionId),
    /// Defined in several files; the connection list shows just these.
    Many(Filter),
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

/// Connections in file order, then config order within each file.
///
/// ```
/// # use std::{path::Path, sync::Arc};
/// # use quill_core::error::Error;
/// # use quill_drivers::{ConnectionPool, PoolFuture};
/// # use quill_tui::{ConnectionGroup, ConnectionSource, Connections, Target};
/// # struct Offline;
/// # impl ConnectionSource for Offline {
/// #     fn user(&self) -> Option<&str> { None }
/// #     fn kind(&self) -> &'static str { "offline" }
/// #     fn connect(&self) -> PoolFuture<'_, Arc<dyn ConnectionPool>> {
/// #         Box::pin(async { Err(Error::UnsupportedOperation("connect")) })
/// #     }
/// #     fn query_dir(&self) -> &Path { Path::new("q") }
/// # }
/// let group = |label: &str, names: &[&str]| ConnectionGroup {
///     label: label.into(),
///     connections: names
///         .iter()
///         .map(|n| (n.to_string(), Box::new(Offline) as Box<dyn ConnectionSource>))
///         .collect(),
/// };
/// let connections = Connections::new([group("home", &["pg", "ch"]), group("project", &["pg"])]);
/// assert!(matches!(connections.lookup("ch"), Some(Target::One(_))));
/// assert!(matches!(connections.lookup("pg"), Some(Target::Many(_))));
/// assert!(connections.lookup("missing").is_none());
/// ```
pub struct Connections {
    labels: Vec<String>,
    entries: IndexMap<ConnectionId, Entry>,
}

impl Connections {
    pub fn new(groups: impl IntoIterator<Item = ConnectionGroup>) -> Self {
        let mut labels = Vec::new();
        let mut entries = IndexMap::new();
        for (group, ConnectionGroup { label, connections }) in groups.into_iter().enumerate() {
            labels.push(label);
            for (name, source) in connections {
                let id = ConnectionId {
                    group,
                    name: name.into(),
                };
                let entry = Entry {
                    source: Arc::from(source),
                    state: ConnectionState::Idle,
                };
                entries.insert(id, entry);
            }
        }
        Self { labels, entries }
    }

    /// Resolve a bare name across all files; `None` when no file defines it.
    pub fn lookup(&self, name: &str) -> Option<Target> {
        let mut matches = self.entries.keys().filter(|id| id.name() == name);
        let first = matches.next()?;
        Some(match matches.next() {
            None => Target::One(first.clone()),
            Some(_) => Target::Many(Filter::exact(name)),
        })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn label(&self, id: &ConnectionId) -> &str {
        self.labels.get(id.group).map_or("", String::as_str)
    }

    pub(crate) fn kind(&self, id: &ConnectionId) -> &'static str {
        self.entries.get(id).map_or("", |e| e.source.kind())
    }

    pub(crate) fn user<'a>(&'a self, id: &ConnectionId) -> &'a str {
        self.entries
            .get(id)
            .and_then(|e| e.source.user())
            .unwrap_or_default()
    }

    pub(crate) fn host(&self, id: &ConnectionId) -> Cow<'_, str> {
        self.entries
            .get(id)
            .and_then(|e| e.source.host())
            .unwrap_or_default()
    }

    pub(crate) fn database<'a>(&'a self, id: &ConnectionId) -> &'a str {
        self.entries
            .get(id)
            .and_then(|e| e.source.database())
            .unwrap_or_default()
    }

    pub(crate) fn query_dir(&self, id: &ConnectionId) -> PathBuf {
        self.entries
            .get(id)
            .map_or_else(PathBuf::new, |e| e.source.query_dir().to_path_buf())
    }

    pub(crate) fn state(&self, id: &ConnectionId) -> Option<&ConnectionState> {
        self.entries.get(id).map(|e| &e.state)
    }

    pub(crate) fn table_reference(&self, id: &ConnectionId, table: &TableRef) -> String {
        self.entries
            .get(id)
            .map_or_else(|| table.name.clone(), |e| e.source.table_reference(table))
    }

    pub(crate) fn pool(&self, id: &ConnectionId) -> Option<Arc<dyn ConnectionPool>> {
        match self.state(id)? {
            ConnectionState::Connected(pool) => Some(pool.clone()),
            _ => None,
        }
    }

    /// Id, kind and state of each connection, in display order.
    pub(crate) fn iter(
        &self,
    ) -> impl Iterator<Item = (&ConnectionId, &'static str, &ConnectionState)> {
        self.entries
            .iter()
            .map(|(id, e)| (id, e.source.kind(), &e.state))
    }

    /// Mark `id` as connecting and hand back its source, unless it is
    /// already connected or connecting.
    pub(crate) fn begin_connect(&mut self, id: &ConnectionId) -> Option<Arc<dyn ConnectionSource>> {
        let entry = self.entries.get_mut(id)?;
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
        id: &ConnectionId,
        result: Result<Arc<dyn ConnectionPool>>,
    ) {
        let Some(entry) = self.entries.get_mut(id) else {
            return;
        };
        // Disconnected while connecting; drop the late result.
        if !matches!(entry.state, ConnectionState::Connecting) {
            return;
        }
        entry.state = match result {
            Ok(pool) => ConnectionState::Connected(pool),
            Err(error) => ConnectionState::Failed(error_chain(&error)),
        };
    }

    /// Drop the pool, or abandon an attempt in flight. Returns whether
    /// anything was open.
    pub(crate) fn disconnect(&mut self, id: &ConnectionId) -> bool {
        let Some(entry) = self.entries.get_mut(id) else {
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

/// An error and its sources joined by `: `.
pub(crate) fn error_chain(error: &dyn std::error::Error) -> String {
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    message
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use quill_core::error::Error;

    /// A source whose connection attempts always fail.
    pub(crate) struct Offline(pub PathBuf);

    impl ConnectionSource for Offline {
        fn user(&self) -> Option<&'static str> {
            Some("user")
        }

        fn kind(&self) -> &'static str {
            "offline"
        }
        fn connect(&self) -> PoolFuture<'_, Arc<dyn ConnectionPool>> {
            Box::pin(async { Err(Error::UnsupportedOperation("connect")) })
        }
        fn query_dir(&self) -> &Path {
            &self.0
        }
    }

    pub(crate) fn group(label: &str, names: &[&str]) -> ConnectionGroup {
        ConnectionGroup {
            label: label.into(),
            connections: names
                .iter()
                .map(|n| {
                    let source: Box<dyn ConnectionSource> = Box::new(Offline(
                        std::env::temp_dir().join("quill-test-none").join(n),
                    ));
                    ((*n).to_owned(), source)
                })
                .collect(),
        }
    }

    pub(crate) fn connections(names: &[&str]) -> Connections {
        Connections::new([group("home", names)])
    }

    /// The single connection named `name`.
    pub(crate) fn id(connections: &Connections, name: &str) -> ConnectionId {
        match connections.lookup(name) {
            Some(Target::One(id)) => id,
            _ => panic!("expected one connection named {name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        testing::{connections, group, id},
        *,
    };
    use quill_core::error::Error;

    #[test]
    fn connects_once_and_retries_after_failure() {
        let mut connections = connections(&["a"]);
        let name = id(&connections, "a");
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
        let name = id(&connections, "a");
        connections.begin_connect(&name);
        assert!(connections.disconnect(&name));
        connections.finish_connect(&name, Err(Error::UnsupportedOperation("connect")));
        assert!(matches!(
            connections.state(&name),
            Some(ConnectionState::Idle)
        ));
        assert!(!connections.disconnect(&name));
    }

    #[test]
    fn same_name_in_two_files_is_ambiguous() {
        let connections = Connections::new([group("home", &["pg"]), group("project", &["pg"])]);
        assert_eq!(
            connections.lookup("pg"),
            Some(Target::Many(Filter::exact("pg")))
        );
        let ids: Vec<_> = connections
            .iter()
            .map(|(id, ..)| connections.label(id))
            .collect();
        assert_eq!(ids, ["home", "project"]);
    }
}
