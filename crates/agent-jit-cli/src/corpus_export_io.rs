//! Private staging and atomic publication for corpus exports.

use std::fs::{DirBuilder, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use agent_jit_store::{IdSource, SystemIdSource};

use crate::error::{CommandError, ExitClass};

pub(crate) fn prepare_destination(output: &Path, worktree: &Path) -> Result<PathBuf, CommandError> {
    if !output.is_absolute() {
        return Err(CommandError::refused(
            "export_path_not_absolute",
            "export path must be absolute",
        ));
    }
    if let Ok(metadata) = std::fs::symlink_metadata(output)
        && metadata.file_type().is_symlink()
    {
        return Err(CommandError::refused(
            "export_path_symlink",
            "export path must not be a symlink",
        ));
    }
    let parent = output
        .parent()
        .ok_or_else(|| internal("export path has no parent"))?;
    let parent = parent
        .canonicalize()
        .map_err(|error| io_error(parent, &error))?;
    let name = output
        .file_name()
        .ok_or_else(|| internal("export path has no final component"))?;
    let resolved = parent.join(name);
    let worktree = worktree
        .canonicalize()
        .map_err(|error| io_error(worktree, &error))?;
    if resolved == worktree || resolved.starts_with(&worktree) {
        return Err(CommandError::refused(
            "export_path_in_repository",
            "export path must be outside the observed repository",
        ));
    }
    if resolved.exists()
        && std::fs::read_dir(&resolved)
            .map_err(|error| io_error(output, &error))?
            .next()
            .is_some()
    {
        return Err(CommandError::refused(
            "export_path_not_empty",
            "export directory must be empty",
        ));
    }
    Ok(resolved)
}

pub(crate) struct StageDir {
    path: PathBuf,
    cleanup_armed: bool,
}

impl StageDir {
    pub(crate) fn create(output: &Path) -> Result<Self, CommandError> {
        Self::create_with(output, || SystemIdSource.next_body())
    }

    fn create_with(
        output: &Path,
        mut next_body: impl FnMut() -> String,
    ) -> Result<Self, CommandError> {
        let parent = output
            .parent()
            .ok_or_else(|| internal("export path has no parent"))?;
        for _ in 0..16 {
            let path = parent.join(format!(".agent-jit-export-{}", next_body()));
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        cleanup_armed: true,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(io_error(&path, &error)),
            }
        }
        Err(internal(
            "unable to allocate a unique export staging directory",
        ))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn publish(mut self, output: &Path) -> Result<(), CommandError> {
        if output.exists() {
            std::fs::remove_dir(output).map_err(|error| io_error(output, &error))?;
        }
        std::fs::rename(&self.path, output).map_err(|error| io_error(output, &error))?;
        self.cleanup_armed = false;
        Ok(())
    }
}

pub(crate) fn create_private_dir_all(path: &Path) -> Result<(), CommandError> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|error| io_error(path, &error))
}

pub(crate) fn write_private(path: &Path, content: &str) -> Result<(), CommandError> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| io_error(path, &error))?;
    file.write_all(content.as_bytes())
        .map_err(|error| io_error(path, &error))?;
    file.sync_all().map_err(|error| io_error(path, &error))
}

impl Drop for StageDir {
    fn drop(&mut self) {
        if self.cleanup_armed {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

fn internal(message: impl Into<String>) -> CommandError {
    CommandError::new("export_failed", message, ExitClass::Internal)
}

fn io_error(path: &Path, error: &std::io::Error) -> CommandError {
    CommandError::new(
        "export_failed",
        format!("`{}`: {error}", path.display()),
        ExitClass::Internal,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::StageDir;

    #[test]
    fn stale_collision_is_preserved_while_a_unique_stage_is_allocated_and_cleaned() {
        // Given
        let parent = tempfile::tempdir().unwrap();
        let output = parent.path().join("export");
        let stale = parent.path().join(".agent-jit-export-collision");
        std::fs::create_dir(&stale).unwrap();
        let mut names = ["collision", "unique"].into_iter();

        // When
        let owned_stage =
            StageDir::create_with(&output, || names.next().unwrap().to_owned()).unwrap();
        let allocated = owned_stage.path().to_path_buf();
        drop(owned_stage);

        // Then
        assert!(stale.exists());
        assert!(!allocated.exists());
    }

    #[test]
    fn failed_publication_cleans_owned_stage_without_deleting_destination() {
        // Given
        let parent = tempfile::tempdir().unwrap();
        let output = parent.path().join("export");
        std::fs::create_dir(&output).unwrap();
        std::fs::write(output.join("owned"), "keep").unwrap();
        let stage = StageDir::create_with(&output, || "unique".to_owned()).unwrap();
        let allocated = stage.path().to_path_buf();

        // When
        let result = stage.publish(&output);

        // Then
        assert!(result.is_err());
        assert!(!allocated.exists());
        assert_eq!(
            std::fs::read_to_string(output.join("owned")).unwrap(),
            "keep"
        );
    }
}
