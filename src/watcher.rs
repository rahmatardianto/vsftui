use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use notify::RecursiveMode;
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};
use tokio::sync::mpsc;

use crate::config::SftpConfig;

pub enum WatcherEvent {
    Changed(PathBuf),
}

pub struct WorkspaceWatcher {
    _debouncer: Debouncer<notify::RecommendedWatcher>,
}

impl WorkspaceWatcher {
    pub fn start(
        workspace_root: PathBuf,
        config: SftpConfig,
        event_tx: mpsc::UnboundedSender<WatcherEvent>,
    ) -> Result<Self> {
        let debounce_ms = config
            .git_auto_sync
            .as_ref()
            .map(|g| g.debounce_ms)
            .unwrap_or(300);

        let ws_root = workspace_root.clone();
        let cfg = config.clone();

        let mut debouncer = new_debouncer(
            Duration::from_millis(debounce_ms),
            move |res: DebounceEventResult| {
                if let Ok(events) = res {
                    for event in events {
                        let path = event.path;
                        if let Ok(rel) = path.strip_prefix(&ws_root) {
                            let rel_str = rel.to_string_lossy();
                            if !cfg.is_ignored(&rel_str) {
                                let _ = event_tx.send(WatcherEvent::Changed(rel.to_path_buf()));
                            }
                        }
                    }
                }
            },
        )
        .with_context(|| "Failed to create file debouncer")?;

        watch_non_ignored_dirs(debouncer.watcher(), &workspace_root, &workspace_root, &config)
            .with_context(|| format!("Failed to watch workspace root: {}", workspace_root.display()))?;

        tracing::info!("File watcher started on {}", workspace_root.display());

        Ok(Self {
            _debouncer: debouncer,
        })
    }
}

/// Recursively watch non-ignored directories without registering inotify watches on ignored trees (e.g. target/, .git/).
fn watch_non_ignored_dirs<W: notify::Watcher + ?Sized>(
    watcher: &mut W,
    root: &Path,
    dir: &Path,
    config: &SftpConfig,
) -> Result<()> {
    watcher.watch(dir, RecursiveMode::NonRecursive)?;

    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(()),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if let Ok(rel) = path.strip_prefix(root) {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if config.is_ignored(&rel_str) || config.is_ignored(&format!("{}/_", rel_str)) {
                continue;
            }
        }
        let _ = watch_non_ignored_dirs(watcher, root, &path, config);
    }

    Ok(())
}
