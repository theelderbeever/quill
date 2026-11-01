use std::path::PathBuf;

use crate::{connection::ConnectionId, view::Route};

/// A state change requested by a component or command.
pub enum Action {
    /// Go one level down.
    Open(Route),
    /// Replace everything above the connection list with `route`.
    Jump(Route),
    Back,
    /// Close a connection's pool and leave its views.
    Disconnect(ConnectionId),
    /// Show an error on the palette line.
    Report(String),
    /// Export the shown result to a file.
    Save(PathBuf),
    /// Put text on the system clipboard.
    Copy(String),
    Quit,
}
