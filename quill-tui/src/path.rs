//! Save path expansion and completion shared by parsing and the command palette.
use std::path::PathBuf;

pub struct SavePath;

impl SavePath {
    pub fn expand(path: &str) -> Result<PathBuf, String> {
        shellexpand::full(path)
            .map(|path| PathBuf::from(path.as_ref()))
            .map_err(|error| error.to_string())
    }

    /// Preserve the command prefix and treat the remaining text as one path.
    pub fn argument(input: &str) -> Option<(&str, &str)> {
        let trimmed = input.trim_start();
        let end = trimmed.find(char::is_whitespace)?;
        if !matches!(&trimmed[..end], "save" | "w") {
            return None;
        }
        let path = trimmed[end..].trim_start();
        Some((&input[..input.len() - path.len()], path))
    }

    pub fn completions(input: &str) -> Result<Vec<String>, String> {
        let Some((command, path)) = Self::argument(input) else {
            return Ok(Vec::new());
        };
        let expanded = Self::expand(path)?;
        // Resolve a bare home or environment-variable directory before listing it.
        let path = if !path.ends_with('/')
            && expanded.is_dir()
            && (path == "~" || path.starts_with('$'))
        {
            format!("{path}/")
        } else {
            path.to_owned()
        };
        let (parent, prefix) = path
            .rsplit_once('/')
            .map_or(("", path.as_str()), |(parent, prefix)| {
                (&path[..parent.len() + 1], prefix)
            });
        let directory = if parent.is_empty() {
            PathBuf::from(".")
        } else {
            Self::expand(parent)?
        };
        let mut matches = Vec::new();
        for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if !name.starts_with(prefix) || (name.starts_with('.') && !prefix.starts_with('.')) {
                continue;
            }
            let suffix = if entry.path().is_dir() { "/" } else { "" };
            matches.push(format!("{command}{parent}{name}{suffix}"));
        }
        matches.sort();
        Ok(matches)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home_and_environment_without_running_shell_code() {
        let home = SavePath::expand("~").unwrap();
        assert!(home.is_absolute());
        assert_eq!(
            SavePath::expand("~/my results.csv").unwrap(),
            home.join("my results.csv")
        );
        if let Ok(value) = std::env::var("HOME") {
            assert_eq!(
                SavePath::expand("${HOME}/out.csv").unwrap(),
                PathBuf::from(value).join("out.csv")
            );
        }
        assert!(SavePath::expand("$QUILL_TEST_UNDEFINED_SAVE_PATH_98B1/out.csv").is_err());
        assert_eq!(
            SavePath::expand("$(echo surprise).csv").unwrap(),
            PathBuf::from("$(echo surprise).csv")
        );
    }

    #[test]
    fn completion_preserves_spaces_and_ignores_unmatched_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("my result.csv"), "").unwrap();
        std::fs::write(dir.path().join(".hidden"), "").unwrap();
        let prefix = format!("  save   {}/my ", dir.path().display());
        assert_eq!(
            SavePath::completions(&prefix).unwrap(),
            vec![format!("{prefix}result.csv")]
        );
        assert!(
            SavePath::completions(&format!("save {}/missing", dir.path().display()))
                .unwrap()
                .is_empty()
        );
        assert!(SavePath::completions("conn pg").unwrap().is_empty());
        let matches = SavePath::completions(&format!("save {}/", dir.path().display())).unwrap();
        assert_eq!(matches.len(), 1);
    }
}
