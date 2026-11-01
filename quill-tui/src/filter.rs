//! Narrowing lists by name.

/// Which list entries to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    /// Case-insensitive substring, typed after `/`. Stored lowercased.
    Contains(String),
    /// Exactly this name; used when a bare name matches several connections.
    Exact(String),
}

impl Filter {
    /// `None` for empty input, which shows everything.
    pub fn contains(text: &str) -> Option<Self> {
        (!text.is_empty()).then(|| Self::Contains(text.to_lowercase()))
    }

    pub fn exact(name: &str) -> Self {
        Self::Exact(name.to_owned())
    }

    pub fn matches(&self, name: &str) -> bool {
        match self {
            Self::Contains(needle) => name.to_lowercase().contains(needle.as_str()),
            Self::Exact(exact) => name == exact,
        }
    }

    /// Shown in list titles.
    pub fn describe(&self) -> String {
        match self {
            Self::Contains(text) => format!("/{text}"),
            Self::Exact(name) => format!("= {name}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_ignores_case_and_exact_does_not() {
        let filter = Filter::contains("Rev").unwrap();
        assert!(filter.matches("daily_REVENUE"));
        assert!(!filter.matches("daily"));
        assert!(Filter::contains("").is_none());
        assert!(Filter::exact("pg").matches("pg"));
        assert!(!Filter::exact("pg").matches("pg_prod"));
    }
}
