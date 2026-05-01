use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// In-process handle for a running query. Stateless to allocate (no shared
/// counter), unique across all engine instances. Not persisted — history
/// records the SQL text and timestamps, not this id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct QueryId(Uuid);

impl QueryId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub const fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    pub const fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for QueryId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for QueryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.0, f)
    }
}
