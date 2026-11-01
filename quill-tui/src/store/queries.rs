//! Saved queries: one `<name>.sql` file per query in a connection's directory.
use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, bail};

const EXTENSION: &str = "sql";

/// A query name that is safe as a file stem: not empty, no path separators,
/// no leading `.`, and no control characters.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct QueryName(String);

impl QueryName {
    pub fn parse(name: &str) -> anyhow::Result<Self> {
        let name = name.trim();
        if name.is_empty() {
            bail!("query name is empty");
        }
        if name.starts_with('.') {
            bail!("query name cannot start with `.`");
        }
        if name
            .chars()
            .any(|c| matches!(c, '/' | '\\') || c.is_control())
        {
            bail!("query name cannot contain `/`, `\\` or control characters");
        }
        Ok(Self(name.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for QueryName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One connection's query directory. Created on first write.
#[derive(Debug, Clone)]
pub struct QueryStore {
    dir: PathBuf,
}

impl QueryStore {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, name: &QueryName) -> PathBuf {
        self.dir.join(format!("{name}.{EXTENSION}"))
    }

    /// Saved queries sorted by name; empty when the directory does not exist.
    pub fn list(&self) -> anyhow::Result<Vec<QueryName>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e).with_context(|| format!("list {}", self.dir.display())),
        };
        let mut names = Vec::new();
        for entry in entries {
            let path = entry
                .with_context(|| format!("list {}", self.dir.display()))?
                .path();
            if path.extension().is_some_and(|e| e == EXTENSION) && path.is_file() {
                let stem = path.file_stem().and_then(|s| s.to_str());
                if let Some(name) = stem.and_then(|s| QueryName::parse(s).ok()) {
                    names.push(name);
                }
            }
        }
        names.sort();
        Ok(names)
    }

    /// The saved text; empty for a query never written.
    pub fn read(&self, name: &QueryName) -> anyhow::Result<String> {
        let path = self.path(name);
        match fs::read_to_string(&path) {
            Ok(text) => Ok(text),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(String::new()),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    /// Create an empty query; fails if one with this name exists.
    pub fn create(&self, name: &QueryName) -> anyhow::Result<()> {
        let path = self.path(name);
        self.ensure_dir()?;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("create {}", path.display()))?;
        Ok(())
    }

    /// Rename without replacing an existing query.
    pub fn rename(&self, from: &QueryName, to: &QueryName) -> anyhow::Result<()> {
        let (from, to) = (self.path(from), self.path(to));
        if to.exists() {
            bail!("{} already exists", to.display());
        }
        fs::rename(&from, &to)
            .with_context(|| format!("rename {} to {}", from.display(), to.display()))
    }

    pub fn delete(&self, name: &QueryName) -> anyhow::Result<()> {
        let path = self.path(name);
        fs::remove_file(&path).with_context(|| format!("delete {}", path.display()))
    }

    /// Replace the saved text through a temporary file, so a crash mid-write
    /// leaves the previous version intact.
    pub fn write(&self, name: &QueryName, text: &str) -> anyhow::Result<()> {
        let path = self.path(name);
        self.ensure_dir()?;
        let temp = self.dir.join(format!(".{name}.{EXTENSION}.tmp"));
        fs::write(&temp, text).with_context(|| format!("write {}", temp.display()))?;
        fs::rename(&temp, &path).with_context(|| format!("write {}", path.display()))
    }

    fn ensure_dir(&self) -> anyhow::Result<()> {
        fs::create_dir_all(&self.dir).with_context(|| format!("create {}", self.dir.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(s: &str) -> QueryName {
        QueryName::parse(s).unwrap()
    }

    #[test]
    fn rejects_unsafe_names() {
        for bad in ["", "  ", ".hidden", "a/b", "a\\b", "tab\there"] {
            assert!(QueryName::parse(bad).is_err(), "{bad:?}");
        }
        assert_eq!(name(" daily revenue ").as_str(), "daily revenue");
    }

    #[test]
    fn create_write_rename_delete_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = QueryStore::new(dir.path().join("queries/pg"));
        assert!(store.list().unwrap().is_empty());

        store.create(&name("b")).unwrap();
        store.create(&name("a")).unwrap();
        assert!(store.create(&name("a")).is_err());
        store.write(&name("a"), "SELECT 1").unwrap();
        assert_eq!(store.read(&name("a")).unwrap(), "SELECT 1");
        assert_eq!(store.list().unwrap(), [name("a"), name("b")]);

        assert!(store.rename(&name("a"), &name("b")).is_err());
        store.rename(&name("a"), &name("c")).unwrap();
        store.delete(&name("b")).unwrap();
        assert_eq!(store.list().unwrap(), [name("c")]);
        assert_eq!(store.read(&name("c")).unwrap(), "SELECT 1");
        assert_eq!(store.read(&name("missing")).unwrap(), "");
    }
}
