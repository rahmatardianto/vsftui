use std::io::stdout;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use crossterm::event::{Event, EventStream};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc;

mod app;
mod config;
mod git_sync;
mod transfer;
mod ui;
mod watcher;

use app::{App, AppAction, ConnectionStatus};
use config::{ConfigStore, SftpConfig};
use git_sync::{FileOpKind, GitChangeItem, GitInspector};
use transfer::conveyer::{Conveyer, ConveyerEvent, JobStatus, TransferKind};
use transfer::fs_provider::{FileStat, RemoteFs, create_remote_fs};
use watcher::{WatcherEvent, WorkspaceWatcher};

enum UiUpdate {
    ConnectResult {
        profile_name: String,
        remote_dir: String,
        result: Result<Arc<dyn RemoteFs>>,
    },
    RemoteDirLoaded {
        path: String,
        result: Result<Vec<FileStat>>,
    },
    GitStatusRefreshed {
        result: Result<Vec<GitChangeItem>>,
    },
    SyncAllReady {
        result: Result<Vec<GitChangeItem>>,
    },
    BatchUploadsReady {
        label: String,
        result: Result<(Vec<(PathBuf, String)>, Vec<String>)>,
    },
    BatchDownloadsReady {
        label: String,
        result: Result<Vec<(String, PathBuf)>>,
    },
    ChmodDone {
        remote_path: String,
        mode: u32,
        result: Result<()>,
    },
}

#[derive(Parser, Debug)]
#[command(name = "vsftui", author, version, about = "Terminal SFTP/FTP synchronization with vscode-sftp compatibility and Git uncommitted auto-sync")]
struct Cli {
    /// Path to .vscode/sftp.json configuration file
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// Profile name to activate from sftp.json
    #[arg(short, long)]
    profile: Option<String>,

    /// Workspace root directory (defaults to current directory)
    #[arg(short, long, default_value = ".")]
    workspace: PathBuf,

    /// Headless mode: Batch sync all Git uncommitted files to remote and exit
    #[arg(long)]
    sync_uncommitted: bool,

    /// Headless mode: Continuous watch and auto-sync of Git uncommitted files
    #[arg(long)]
    watch_git: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let workspace_root = std::fs::canonicalize(&cli.workspace)
        .with_context(|| format!("Invalid workspace root: {}", cli.workspace.display()))?;

    // Load sftp.json (with JSONC comment stripping and profile resolution)
    let config_store = ConfigStore::discover_and_load(&workspace_root, cli.config.as_deref())
        .with_context(|| "Failed to load sftp.json configuration")?;

    let active_profile_name = cli
        .profile
        .clone()
        .unwrap_or_else(|| config_store.default_profile.clone());

    let active_config = config_store
        .get_profile(Some(&active_profile_name))
        .cloned()
        .ok_or_else(|| anyhow!("Profile '{}' not found in config", active_profile_name))?;

    // Headless: Batch sync uncommitted files
    if cli.sync_uncommitted {
        return run_headless_sync_uncommitted(&workspace_root, active_config).await;
    }

    // Headless: Continuous Git auto-sync watcher
    if cli.watch_git {
        return run_headless_watch_git(&workspace_root, active_config).await;
    }

    // Interactive TUI Mode
    run_tui_app(workspace_root, config_store, cli.profile).await
}

// -----------------------------------------------------------------------------
// Headless: Batch Sync Uncommitted
// -----------------------------------------------------------------------------

