use std::path::Path;

use kudelma_storage::canonicalize_lenient;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub value: String,
    /// Exact scopes never match a `*` wildcard allow rule. Used for shell
    /// redirections and other inputs whose blast radius is not obvious.
    pub exact: bool,
}

impl Scope {
    pub fn path(path: impl AsRef<Path>) -> Self {
        Self {
            value: canonicalize_lenient(path.as_ref()).display().to_string(),
            exact: false,
        }
    }

    pub fn command(command: impl Into<String>) -> Self {
        Self {
            value: command.into(),
            exact: false,
        }
    }

    pub fn exact(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            exact: true,
        }
    }

    pub fn is_exact(&self) -> bool {
        self.exact
    }
}

pub fn is_universal(pattern: &str) -> bool {
    pattern == "*" || pattern == "**" || pattern == "/**" || pattern.trim().is_empty()
}

/// Match a permission rule scope against a concrete scope.
/// Path patterns ending in `/**` compare path components; command patterns
/// ending in ` *` match the bare command and the command with arguments.
pub fn scope_matches(pattern: &str, value: &str) -> bool {
    if is_universal(pattern) {
        return true;
    }
    if let Some(prefix) = pattern.strip_suffix("/**") {
        let prefix = canonicalize_lenient(Path::new(prefix));
        let value = canonicalize_lenient(Path::new(value));
        return value.starts_with(&prefix);
    }
    if let Some(prefix) = pattern.strip_suffix(" *") {
        return value == prefix || value.starts_with(&format!("{prefix} "));
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return value.starts_with(prefix);
    }
    pattern == value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_star_matches_everything() {
        assert!(scope_matches("*", "rm -rf /"));
    }

    #[test]
    fn command_prefix_matches_bare_and_with_args() {
        assert!(scope_matches("git *", "git status"));
        assert!(scope_matches("git *", "git"));
        assert!(!scope_matches("git *", "gitu"));
        assert!(!scope_matches("git *", "rm -rf /"));
    }

    #[test]
    fn subtree_patterns_compare_components() {
        assert!(scope_matches("/work/**", "/work/src/main.rs"));
        assert!(!scope_matches("/work/**", "/workspace/main.rs"));
    }
}
