//! The security boundary: every path a tool receives is resolved here, and nothing
//! outside the project root (or on the deny list) is ever opened.
//!
//! Rules:
//! * paths must be **relative**; absolute paths and `..` components are rejected,
//! * the path is canonicalised (symlinks resolved) and must still be inside the root,
//!   so a symlink pointing outside the project cannot be used to escape,
//! * anything inside a `.git` directory and files named `.env*` are never exposed
//!   (they commonly hold credentials).

use std::fmt;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, PartialEq, Eq)]
pub enum SandboxError {
    /// The path is absolute, contains `..`, or otherwise cannot be a project-relative path.
    NotRelative,
    /// The path escapes the root (for example through a symlink).
    OutsideRoot,
    /// The path is on the deny list (`.git`, `.env*`).
    Denied,
    /// The path does not exist (or cannot be read).
    NotFound,
}

impl fmt::Display for SandboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            SandboxError::NotRelative => {
                "path must be relative to the project root and must not contain '..'"
            }
            SandboxError::OutsideRoot => "path resolves outside the project root",
            SandboxError::Denied => {
                "access to this path is not allowed (.git and .env* files are hidden)"
            }
            SandboxError::NotFound => "path not found",
        })
    }
}

impl std::error::Error for SandboxError {}

#[derive(Debug, Clone)]
pub struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    /// Create a sandbox rooted at `root`, which must be an existing directory.
    pub fn new(root: impl AsRef<Path>) -> std::io::Result<Self> {
        let root = root.as_ref().canonicalize()?;
        if !root.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                format!("{} is not a directory", root.display()),
            ));
        }
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Resolve a project-relative path to a canonical absolute path inside the root.
    /// An empty string or "." means the root itself.
    pub fn resolve(&self, relative: &str) -> Result<PathBuf, SandboxError> {
        let relative = Path::new(relative);
        let mut checked = PathBuf::new();
        for component in relative.components() {
            match component {
                Component::Normal(part) => {
                    if is_denied_name(part.to_string_lossy().as_ref()) {
                        return Err(SandboxError::Denied);
                    }
                    checked.push(part);
                }
                Component::CurDir => {}
                // Prefix / RootDir (absolute paths) and ParentDir (`..`).
                _ => return Err(SandboxError::NotRelative),
            }
        }
        let full = self.root.join(&checked);
        let canonical = full.canonicalize().map_err(|_| SandboxError::NotFound)?;
        if !canonical.starts_with(&self.root) {
            return Err(SandboxError::OutsideRoot);
        }
        // A symlink inside the root may point at a denied location inside the root.
        let inside = canonical.strip_prefix(&self.root).unwrap_or(&canonical);
        if inside
            .components()
            .any(|c| is_denied_name(&c.as_os_str().to_string_lossy()))
        {
            return Err(SandboxError::Denied);
        }
        Ok(canonical)
    }

    /// Path relative to the root, for display (never shows the absolute location).
    pub fn display_relative(&self, path: &Path) -> String {
        let relative = path.strip_prefix(&self.root).unwrap_or(path);
        let text = relative.to_string_lossy();
        if text.is_empty() {
            ".".to_string()
        } else {
            text.into_owned()
        }
    }
}

/// Names that are never exposed, in listings or by direct path.
pub fn is_denied_name(name: &str) -> bool {
    name == ".git" || name == ".env" || name.starts_with(".env.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn project() -> (tempfile::TempDir, Sandbox) {
        let dir = tempdir().unwrap();
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/lib.rs"), "fn main() {}").unwrap();
        fs::write(dir.path().join("README.md"), "# hi").unwrap();
        let sandbox = Sandbox::new(dir.path()).unwrap();
        (dir, sandbox)
    }

    #[test]
    fn resolves_files_and_the_root() {
        let (_dir, sb) = project();
        assert!(sb.resolve("src/lib.rs").unwrap().ends_with("src/lib.rs"));
        assert_eq!(sb.resolve("").unwrap(), sb.root());
        assert_eq!(sb.resolve(".").unwrap(), sb.root());
        assert_eq!(
            sb.resolve("./src/../src").unwrap_err(),
            SandboxError::NotRelative
        );
    }

    #[test]
    fn rejects_absolute_paths_and_parent_components() {
        let (_dir, sb) = project();
        assert_eq!(
            sb.resolve("/etc/passwd").unwrap_err(),
            SandboxError::NotRelative
        );
        assert_eq!(
            sb.resolve("../secret").unwrap_err(),
            SandboxError::NotRelative
        );
        assert_eq!(
            sb.resolve("src/../../x").unwrap_err(),
            SandboxError::NotRelative
        );
    }

    #[test]
    fn missing_paths_are_not_found() {
        let (_dir, sb) = project();
        assert_eq!(sb.resolve("nope.txt").unwrap_err(), SandboxError::NotFound);
    }

    #[test]
    fn git_and_env_files_are_denied_even_if_they_exist() {
        let (dir, sb) = project();
        fs::create_dir_all(dir.path().join(".git")).unwrap();
        fs::write(dir.path().join(".git/config"), "secret").unwrap();
        fs::write(dir.path().join(".env"), "TOKEN=1").unwrap();
        fs::write(dir.path().join(".env.production"), "TOKEN=2").unwrap();
        fs::write(dir.path().join(".envrc-not-env"), "ok").unwrap();

        assert_eq!(sb.resolve(".git").unwrap_err(), SandboxError::Denied);
        assert_eq!(sb.resolve(".git/config").unwrap_err(), SandboxError::Denied);
        assert_eq!(sb.resolve(".env").unwrap_err(), SandboxError::Denied);
        assert_eq!(
            sb.resolve(".env.production").unwrap_err(),
            SandboxError::Denied
        );
        assert!(
            sb.resolve(".envrc-not-env").is_ok(),
            "only .env and .env.* are denied"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_pointing_outside_the_root_cannot_be_followed() {
        let (dir, sb) = project();
        let outside = tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "top secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.txt"),
            dir.path().join("link.txt"),
        )
        .unwrap();

        assert_eq!(sb.resolve("escape").unwrap_err(), SandboxError::OutsideRoot);
        assert_eq!(
            sb.resolve("escape/secret.txt").unwrap_err(),
            SandboxError::OutsideRoot
        );
        assert_eq!(
            sb.resolve("link.txt").unwrap_err(),
            SandboxError::OutsideRoot
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_denied_file_inside_the_root_is_still_denied() {
        let (dir, sb) = project();
        fs::write(dir.path().join(".env"), "TOKEN=1").unwrap();
        std::os::unix::fs::symlink(dir.path().join(".env"), dir.path().join("innocent.txt"))
            .unwrap();
        assert_eq!(
            sb.resolve("innocent.txt").unwrap_err(),
            SandboxError::Denied
        );
    }

    #[test]
    fn display_paths_are_relative() {
        let (_dir, sb) = project();
        let p = sb.resolve("src/lib.rs").unwrap();
        assert_eq!(sb.display_relative(&p), "src/lib.rs");
        assert_eq!(sb.display_relative(sb.root()), ".");
    }

    #[test]
    fn root_must_be_an_existing_directory() {
        let (dir, _sb) = project();
        assert!(Sandbox::new(dir.path().join("README.md")).is_err());
        assert!(Sandbox::new(dir.path().join("missing")).is_err());
    }
}