async fn run_headless_sync_uncommitted(
    workspace_root: &Path,
    config: SftpConfig,
) -> Result<()> {
    println!("⚡ VSFTUI: Headless Git Uncommitted Batch Sync");
    println!("Workspace : {}", workspace_root.display());
    println!("Target    : {}@{}:{}", config.username, config.host, config.remote_path);

    let changes = GitInspector::inspect_uncommitted(workspace_root, &config)?;
    if changes.is_empty() {
        println!("✔ Working tree is clean. No uncommitted files to sync.");
        return Ok(());
    }

    println!("Found {} uncommitted change(s):", changes.len());
    for c in &changes {
        println!("  [{}] {}", c.badge.as_str(), c.rel_path.display());
    }

    println!("Connecting to remote...");
    let remote_fs = create_remote_fs(config.clone()).await?;
    println!("✔ Connected successfully.");

    let (conveyer_event_tx, mut conveyer_event_rx) = mpsc::unbounded_channel();
    let conveyer = Arc::new(Conveyer::new(remote_fs, 4, conveyer_event_tx));

    let mut queued_count = 0;
    for change in &changes {
        let full_local = workspace_root.join(&change.rel_path);
        let remote_path = format!(
            "{}/{}",
            config.remote_path.trim_end_matches('/'),
            change.rel_path.to_string_lossy().trim_start_matches('/')
        );

        match change.op {
            FileOpKind::Upload => {
                conveyer.enqueue(
                    TransferKind::Upload,
                    Some(full_local),
                    remote_path,
                    config.use_temp_file.unwrap_or(false),
                );
                queued_count += 1;
            }
            FileOpKind::Delete => {
                conveyer.enqueue(TransferKind::Delete, None, remote_path, false);
                queued_count += 1;
            }
        }
    }

    println!("Queued {} task(s). Syncing...", queued_count);

    let mut completed_count = 0;
    let mut failed_count = 0;

    while let Some(evt) = conveyer_event_rx.recv().await {
        match evt {
            ConveyerEvent::JobStarted(id) => {
                println!("  [Job #{}] In progress...", id);
            }
            ConveyerEvent::JobCompleted { id, result } => {
                completed_count += 1;
                match result {
                    Ok(bytes) => {
                        println!("  ✔ [Job #{}] Success ({} bytes)", id, bytes);
                    }
                    Err(e) => {
                        failed_count += 1;
                        eprintln!("  ✖ [Job #{}] FAILED: {}", id, e);
                    }
                }

                if completed_count >= queued_count {
                    break;
                }
            }
            ConveyerEvent::Log(msg) => {
                println!("  [Conveyer] {}", msg);
            }
            _ => {}
        }
    }

    if failed_count > 0 {
        return Err(anyhow!("Sync completed with {} failure(s)", failed_count));
    }

    println!("✔ All {} changes synced successfully!", completed_count);
    Ok(())
}

// -----------------------------------------------------------------------------
// Headless: Continuous Watch & Sync
// -----------------------------------------------------------------------------

