use std::path::Path;

/// Detect the install command to run in a freshly created worktree.
///
/// Returns the highest-priority match: an explicit `make install` target wins
/// over a JS package manager, which wins over Cargo.
pub fn detect(worktree: &Path) -> Option<String> {
    if makefile_has_install_target(worktree) {
        return Some("make install".to_string());
    }
    if worktree.join("package.json").is_file() {
        return Some(format!("{} install", package_manager(worktree)));
    }
    if worktree.join("Cargo.toml").is_file() {
        return Some("cargo build".to_string());
    }
    None
}

fn package_manager(worktree: &Path) -> &'static str {
    if worktree.join("bun.lockb").is_file() || worktree.join("bun.lock").is_file() {
        "bun"
    } else if worktree.join("pnpm-lock.yaml").is_file() {
        "pnpm"
    } else if worktree.join("yarn.lock").is_file() {
        "yarn"
    } else {
        "npm"
    }
}

fn makefile_has_install_target(worktree: &Path) -> bool {
    let path = worktree.join("Makefile");
    let Ok(contents) = std::fs::read_to_string(&path) else {
        return false;
    };
    contents.lines().any(|line| {
        let trimmed = line.trim_start();
        matches!(trimmed.strip_prefix("install"), Some(rest) if rest.trim_start().starts_with(':'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn none_when_empty() {
        let dir = tempdir().unwrap();
        assert_eq!(detect(dir.path()), None);
    }

    #[test]
    fn cargo_builds() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        assert_eq!(detect(dir.path()).as_deref(), Some("cargo build"));
    }

    #[test]
    fn package_manager_from_lockfile() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("package.json"), "{}").unwrap();
        assert_eq!(detect(dir.path()).as_deref(), Some("npm install"));

        fs::write(dir.path().join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(detect(dir.path()).as_deref(), Some("pnpm install"));

        fs::write(dir.path().join("yarn.lock"), "").unwrap();
        // pnpm lockfile still takes precedence over yarn
        assert_eq!(detect(dir.path()).as_deref(), Some("pnpm install"));

        fs::write(dir.path().join("bun.lockb"), "").unwrap();
        assert_eq!(detect(dir.path()).as_deref(), Some("bun install"));
    }

    #[test]
    fn makefile_install_target_wins() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        fs::write(
            dir.path().join("Makefile"),
            "install:\n\tcargo install --path .\n",
        )
        .unwrap();
        assert_eq!(detect(dir.path()).as_deref(), Some("make install"));
    }

    #[test]
    fn makefile_without_install_target_ignored() {
        let dir = tempdir().unwrap();
        fs::write(dir.path().join("Makefile"), "build:\n\tcargo build\n").unwrap();
        assert_eq!(detect(dir.path()), None);
    }
}
