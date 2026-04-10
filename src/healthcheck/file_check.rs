//! File-based healthcheck implementation.
//!
//! Checks whether a directory contains at least one file. If so, it considers
//! the check healthy and deletes the triggering file so the check resets on the
//! next poll.

use tracing::{info, warn};

/// Perform a file-based healthcheck on the directory at `path`.
///
/// Returns `true` if at least one entry is found inside the directory. The
/// first discovered file is deleted so that subsequent polls reflect fresh
/// trigger drops. Returns `false` if the directory is empty, does not exist,
/// or cannot be read.
pub async fn check_file(path: &str) -> bool {
    let mut dir = match tokio::fs::read_dir(path).await {
        Ok(d) => d,
        Err(e) => {
            warn!(path, error = %e, "File check failed — cannot read directory");
            return false;
        }
    };

    // Look for the first entry in the directory.
    match dir.next_entry().await {
        Ok(Some(entry)) => {
            let file_path = entry.path();
            // Delete the trigger file so the check resets on the next poll.
            if let Err(e) = tokio::fs::remove_file(&file_path).await {
                warn!(
                    path = %file_path.display(),
                    error = %e,
                    "File check: found trigger file but failed to delete it"
                );
                // Still count as healthy — we found the file.
            } else {
                info!(
                    path = %file_path.display(),
                    "File check passed — trigger file found and deleted"
                );
            }
            true
        }
        Ok(None) => {
            // Directory is empty — no trigger file present.
            warn!(path, "File check failed — directory is empty");
            false
        }
        Err(e) => {
            warn!(path, error = %e, "File check failed — error reading directory entry");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty temporary directory should cause check_file to return false.
    #[tokio::test]
    async fn test_empty_dir_returns_false() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let result = check_file(dir.path().to_str().unwrap()).await;
        assert!(!result, "empty directory should return false");
    }

    /// A directory containing a file should cause check_file to return true
    /// and the file should be deleted afterwards.
    #[tokio::test]
    async fn test_file_present_returns_true_and_deletes() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("trigger.txt");
        tokio::fs::write(&file_path, "ok").await.expect("write file");

        let result = check_file(dir.path().to_str().unwrap()).await;
        assert!(result, "directory with a file should return true");

        // The file should have been deleted.
        assert!(
            !file_path.exists(),
            "trigger file should be deleted after check"
        );
    }

    /// A path that does not exist should return false.
    #[tokio::test]
    async fn test_nonexistent_dir_returns_false() {
        let result = check_file("/tmp/healthmon_nonexistent_xyz_12345").await;
        assert!(!result, "nonexistent path should return false");
    }
}