async fn run_headless_watch_git(
    workspace_root: &Path,
    config: SftpConfig,
) -> Result<()> {
    println!("⚡ VSFTUI: Headless Continuous Git Auto-Sync Watcher");
    println!("Workspace : {}", workspace_root.display());
    println!("Target    : {}@{}:{}", config.username, config.host, config.remote_path);

    println!("Connecting to remote...");
    let remote_fs = create_remote_fs(config.clone()).await?;
    println!("✔ Connected successfully. Watching filesystem events (Ctrl+C to stop)...");

    let (conveyer_event_tx, mut conveyer_event_rx) = mpsc::unbounded_channel();
    let conveyer = Arc::new(Conveyer::new(remote_fs, 4, conveyer_event_tx));

    let (watcher_tx, mut watcher_rx) = mpsc::unbounded_channel();
    let _watcher = WorkspaceWatcher::start(
        workspace_root.to_path_buf(),
        config.clone(),
        watcher_tx,
    )?;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\nShutdown signal received. Exiting...");
                break;
            }
            Some(w_evt) = watcher_rx.recv() => {
                match w_evt {
                    WatcherEvent::Changed(rel_path) => {
                        let is_clean = GitInspector::is_file_clean(workspace_root, &rel_path).unwrap_or(false);
                        let full_local = workspace_root.join(&rel_path);
                        let remote_path = format!(
                            "{}/{}",
                            config.remote_path.trim_end_matches('/'),
                            rel_path.to_string_lossy().trim_start_matches('/')
                        );

                        if is_clean {
                            if full_local.exists() {
                                println!("⚡ [Auto-Sync] Clean/Reverted: Syncing {}", rel_path.display());
                                conveyer.enqueue(
                                    TransferKind::Upload,
                                    Some(full_local),
                                    remote_path,
                                    config.use_temp_file.unwrap_or(false),
                                );
                            }
                        } else if let Ok(changes) = GitInspector::inspect_uncommitted(workspace_root, &config) {
                            if let Some(change) = changes.iter().find(|c| c.rel_path == rel_path) {
                                println!("⚡ [Auto-Sync] Dirty [{:?}]: Syncing {}", change.badge, rel_path.display());
                                match change.op {
                                    FileOpKind::Upload => {
                                        conveyer.enqueue(
                                            TransferKind::Upload,
                                            Some(full_local),
                                            remote_path,
                                            config.use_temp_file.unwrap_or(false),
                                        );
                                    }
                                    FileOpKind::Delete => {
                                        conveyer.enqueue(TransferKind::Delete, None, remote_path, false);
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Some(c_evt) = conveyer_event_rx.recv() => {
                match c_evt {
                    ConveyerEvent::JobCompleted { id, result } => {
                        match result {
                            Ok(bytes) => println!("  ✔ Job #{} completed ({} bytes)", id, bytes),
                            Err(e) => eprintln!("  ✖ Job #{} failed: {}", id, e),
                        }
                    }
                    ConveyerEvent::Log(msg) => println!("  [Conveyer] {}", msg),
                    _ => {}
                }
            }
        }
    }

    Ok(())
}

// -----------------------------------------------------------------------------
// Interactive TUI Application
// -----------------------------------------------------------------------------

async fn run_tui_app(
    workspace_root: PathBuf,
    config_store: ConfigStore,
    initial_profile: Option<String>,
) -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Channels for UI event loop
    let (action_tx, mut action_rx) = mpsc::unbounded_channel::<AppAction>();
    let (conveyer_event_tx, mut conveyer_event_rx) = mpsc::unbounded_channel::<ConveyerEvent>();
    let (watcher_tx, mut watcher_rx) = mpsc::unbounded_channel::<WatcherEvent>();

    let mut app = App::new(
        workspace_root.clone(),
        config_store,
        initial_profile,
        action_tx.clone(),
        conveyer_event_tx.clone(),
    )?;

    // Start file system watcher
    let watcher = WorkspaceWatcher::start(
        workspace_root.clone(),
        app.active_config.clone(),
        watcher_tx,
    );
    match watcher {
        Ok(w) => app._watcher = Some(w),
        Err(e) => app.add_log("WARN", format!("Watcher warning: {}", e)),
    }

    let (ui_tx, mut ui_rx) = mpsc::unbounded_channel::<UiUpdate>();

    // Initial connection attempt
    let _ = action_tx.send(AppAction::ConnectProfile(app.active_profile_name.clone()));

    let mut reader = EventStream::new();
    let mut tick_interval = tokio::time::interval(Duration::from_millis(250));
    let mut needs_redraw = true;
    let mut git_refresh_in_flight = false;
    let mut git_refresh_queued = false;

    // Async Event Loop: zero blocking on UI thread
    loop {
        if needs_redraw {
            terminal.draw(|f| ui::draw(f, &app))?;
            needs_redraw = false;
        }

        if app.should_quit {
            break;
        }

        tokio::select! {
            _ = tick_interval.tick() => {
                // Periodic refresh only when transfer queue has active jobs or changed length
                if let Some(ref c) = app.conveyer {
                    let jobs = c.get_jobs();
                    let has_active = jobs.iter().any(|j| matches!(j.status, JobStatus::Pending | JobStatus::Running | JobStatus::Retrying(_)));
                    if has_active || jobs.len() != app.queue_jobs.len() {
                        app.queue_jobs = jobs;
                        needs_redraw = true;
                    }
                }
            }
            Some(Ok(event)) = reader.next() => {
                match event {
                    Event::Key(key_event) => {
                        app.handle_key_event(key_event);
                        needs_redraw = true;
                    }
                    Event::Resize(_, _) => {
                        needs_redraw = true;
                    }
                    _ => {}
                }
            }
            Some(w_event) = watcher_rx.recv() => {
                match w_event {
                    WatcherEvent::Changed(rel_path) => {
                        app.handle_file_changed(rel_path);
                        needs_redraw = true;
                    }
                }
            }
            Some(c_event) = conveyer_event_rx.recv() => {
                match c_event {
                    ConveyerEvent::JobAdded(job) => {
                        app.add_log("INFO", format!("Enqueued {} target: {}", job.kind.as_str(), job.remote_path));
                        if let Some(ref c) = app.conveyer {
                            app.queue_jobs = c.get_jobs();
                        }
                    }
                    ConveyerEvent::JobStarted(id) => {
                        app.add_log("INFO", format!("Transfer #{} started", id));
                        if let Some(ref c) = app.conveyer {
                            app.queue_jobs = c.get_jobs();
                        }
                    }
                    ConveyerEvent::JobProgress { .. } => {}
                    ConveyerEvent::JobCompleted { id, result } => {
                        match result {
                            Ok(bytes) => {
                                app.add_log("INFO", format!("Transfer #{} finished ({} B)", id, bytes));
                            }
                            Err(err) => {
                                app.add_log("ERROR", format!("Transfer #{} failed: {}", id, err));
                            }
                        }
                        if let Some(ref c) = app.conveyer {
                            app.queue_jobs = c.get_jobs();
                        }
                        // Refresh views after completion
                        let _ = action_tx.send(AppAction::RefreshGitStatus);
                        let target_remote = app.remote_current_dir.clone();
                        let _ = action_tx.send(AppAction::LoadRemoteDir(target_remote));
                    }
                    ConveyerEvent::Log(msg) => {
                        app.add_log("INFO", msg);
                    }
                }
                needs_redraw = true;
            }
            Some(action) = action_rx.recv() => {
                if matches!(action, AppAction::RefreshGitStatus) {
                    if git_refresh_in_flight {
                        git_refresh_queued = true;
                    } else {
                        git_refresh_in_flight = true;
                        handle_app_action(&mut app, action, ui_tx.clone());
                    }
                } else {
                    handle_app_action(&mut app, action, ui_tx.clone());
                }
                needs_redraw = true;
            }
            Some(update) = ui_rx.recv() => {
                if matches!(update, UiUpdate::GitStatusRefreshed { .. }) {
                    git_refresh_in_flight = false;
                    if git_refresh_queued {
                        git_refresh_queued = false;
                        let _ = action_tx.send(AppAction::RefreshGitStatus);
                    }
                }
                handle_ui_update(&mut app, update, conveyer_event_tx.clone());
                needs_redraw = true;
            }
        }
    }

    // Cleanup terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

fn handle_app_action(
    app: &mut App,
    action: AppAction,
    ui_tx: mpsc::UnboundedSender<UiUpdate>,
) {
    match action {
        AppAction::ConnectProfile(p_name) => {
            app.connection_status = ConnectionStatus::Connecting;
            app.add_log("INFO", format!("Connecting to profile '{}'...", p_name));

            let cfg = match app.config_store.get_profile(Some(&p_name)) {
                Some(c) => c.clone(),
                None => {
                    app.connection_status = ConnectionStatus::Error(format!("Profile {} not found", p_name));
                    return;
                }
            };

            let remote_dir = cfg.remote_path.clone();

            tokio::spawn(async move {
                let result = tokio::time::timeout(Duration::from_secs(20), create_remote_fs(cfg))
                    .await
                    .unwrap_or_else(|_| Err(anyhow!("Connection timed out after 20s")));
                let _ = ui_tx.send(UiUpdate::ConnectResult {
                    profile_name: p_name,
                    remote_dir,
                    result,
                });
            });
        }

        AppAction::LoadRemoteDir(path) => {
            if app.abort_if_conn_error() {
                return;
            }
            if let Some(ref fs) = app.remote_fs {
                let fs = Arc::clone(fs);
                tokio::spawn(async move {
                    let result = tokio::time::timeout(Duration::from_secs(20), fs.read_dir(&path))
                        .await
                        .unwrap_or_else(|_| Err(anyhow!("Remote directory listing timed out")));
                    let _ = ui_tx.send(UiUpdate::RemoteDirLoaded { path, result });
                });
            }
        }

        AppAction::LoadLocalDir(p) => {
            // Internal sync notification
            let _ = p;
        }

        AppAction::RefreshGitStatus => {
            let ws = app.workspace_root.clone();
            let cfg = app.active_config.clone();
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || GitInspector::inspect_uncommitted(&ws, &cfg))
                    .await
                    .unwrap_or_else(|e| Err(anyhow!("Git status task failed: {}", e)));
                let _ = ui_tx.send(UiUpdate::GitStatusRefreshed { result });
            });
        }

        AppAction::EnqueueUpload { local, remote, use_temp } => {
            if app.abort_if_conn_error() {
                return;
            }
            if let Some(ref conveyer) = app.conveyer {
                conveyer.enqueue(TransferKind::Upload, Some(local), remote, use_temp);
            } else {
                app.add_log("WARN", "Cannot upload: not connected to remote".to_string());
            }
        }

        AppAction::EnqueueDownload { remote, local } => {
            if app.abort_if_conn_error() {
                return;
            }
            if let Some(ref conveyer) = app.conveyer {
                conveyer.enqueue(TransferKind::Download, Some(local), remote, false);
            } else {
                app.add_log("WARN", "Cannot download: not connected to remote".to_string());
            }
        }

        AppAction::EnqueueDelete { remote } => {
            if app.abort_if_conn_error() {
                return;
            }
            if let Some(ref conveyer) = app.conveyer {
                conveyer.enqueue(TransferKind::Delete, None, remote, false);
            } else {
                app.add_log("WARN", "Cannot delete: not connected to remote".to_string());
            }
        }

        AppAction::SyncAllUncommitted => {
            if app.abort_if_conn_error() {
                return;
            }
            let ws = app.workspace_root.clone();
            let cfg = app.active_config.clone();
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || GitInspector::inspect_uncommitted(&ws, &cfg))
                    .await
                    .unwrap_or_else(|e| Err(anyhow!("Git status task failed: {}", e)));
                let _ = ui_tx.send(UiUpdate::SyncAllReady { result });
            });
        }

        AppAction::SyncLocalDir { local_dir, remote_dir } => {
            if app.abort_if_conn_error() {
                return;
            }
            let ws = app.workspace_root.clone();
            let cfg = app.active_config.clone();
            let fs_opt = app.remote_fs.clone();
            let label = local_dir.display().to_string();
            let delete_extra = cfg.sync_option.as_ref().is_some_and(|s| s.delete);
            let remote_dir_scan = remote_dir.clone();
            let local_dir_scan = local_dir.clone();
            tokio::spawn(async move {
                let res = tokio::task::spawn_blocking(move || {
                    let mut items = Vec::new();
                    collect_local_files(&ws, &local_dir, &remote_dir, &cfg, &mut items)?;
                    Ok::<_, anyhow::Error>(items)
                })
                .await
                .unwrap_or_else(|e| Err(anyhow!("Local folder scan failed: {}", e)));

                let result = match res {
                    Ok(items) => {
                        let mut deletes = Vec::new();
                        if delete_extra {
                            if let Some(fs) = fs_opt {
                                if let Ok(remote_items) = collect_remote_files(fs, remote_dir_scan, local_dir_scan).await {
                                    let local_remotes: std::collections::HashSet<&str> =
                                        items.iter().map(|(_, r)| r.as_str()).collect();
                                    for (r_path, _) in remote_items {
                                        if !local_remotes.contains(r_path.as_str()) {
                                            deletes.push(r_path);
                                        }
                                    }
                                }
                            }
                        }
                        Ok((items, deletes))
                    }
                    Err(e) => Err(e),
                };
                let _ = ui_tx.send(UiUpdate::BatchUploadsReady { label, result });
            });
        }

        AppAction::SyncRemoteDir { remote_dir, local_dir } => {
            if app.abort_if_conn_error() {
                return;
            }
            let Some(ref fs) = app.remote_fs else {
                app.add_log("WARN", "Cannot sync folder: not connected to remote".to_string());
                return;
            };
            let fs = Arc::clone(fs);
            let label = remote_dir.clone();
            tokio::spawn(async move {
                let result = collect_remote_files(fs, remote_dir, local_dir).await;
                let _ = ui_tx.send(UiUpdate::BatchDownloadsReady { label, result });
            });
        }

        AppAction::ChmodRemote { remote_path, mode } => {
            if app.abort_if_conn_error() {
                return;
            }
            let Some(ref fs) = app.remote_fs else {
                app.add_log("WARN", "Cannot chmod: not connected to remote".to_string());
                return;
            };
            let fs = Arc::clone(fs);
            tokio::spawn(async move {
                let result = tokio::time::timeout(Duration::from_secs(15), fs.chmod(&remote_path, mode))
                    .await
                    .unwrap_or_else(|_| Err(anyhow!("Chmod timed out")));
                let _ = ui_tx.send(UiUpdate::ChmodDone {
                    remote_path,
                    mode,
                    result,
                });
            });
        }
    }
}

