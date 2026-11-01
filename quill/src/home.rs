//! The directory holding Quill's own config, home connections and their queries.
use std::path::{Path, PathBuf};

use quill_core::error::{Error, Result};

/// `QUILL_HOME`, else `$XDG_CONFIG_HOME/quill`, else the platform config
/// directory's `quill`. Not required to exist.
#[derive(Debug, Clone)]
pub struct QuillHome(PathBuf);

impl QuillHome {
    pub fn resolve() -> Result<Self> {
        let var = |name| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        var("QUILL_HOME")
            .or_else(|| var("XDG_CONFIG_HOME").map(|p| p.join("quill")))
            .or_else(|| dirs::config_dir().map(|p| p.join("quill")))
            .ok_or_else(|| Error::Config("cannot find a config directory; set QUILL_HOME".into()))
            .and_then(|dir| {
                // Absolute, so it compares equal to absolute connection file paths.
                std::path::absolute(&dir)
                    .map(Self)
                    .map_err(|e| Error::Config(format!("{}: {e}", dir.display())))
            })
    }

    /// Use `dir` as home, bypassing the environment.
    ///
    /// ```
    /// use quill::home::QuillHome;
    /// let home = QuillHome::at("/etc/quill");
    /// assert_eq!(home.connections(), std::path::Path::new("/etc/quill/connections.toml"));
    /// ```
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        Self(dir.into())
    }

    pub fn path(&self) -> &Path {
        &self.0
    }

    /// App settings: results, UI.
    pub fn config(&self) -> PathBuf {
        self.0.join("config.toml")
    }

    pub fn connections(&self) -> PathBuf {
        self.0.join("connections.toml")
    }

    /// Parent of per-connection query directories for home connections.
    pub fn queries(&self) -> PathBuf {
        self.0.join("queries")
    }
}
