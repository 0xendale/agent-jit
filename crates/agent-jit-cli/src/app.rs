//! Private application paths.
//!
//! Nothing agent-jit records may live inside an observed repository: the pilot repository is
//! read-only, and a trace committed by accident is a leak that cannot be recalled. Paths resolve
//! under macOS application support and cache directories, or under `AGENT_JIT_HOME` when an
//! operator or test overrides them, and every directory is created `0700`.

use std::fs::{DirBuilder, Permissions};
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};

use serde_json::{Value, json};

use crate::error::{CommandError, ExitClass};

/// Environment variable that overrides path resolution, for tests and operators.
pub const HOME_OVERRIDE: &str = "AGENT_JIT_HOME";

/// Directory mode for every private directory the product creates.
const PRIVATE_DIR_MODE: u32 = 0o700;

/// Resolved private locations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    /// Root the other paths hang from.
    pub home: PathBuf,
    /// Durable state: the database and its provenance.
    pub data: PathBuf,
    /// Rebuildable working data.
    pub cache: PathBuf,
    /// The `SQLite` database file.
    pub state_db: PathBuf,
    /// Append-only hook spools.
    pub spool: PathBuf,
    /// Diagnostic logs.
    pub logs: PathBuf,
}

impl AppPaths {
    /// Resolves paths without creating anything.
    ///
    /// # Errors
    ///
    /// Returns a [`CommandError`] when the home cannot be resolved, is a symlink, or sits inside a
    /// Git repository.
    pub fn resolve() -> Result<Self, CommandError> {
        let home = match std::env::var_os(HOME_OVERRIDE) {
            Some(value) if !value.is_empty() => PathBuf::from(value),
            _ => default_home()?,
        };
        Self::under(&home)
    }

    /// Resolves paths under an explicit home, refusing unsafe locations.
    ///
    /// # Errors
    ///
    /// Returns a [`CommandError`] when `home` is relative, a symlink, or inside a Git repository.
    pub fn under(home: &Path) -> Result<Self, CommandError> {
        if !home.is_absolute() {
            return Err(CommandError::new(
                "home_not_absolute",
                format!("`{}` must be an absolute path", home.display()),
                ExitClass::SafetyRefusal,
            ));
        }
        if home
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(CommandError::new(
                "home_path_traversal",
                format!("`{}` contains a parent-directory component", home.display()),
                ExitClass::SafetyRefusal,
            ));
        }
        if let Ok(metadata) = std::fs::symlink_metadata(home)
            && metadata.file_type().is_symlink()
        {
            return Err(CommandError::new(
                "home_symlink",
                format!("`{}` is a symlink; pass the real directory", home.display()),
                ExitClass::SafetyRefusal,
            ));
        }
        if let Some(repository) = enclosing_repository(home) {
            return Err(CommandError::new(
                "home_inside_repository",
                format!(
                    "`{}` is inside the Git repository `{}`; agent-jit state never lives in an observed repository",
                    home.display(),
                    repository.display()
                ),
                ExitClass::SafetyRefusal,
            ));
        }

        let data = home.join("data");
        let cache = home.join("cache");
        Ok(Self {
            state_db: data.join("state.sqlite3"),
            spool: cache.join("spool"),
            logs: cache.join("logs"),
            data,
            cache,
            home: home.to_path_buf(),
        })
    }

    /// Creates every private directory, `0700`, if it does not already exist.
    ///
    /// # Errors
    ///
    /// Returns a [`CommandError`] when a directory cannot be created or secured.
    pub fn ensure(&self) -> Result<(), CommandError> {
        for directory in [&self.home, &self.data, &self.cache, &self.spool, &self.logs] {
            create_private_dir(directory)?;
        }
        Ok(())
    }

    /// Renders the paths as a machine-readable report.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "home": self.home.to_string_lossy(),
            "data": self.data.to_string_lossy(),
            "cache": self.cache.to_string_lossy(),
            "state_db": self.state_db.to_string_lossy(),
            "spool": self.spool.to_string_lossy(),
            "logs": self.logs.to_string_lossy(),
        })
    }
}

/// Resolves the default home under the user's macOS application support directory.
fn default_home() -> Result<PathBuf, CommandError> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| {
            CommandError::new(
                "home_unresolvable",
                format!("neither {HOME_OVERRIDE} nor HOME names an absolute directory"),
                ExitClass::SafetyRefusal,
            )
        })?;
    Ok(home.join("Library/Application Support/agent-jit"))
}

/// Returns the repository directory containing `path`, when there is one.
///
/// This walks the path itself rather than asking Git, so it works for a directory that does not
/// exist yet — which is exactly the case that must be refused before anything is created.
fn enclosing_repository(path: &Path) -> Option<PathBuf> {
    let mut cursor = Some(path);
    while let Some(current) = cursor {
        if current.join(".git").exists() {
            return Some(current.to_path_buf());
        }
        cursor = current.parent();
    }
    None
}

/// Creates one directory with `0700`, and tightens it if it already existed more permissively.
fn create_private_dir(path: &Path) -> Result<(), CommandError> {
    if !path.exists() {
        DirBuilder::new()
            .recursive(true)
            .mode(PRIVATE_DIR_MODE)
            .create(path)
            .map_err(|error| {
                CommandError::new(
                    "path_uncreatable",
                    format!("`{}`: {error}", path.display()),
                    ExitClass::Internal,
                )
            })?;
    }

    let metadata = std::fs::metadata(path).map_err(|error| {
        CommandError::new(
            "path_unreadable",
            format!("`{}`: {error}", path.display()),
            ExitClass::Internal,
        )
    })?;
    if !metadata.is_dir() {
        return Err(CommandError::new(
            "path_not_a_directory",
            format!("`{}` exists and is not a directory", path.display()),
            ExitClass::SafetyRefusal,
        ));
    }
    if metadata.permissions().mode() & 0o777 != PRIVATE_DIR_MODE {
        std::fs::set_permissions(path, Permissions::from_mode(PRIVATE_DIR_MODE)).map_err(
            |error| {
                CommandError::new(
                    "path_unsecurable",
                    format!("`{}`: {error}", path.display()),
                    ExitClass::Internal,
                )
            },
        )?;
    }
    Ok(())
}