fn collect_local_files(
    workspace_root: &Path,
    local_dir: &Path,
    remote_dir: &str,
    config: &SftpConfig,
    out: &mut Vec<(PathBuf, String)>,
) -> Result<()> {
    for entry in std::fs::read_dir(local_dir)? {
        let entry = entry?;
        let path = entry.path();
        if let Ok(rel) = path.strip_prefix(workspace_root) {
            let rel_str = rel.to_string_lossy().replace('\\', "/");
            if config.is_ignored(&rel_str) || (path.is_dir() && config.is_ignored(&format!("{}/_", rel_str))) {
                continue;
            }
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let next_remote = format!("{}/{}", remote_dir.trim_end_matches('/'), name);
        if path.is_dir() {
            collect_local_files(workspace_root, &path, &next_remote, config, out)?;
        } else if path.is_file() {
            out.push((path, next_remote));
        }
    }
    Ok(())
}

async fn collect_remote_files(
    fs: Arc<dyn RemoteFs>,
    remote_root: String,
    local_root: PathBuf,
) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    let mut stack = vec![(remote_root, local_root)];
    while let Some((r_dir, l_dir)) = stack.pop() {
        let entries = tokio::time::timeout(Duration::from_secs(20), fs.read_dir(&r_dir))
            .await
            .map_err(|_| anyhow!("Remote directory listing timed out for {}", r_dir))??;
        for entry in entries {
            if entry.name == "." || entry.name == ".." {
                continue;
            }
            let next_local = l_dir.join(&entry.name);
            if entry.is_dir {
                stack.push((entry.path, next_local));
            } else {
                out.push((entry.path, next_local));
            }
        }
    }
    Ok(out)
}

