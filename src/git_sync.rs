use std::path::{Path, PathBuf};
use anyhow::{Context, Result};
use git2::{Repository, Status, StatusOptions};

use crate::config::SftpConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitBadge {
    Modified,
    Added,
    Untracked,
    Deleted,
    Renamed,
}

impl GitBadge {
    pub fn as_str(&self) -> &'static str {
        match self {
            GitBadge::Modified => "M ",
            GitBadge::Added => "A ",
            GitBadge::Untracked => "??",
            GitBadge::Deleted => "D ",
            GitBadge::Renamed => "R ",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileOpKind {
    Upload,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitChangeItem {
    pub rel_path: PathBuf,
    pub badge: GitBadge,
    pub op: FileOpKind,
    pub is_staged: bool,
}

pub struct GitInspector;

impl GitInspector {
    /// Scan git repository for all uncommitted changes (working tree + index).
    pub fn inspect_uncommitted(
        workspace_root: &Path,
        config: &SftpConfig,
    ) -> Result<Vec<GitChangeItem>> {
        let repo = match Repository::discover(workspace_root) {
            Ok(r) => r,
            Err(e) => {
                // Not a git repository or git error
                tracing::warn!("Git repository not found at {}: {}", workspace_root.display(), e);
                return Ok(Vec::new());
            }
        };

        let git_settings = config.git_auto_sync.clone().unwrap_or_default();
        let mut opts = StatusOptions::new();
        opts.include_untracked(git_settings.include_untracked)
            .recurse_untracked_dirs(true)
            .renames_head_to_index(true)
            .renames_index_to_workdir(true)
            .include_ignored(false);

        let statuses = repo
            .statuses(Some(&mut opts))
            .with_context(|| "Failed to query git status")?;

        let mut changes = Vec::new();

        for entry in statuses.iter() {
            let path_str = match entry.path() {
                Ok(p) => p,
                Err(_) => continue,
            };

            // Check against sftp.json ignore patterns
            if config.is_ignored(path_str) {
                continue;
            }

            let status = entry.status();
            let mut badge = None;
            let mut op = FileOpKind::Upload;
            let mut is_staged = false;

            // Check Index (Staged) changes
            if status.contains(Status::INDEX_NEW) {
                if git_settings.include_staged {
                    badge = Some(GitBadge::Added);
                    op = FileOpKind::Upload;
                    is_staged = true;
                }
            } else if status.contains(Status::INDEX_MODIFIED) {
                if git_settings.include_staged || git_settings.include_modified {
                    badge = Some(GitBadge::Modified);
                    op = FileOpKind::Upload;
                    is_staged = true;
                }
            } else if status.contains(Status::INDEX_DELETED) {
                if git_settings.propagate_deletes {
                    badge = Some(GitBadge::Deleted);
                    op = FileOpKind::Delete;
                    is_staged = true;
                }
            } else if status.contains(Status::INDEX_RENAMED) {
                if git_settings.include_staged {
                    badge = Some(GitBadge::Renamed);
                    op = FileOpKind::Upload;
                    is_staged = true;
                }
            }

            // Check Working Tree (Unstaged) changes
            if status.contains(Status::WT_NEW) {
                if git_settings.include_untracked {
                    badge = Some(GitBadge::Untracked);
                    op = FileOpKind::Upload;
                    is_staged = false;
                }
            } else if status.contains(Status::WT_MODIFIED) {
                if git_settings.include_modified {
                    badge = Some(GitBadge::Modified);
                    op = FileOpKind::Upload;
                    is_staged = false;
                }
            } else if status.contains(Status::WT_DELETED) {
                if git_settings.propagate_deletes {
                    badge = Some(GitBadge::Deleted);
                    op = FileOpKind::Delete;
                    is_staged = false;
                }
            } else if status.contains(Status::WT_RENAMED) {
                if git_settings.include_modified {
                    badge = Some(GitBadge::Renamed);
                    op = FileOpKind::Upload;
                    is_staged = false;
                }
            }

            if let Some(b) = badge {
                changes.push(GitChangeItem {
                    rel_path: PathBuf::from(path_str),
                    badge: b,
                    op,
                    is_staged,
                });
            }
        }

        changes.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        Ok(changes)
    }

    /// Check if a single file is currently clean (e.g. reverted/restored to HEAD)
    pub fn is_file_clean(workspace_root: &Path, rel_path: &Path) -> Result<bool> {
        let repo = match Repository::discover(workspace_root) {
            Ok(r) => r,
            Err(_) => return Ok(false),
        };

        let status = repo.status_file(rel_path)?;
        // If file status is empty / current, it is clean
        Ok(status.is_empty() || status == Status::CURRENT)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs::File;
    use std::io::Write;
    use crate::config::GitAutoSyncConfig;

    #[test]
    fn test_git_uncommitted_inspection() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path();

        // Initialize git repo
        let repo = Repository::init(repo_path).unwrap();

        // Create an untracked file
        let file1 = repo_path.join("untracked.txt");
        let mut f1 = File::create(&file1).unwrap();
        writeln!(f1, "hello world").unwrap();

        let mut config = SftpConfig {
            name: Some("test".to_string()),
            host: "localhost".to_string(),
            protocol: "sftp".to_string(),
            port: 22,
            username: "test".to_string(),
            password: None,
            private_key_path: None,
            passphrase: None,
            agent: None,
            interactive_auth: None,
            hop: None,
            remote_path: "/remote".to_string(),
            context: ".".to_string(),
            upload_on_save: None,
            use_temp_file: None,
            open_ssh: None,
            download_on_open: None,
            sync_option: None,
            file_perm: None,
            dir_perm: None,
            ignore: vec![],
            watcher: None,
            profiles: None,
            default_profile: None,
            git_auto_sync: Some(GitAutoSyncConfig::default()),
            ignore_glob_set: None,
        };
        config.finalize().unwrap();

        // Scan uncommitted
        let changes = GitInspector::inspect_uncommitted(repo_path, &config).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].rel_path, PathBuf::from("untracked.txt"));
        assert_eq!(changes[0].badge, GitBadge::Untracked);
        assert_eq!(changes[0].op, FileOpKind::Upload);

        // Stage file
        let mut index = repo.index().unwrap();
        index.add_path(Path::new("untracked.txt")).unwrap();
        index.write().unwrap();

        let changes_after_stage = GitInspector::inspect_uncommitted(repo_path, &config).unwrap();
        assert_eq!(changes_after_stage.len(), 1);
        assert_eq!(changes_after_stage[0].badge, GitBadge::Added);
        assert!(changes_after_stage[0].is_staged);
    }
}
