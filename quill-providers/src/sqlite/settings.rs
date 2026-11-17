use crate::settings::Params;
use serde::Deserialize;
use std::{path::PathBuf, time::Duration};

#[derive(Debug, Clone, Deserialize)]
pub struct SqliteSettings {
    pub path: PathBuf,
    /// Additional database files, each attached under its file stem.
    #[serde(default)]
    pub databases: Vec<String>,
    /// How long a statement waits on a locked database before failing.
    #[serde(default = "default_busy_timeout_ms")]
    pub busy_timeout_ms: u64,
    #[serde(default)]
    pub params: Params,
}

impl SqliteSettings {
    pub const fn busy_timeout(&self) -> Duration {
        Duration::from_millis(self.busy_timeout_ms)
    }
}

/// Long enough to wait out another connection's brief write lock.
const fn default_busy_timeout_ms() -> u64 {
    5000
}