fn handle_ui_update(
    app: &mut App,
    update: UiUpdate,
    conveyer_event_tx: mpsc::UnboundedSender<ConveyerEvent>,
) {
    match update {
        UiUpdate::ConnectResult {
            profile_name,
            remote_dir,
            result,
        } => {
            if app.active_profile_name != profile_name {
                return;
            }
            match result {
                Ok(remote_fs) => {
                    app.connection_status = ConnectionStatus::Connected;
                    app.add_log("INFO", format!("✔ Connected to '{}'", profile_name));

                    let conveyer = Arc::new(Conveyer::new(
                        Arc::clone(&remote_fs),
                        4,
                        conveyer_event_tx,
                    ));
                    app.remote_fs = Some(Arc::clone(&remote_fs));
                    app.conveyer = Some(conveyer);

                    let _ = app.action_tx.send(AppAction::LoadRemoteDir(remote_dir));
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    app.connection_status = ConnectionStatus::Error(err_msg.clone());
                    app.add_log("ERROR", format!("Connection failed: {}", err_msg));
                }
            }
        }
        UiUpdate::RemoteDirLoaded { path, result } => match result {
            Ok(entries) => {
                app.remote_entries = entries;
                app.remote_current_dir = path;
                if app.remote_cursor >= app.remote_entries.len() {
                    app.remote_cursor = app.remote_entries.len().saturating_sub(1);
                }
            }
            Err(e) => {
                app.add_log("WARN", format!("Failed to read remote dir {}: {}", path, e));
            }
        },
        UiUpdate::GitStatusRefreshed { result } => match result {
            Ok(changes) => {
                app.git_changes = changes;
                if app.git_cursor >= app.git_changes.len() {
                    app.git_cursor = app.git_changes.len().saturating_sub(1);
                }
            }
            Err(e) => {
                app.add_log("WARN", format!("Git status error: {}", e));
            }
        },
        UiUpdate::SyncAllReady { result } => match result {
            Ok(changes) => {
                app.git_changes = changes;
                if app.git_cursor >= app.git_changes.len() {
                    app.git_cursor = app.git_changes.len().saturating_sub(1);
                }
                if app.git_changes.is_empty() {
                    app.add_log("INFO", "Git status clean. Nothing to sync.".to_string());
                    return;
                }

                let mut count = 0;
                for change in &app.git_changes {
                    let full_local = app.workspace_root.join(&change.rel_path);
                    let remote_path = format!(
                        "{}/{}",
                        app.active_config.remote_path.trim_end_matches('/'),
                        change.rel_path.to_string_lossy().trim_start_matches('/')
                    );

                    match change.op {
                        FileOpKind::Upload => {
                            let _ = app.action_tx.send(AppAction::EnqueueUpload {
                                local: full_local,
                                remote: remote_path,
                                use_temp: app.active_config.use_temp_file.unwrap_or(false),
                            });
                            count += 1;
                        }
                        FileOpKind::Delete => {
                            let _ = app.action_tx.send(AppAction::EnqueueDelete {
                                remote: remote_path,
                            });
                            count += 1;
                        }
                    }
                }

                app.add_log("INFO", format!("Enqueued {} Git change(s) for batch sync", count));
            }
            Err(e) => {
                app.add_log("WARN", format!("Git status error: {}", e));
            }
        },
        UiUpdate::BatchUploadsReady { label, result } => match result {
            Ok((items, deletes)) => {
                let up_count = items.len();
                let del_count = deletes.len();
                let use_temp = app.active_config.use_temp_file.unwrap_or(false);
                for (local, remote) in items {
                    let _ = app.action_tx.send(AppAction::EnqueueUpload {
                        local,
                        remote,
                        use_temp,
                    });
                }
                for remote in deletes {
                    let _ = app.action_tx.send(AppAction::EnqueueDelete { remote });
                }
                app.add_log(
                    "INFO",
                    format!("Enqueued {} upload(s), {} delete(s) from {}", up_count, del_count, label),
                );
            }
            Err(e) => {
                app.add_log("ERROR", format!("Batch upload scan failed ({}): {}", label, e));
            }
        },
        UiUpdate::BatchDownloadsReady { label, result } => match result {
            Ok(items) => {
                let count = items.len();
                for (remote, local) in items {
                    let _ = app.action_tx.send(AppAction::EnqueueDownload { remote, local });
                }
                app.add_log("INFO", format!("Enqueued {} file(s) from {}", count, label));
            }
            Err(e) => {
                app.add_log("ERROR", format!("Batch download scan failed ({}): {}", label, e));
            }
        },
        UiUpdate::ChmodDone { remote_path, mode, result } => match result {
            Ok(()) => {
                app.add_log("INFO", format!("✔ chmod {:o} {}", mode, remote_path));
                let target_remote = app.remote_current_dir.clone();
                let _ = app.action_tx.send(AppAction::LoadRemoteDir(target_remote));
            }
            Err(e) => {
                app.add_log("ERROR", format!("chmod {:o} {} failed: {}", mode, remote_path, e));
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_collect_local_files_skips_ignored() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("sub/nested")).unwrap();
        std::fs::create_dir_all(root.join("sub/.git")).unwrap();
        std::fs::write(root.join("sub/a.txt"), "a").unwrap();
        std::fs::write(root.join("sub/nested/b.txt"), "b").unwrap();
        std::fs::write(root.join("sub/.git/HEAD"), "ref").unwrap();

        let mut cfg: SftpConfig = serde_json::from_str(r#"{"host":"h","username":"u","remotePath":"/r"}"#).unwrap();
        cfg.finalize().unwrap();

        let mut out = Vec::new();
        collect_local_files(root, &root.join("sub"), "/r/sub", &cfg, &mut out).unwrap();
        let remotes: Vec<&str> = out.iter().map(|(_, r)| r.as_str()).collect();
        assert!(remotes.contains(&"/r/sub/a.txt"));
        assert!(remotes.contains(&"/r/sub/nested/b.txt"));
        assert!(!remotes.iter().any(|r| r.contains(".git")));
    }
}
