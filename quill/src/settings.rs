//! App settings from `QUILL_HOME/config.toml`, overridable by `QUILL__` variables.
use std::{num::NonZeroU64, path::PathBuf, time::Duration};

use figment::{
    Figment,
    providers::{Env, Format, Toml},
};
use serde::Deserialize;

use crate::home::QuillHome;
use quill_core::error::{Error, Result};

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub results: ResultSettings,
    #[serde(default)]
    pub ui: UiSettings,
}

/// Where drained query results are written. Each session gets its own
/// temporary directory, removed when the session ends.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ResultSettings {
    /// Parent of per-session directories; the system temp directory when unset.
    pub dir: Option<PathBuf>,
    /// Bytes written per query before streaming stops; unlimited when unset.
    pub max_bytes: Option<NonZeroU64>,
}

impl ResultSettings {
    pub fn dir(&self) -> PathBuf {
        self.dir.clone().unwrap_or_else(std::env::temp_dir)
    }
}

/// Terminal UI behaviour.
#[derive(Debug, Clone, Deserialize)]
pub struct UiSettings {
    /// Milliseconds between redraws while the screen has changed.
    #[serde(default = "defaults::redraw_ms")]
    pub redraw_ms: NonZeroU64,
    /// Milliseconds after the last edit before a query is saved.
    #[serde(default = "defaults::autosave_ms")]
    pub autosave_ms: NonZeroU64,
    /// Widest an auto-sized result column grows, in terminal cells.
    #[serde(default = "defaults::max_column_width")]
    pub max_column_width: u16,
    /// Result rows fetched per page.
    #[serde(default = "defaults::page_rows")]
    pub page_rows: NonZeroU64,
}

impl UiSettings {
    pub const fn redraw_interval(&self) -> Duration {
        Duration::from_millis(self.redraw_ms.get())
    }

    pub const fn autosave_delay(&self) -> Duration {
        Duration::from_millis(self.autosave_ms.get())
    }
}

impl Default for UiSettings {
    fn default() -> Self {
        Self {
            redraw_ms: defaults::redraw_ms(),
            autosave_ms: defaults::autosave_ms(),
            max_column_width: defaults::max_column_width(),
            page_rows: defaults::page_rows(),
        }
    }
}

mod defaults {
    use std::num::NonZeroU64;

    // Evaluated at compile time, so the zero arm cannot be reached at runtime.
    const fn non_zero(n: u64) -> NonZeroU64 {
        match NonZeroU64::new(n) {
            Some(n) => n,
            None => panic!("default must be non-zero"),
        }
    }

    pub const fn redraw_ms() -> NonZeroU64 {
        const { non_zero(100) }
    }

    pub const fn autosave_ms() -> NonZeroU64 {
        const { non_zero(750) }
    }

    pub const fn max_column_width() -> u16 {
        40
    }

    pub const fn page_rows() -> NonZeroU64 {
        const { non_zero(500) }
    }
}

impl Settings {
    /// Read `config.toml` under `home` if it exists, then apply `QUILL__`
    /// variables, e.g. `QUILL__RESULTS__MAX_BYTES`.
    pub fn load(home: &QuillHome) -> Result<Self> {
        Figment::new()
            // A missing file reads as empty; the path is absolute, so no parent search.
            .merge(Toml::file(home.config()))
            .merge(Env::prefixed("QUILL__").split("__"))
            .extract()
            .map_err(|e| Error::Config(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let settings = Settings::load(&QuillHome::at(dir.path())).unwrap();
        assert_eq!(settings.ui.max_column_width, 40);
        assert!(settings.results.max_bytes.is_none());
    }

    #[test]
    fn reads_config_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[results]\nmax_bytes = 1024\n[ui]\npage_rows = 50\n",
        )
        .unwrap();
        let settings = Settings::load(&QuillHome::at(dir.path())).unwrap();
        assert_eq!(settings.results.max_bytes.map(NonZeroU64::get), Some(1024));
        assert_eq!(settings.ui.page_rows.get(), 50);
    }
}
