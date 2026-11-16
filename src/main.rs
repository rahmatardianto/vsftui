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
        result: Result<(Vec<(String, PathBuf)>, Vec<PathBuf>)>,
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

    /// Headless mode: Sync entire local workspace to remote and exit
    #[arg(long)]
    sync_local_to_remote: bool,

    /// Headless mode: Sync entire remote directory to local workspace and exit
    #[arg(long)]
    sync_remote_to_local: bool,
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

    // Headless: Sync local workspace -> remote
    if cli.sync_local_to_remote {
        return run_headless_sync_local_to_remote(&workspace_root, active_config).await;
    }

    // Headless: Sync remote -> local workspace
    if cli.sync_remote_to_local {
        return run_headless_sync_remote_to_local(&workspace_root, active_config).await;
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

    drain_headless_conveyer(&mut conveyer_event_rx, queued_count).await?;
    println!("✔ All {} changes synced successfully!", queued_count);
    Ok(())
}

async fn drain_headless_conveyer(
    conveyer_event_rx: &mut mpsc::UnboundedReceiver<ConveyerEvent>,
    queued_count: usize,
) -> Result<()> {
    if queued_count == 0 {
        return Ok(());
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
    Ok(())
}

async fn run_headless_sync_local_to_remote(
    workspace_root: &Path,
    config: SftpConfig,
) -> Result<()> {
    println!("⚡ VSFTUI: Headless Sync Local -> Remote");
    println!("Workspace : {}", workspace_root.display());
    println!("Target    : {}@{}:{}", config.username, config.host, config.remote_path);

    println!("Connecting to remote...");
    let remote_fs = create_remote_fs(config.clone()).await?;
    println!("✔ Connected successfully.");

    let (items, deletes) = compute_local_to_remote_sync(
        Some(Arc::clone(&remote_fs)),
        workspace_root.to_path_buf(),
        workspace_root.to_path_buf(),
        config.remote_path.clone(),
        config.clone(),
    )
    .await?;

    let queued_count = items.len() + deletes.len();
    if queued_count == 0 {
        println!("✔ Nothing to sync (all files up-to-date).");
        return Ok(());
    }

    let (conveyer_event_tx, mut conveyer_event_rx) = mpsc::unbounded_channel();
    let conveyer = Arc::new(Conveyer::new(remote_fs, 4, conveyer_event_tx));
    let use_temp = config.use_temp_file.unwrap_or(false);

    for (local, remote) in items {
        conveyer.enqueue(TransferKind::Upload, Some(local), remote, use_temp);
    }
    for remote in deletes {
        conveyer.enqueue(TransferKind::Delete, None, remote, false);
    }

    drain_headless_conveyer(&mut conveyer_event_rx, queued_count).await?;
    println!("✔ Local -> Remote sync complete ({} task(s)).", queued_count);
    Ok(())
}

async fn run_headless_sync_remote_to_local(
    workspace_root: &Path,
    config: SftpConfig,
) -> Result<()> {
    println!("⚡ VSFTUI: Headless Sync Remote -> Local");
    println!("Remote    : {}@{}:{}", config.username, config.host, config.remote_path);
    println!("Workspace : {}", workspace_root.display());

    println!("Connecting to remote...");
    let remote_fs = create_remote_fs(config.clone()).await?;
    println!("✔ Connected successfully.");

    let (items, deletes) = compute_remote_to_local_sync(
        Arc::clone(&remote_fs),
        workspace_root.to_path_buf(),
        config.remote_path.clone(),
        workspace_root.to_path_buf(),
        config.clone(),
    )
    .await?;

    let queued_count = items.len();
    let delete_count = deletes.len();
    if queued_count == 0 && delete_count == 0 {
        println!("✔ Nothing to sync.");
        return Ok(());
    }

    for path in deletes {
        match std::fs::remove_file(&path) {
            Ok(()) => println!("  ✔ Deleted extra local file: {}", path.display()),
            Err(e) => eprintln!("  ✖ Failed to delete {}: {}", path.display(), e),
        }
    }

    if queued_count > 0 {
        let (conveyer_event_tx, mut conveyer_event_rx) = mpsc::unbounded_channel();
        let conveyer = Arc::new(Conveyer::new(remote_fs, 4, conveyer_event_tx));

        for (remote, local) in items {
            conveyer.enqueue(TransferKind::Download, Some(local), remote, false);
        }

        drain_headless_conveyer(&mut conveyer_event_rx, queued_count).await?;
    }
    println!(
        "✔ Remote -> Local sync complete ({} download(s), {} local delete(s)).",
        queued_count, delete_count
    );
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

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
    }
}

async fn run_tui_app(
    workspace_root: PathBuf,
    config_store: ConfigStore,
    initial_profile: Option<String>,
) -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let _terminal_guard = TerminalGuard;
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
    drop(_terminal_guard);
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
            tokio::spawn(async move {
                let result = compute_local_to_remote_sync(fs_opt, ws, local_dir, remote_dir, cfg).await;
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
            let ws = app.workspace_root.clone();
            let cfg = app.active_config.clone();
            let label = remote_dir.clone();
            tokio::spawn(async move {
                let result = compute_remote_to_local_sync(fs, ws, remote_dir, local_dir, cfg).await;
                let _ = ui_tx.send(UiUpdate::BatchDownloadsReady { label, result });
            });
        }

        AppAction::ChmodLocal { local_path, mode } => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                match std::fs::set_permissions(&local_path, std::fs::Permissions::from_mode(mode)) {
                    Ok(()) => {
                        app.add_log("INFO", format!("✔ chmod {:o} {}", mode, local_path.display()));
                        app.refresh_local_files();
                    }
                    Err(e) => {
                        app.add_log("ERROR", format!("chmod {:o} {} failed: {}", mode, local_path.display(), e));
                    }
                }
            }
            #[cfg(not(unix))]
            {
                let _ = (local_path, mode);
                app.add_log("WARN", "Local chmod is only supported on Unix systems".to_string());
            }
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

#[derive(Clone)]
struct RemoteFileEntry {
    remote_path: String,
    local_path: PathBuf,
    size: u64,
    modified_secs: Option<i64>,
}

fn local_file_meta(path: &Path) -> (bool, Option<i64>, Option<u64>) {
    match std::fs::metadata(path) {
        Ok(meta) => {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64);
            (true, mtime, Some(meta.len()))
        }
        Err(_) => (false, None, None),
    }
}

/// Evaluates whether a file should be transferred according to `syncOption` flags
/// (`skipCreate`, `ignoreExisting`, `update`).
fn should_sync_file(
    target_exists: bool,
    src_mtime_secs: Option<i64>,
    dst_mtime_secs: Option<i64>,
    src_size: Option<u64>,
    dst_size: Option<u64>,
    sync_opt: &config::SyncOption,
) -> bool {
    if !target_exists {
        return !sync_opt.skip_create;
    }
    if sync_opt.ignore_existing {
        return false;
    }
    if sync_opt.update {
        return match (src_mtime_secs, dst_mtime_secs) {
            (Some(src_t), Some(dst_t)) => {
                src_t > dst_t
                    || (src_t == dst_t
                        && src_size.is_some()
                        && dst_size.is_some()
                        && src_size != dst_size)
            }
            _ => true,
        };
    }
    true
}

async fn collect_remote_files(
    fs: Arc<dyn RemoteFs>,
    remote_root: String,
    local_root: PathBuf,
) -> Result<Vec<RemoteFileEntry>> {
    let mut out = Vec::new();
    let mut stack = vec![(remote_root, local_root)];
    while let Some((r_dir, l_dir)) = stack.pop() {
        let entries = tokio::time::timeout(Duration::from_secs(20), fs.read_dir(&r_dir))
            .await
            .map_err(|_| anyhow!("Remote directory listing timed out for {}", r_dir))??;
        for entry in entries {
            if entry.name == "."
                || entry.name == ".."
                || entry.name.is_empty()
                || entry.name.contains('/')
                || entry.name.contains('\\')
            {
                continue;
            }
            let next_local = l_dir.join(&entry.name);
            if entry.is_dir {
                stack.push((entry.path, next_local));
            } else {
                out.push(RemoteFileEntry {
                    remote_path: entry.path,
                    local_path: next_local,
                    size: entry.size,
                    modified_secs: entry.modified.map(|dt| dt.timestamp()),
                });
            }
        }
    }
    Ok(out)
}

fn is_local_path_ignored(workspace_root: &Path, path: &Path, config: &SftpConfig) -> bool {
    if let Ok(rel) = path.strip_prefix(workspace_root) {
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        return config.is_ignored(&rel_str)
            || (path.is_dir() && config.is_ignored(&format!("{}/_", rel_str)));
    }
    false
}

fn compute_extra_remote_deletes_from_entries(
    workspace_root: &Path,
    config: &SftpConfig,
    remote_entries: &[RemoteFileEntry],
    local_items: &[(PathBuf, String)],
) -> Vec<String> {
    let local_remotes: std::collections::HashSet<&str> =
        local_items.iter().map(|(_, r)| r.as_str()).collect();
    remote_entries
        .iter()
        .filter(|e| !is_local_path_ignored(workspace_root, &e.local_path, config))
        .filter(|e| !local_remotes.contains(e.remote_path.as_str()))
        .map(|e| e.remote_path.clone())
        .collect()
}

async fn compute_local_to_remote_sync(
    fs: Option<Arc<dyn RemoteFs>>,
    workspace_root: PathBuf,
    local_dir: PathBuf,
    remote_dir: String,
    config: SftpConfig,
) -> Result<(Vec<(PathBuf, String)>, Vec<String>)> {
    let ws_scan = workspace_root.clone();
    let local_dir_scan = local_dir.clone();
    let remote_dir_scan = remote_dir.clone();
    let cfg_scan = config.clone();
    let all_local_items = tokio::task::spawn_blocking(move || {
        let mut items = Vec::new();
        collect_local_files(&ws_scan, &local_dir_scan, &remote_dir_scan, &cfg_scan, &mut items)?;
        Ok::<_, anyhow::Error>(items)
    })
    .await
    .unwrap_or_else(|e| Err(anyhow!("Local folder scan failed: {}", e)))?;

    let sync_opt = config.sync_option.as_ref();
    let needs_filter = sync_opt.is_some_and(|s| s.skip_create || s.ignore_existing || s.update);
    let needs_delete = sync_opt.is_some_and(|s| s.delete);

    if let Some(fs) = fs
        && (needs_filter || needs_delete)
    {
        let remote_entries = collect_remote_files(fs, remote_dir, local_dir).await?;

        let deletes = if needs_delete {
            compute_extra_remote_deletes_from_entries(
                &workspace_root,
                &config,
                &remote_entries,
                &all_local_items,
            )
        } else {
            Vec::new()
        };

        let items = if let (true, Some(opt)) = (needs_filter, sync_opt) {
            let remote_map: std::collections::HashMap<&str, &RemoteFileEntry> = remote_entries
                .iter()
                .map(|e| (e.remote_path.as_str(), e))
                .collect();
            all_local_items
                .into_iter()
                .filter(|(local_path, remote_path)| {
                    let (_, src_mtime, src_size) = local_file_meta(local_path);
                    match remote_map.get(remote_path.as_str()) {
                        Some(r) => should_sync_file(
                            true,
                            src_mtime,
                            r.modified_secs,
                            src_size,
                            Some(r.size),
                            opt,
                        ),
                        None => should_sync_file(false, src_mtime, None, src_size, None, opt),
                    }
                })
                .collect()
        } else {
            all_local_items
        };

        Ok((items, deletes))
    } else {
        Ok((all_local_items, Vec::new()))
    }
}

fn compute_extra_local_deletes(
    workspace_root: &Path,
    local_dir: &Path,
    remote_dir: &str,
    config: &SftpConfig,
    remote_items: &[(String, PathBuf)],
) -> Result<Vec<PathBuf>> {
    let mut local_items = Vec::new();
    if local_dir.exists() {
        collect_local_files(workspace_root, local_dir, remote_dir, config, &mut local_items)?;
    }
    let remote_locals: std::collections::HashSet<&Path> =
        remote_items.iter().map(|(_, l)| l.as_path()).collect();
    Ok(local_items
        .into_iter()
        .map(|(l_path, _)| l_path)
        .filter(|l_path| !remote_locals.contains(l_path.as_path()))
        .collect())
}

async fn compute_remote_to_local_sync(
    fs: Arc<dyn RemoteFs>,
    workspace_root: PathBuf,
    remote_dir: String,
    local_dir: PathBuf,
    config: SftpConfig,
) -> Result<(Vec<(String, PathBuf)>, Vec<PathBuf>)> {
    let raw_entries = collect_remote_files(fs, remote_dir.clone(), local_dir.clone()).await?;
    let valid_entries: Vec<RemoteFileEntry> = raw_entries
        .into_iter()
        .filter(|e| !is_local_path_ignored(&workspace_root, &e.local_path, &config))
        .collect();

    let sync_opt = config.sync_option.clone();
    let deletes = if sync_opt.as_ref().is_some_and(|s| s.delete) {
        let items_for_scan: Vec<(String, PathBuf)> = valid_entries
            .iter()
            .map(|e| (e.remote_path.clone(), e.local_path.clone()))
            .collect();
        tokio::task::spawn_blocking(move || {
            compute_extra_local_deletes(&workspace_root, &local_dir, &remote_dir, &config, &items_for_scan)
        })
        .await
        .unwrap_or_else(|e| Err(anyhow!("Local cleanup scan failed: {}", e)))?
    } else {
        Vec::new()
    };

    let items: Vec<(String, PathBuf)> = if let Some(opt) = sync_opt.as_ref().filter(|s| s.skip_create || s.ignore_existing || s.update) {
        valid_entries
            .into_iter()
            .filter(|e| {
                let (dst_exists, dst_mtime, dst_size) = local_file_meta(&e.local_path);
                should_sync_file(
                    dst_exists,
                    e.modified_secs,
                    dst_mtime,
                    Some(e.size),
                    dst_size,
                    opt,
                )
            })
            .map(|e| (e.remote_path, e.local_path))
            .collect()
    } else {
        valid_entries
            .into_iter()
            .map(|e| (e.remote_path, e.local_path))
            .collect()
    };

    Ok((items, deletes))
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
            Ok((items, deletes)) => {
                let count = items.len();
                let mut del_count = 0usize;
                for path in deletes {
                    match std::fs::remove_file(&path) {
                        Ok(()) => del_count += 1,
                        Err(e) => app.add_log("ERROR", format!("Failed to delete {}: {}", path.display(), e)),
                    }
                }
                if del_count > 0 {
                    app.refresh_local_files();
                }
                for (remote, local) in items {
                    let _ = app.action_tx.send(AppAction::EnqueueDownload { remote, local });
                }
                app.add_log(
                    "INFO",
                    format!("Enqueued {} download(s), {} local delete(s) from {}", count, del_count, label),
                );
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

    #[test]
    fn test_compute_extra_local_deletes_respects_ignore() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("keep.txt"), "keep").unwrap();
        std::fs::write(root.join("stale.txt"), "stale").unwrap();
        std::fs::write(root.join("debug.log"), "ignored").unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref").unwrap();

        let mut cfg: SftpConfig = serde_json::from_str(
            r#"{"host":"h","username":"u","remotePath":"/r","ignore":["**/*.log"],"syncOption":{"delete":true}}"#,
        )
        .unwrap();
        cfg.finalize().unwrap();

        let remote_items = vec![("/r/keep.txt".to_string(), root.join("keep.txt"))];
        let deletes = compute_extra_local_deletes(root, root, "/r", &cfg, &remote_items).unwrap();

        assert_eq!(deletes, vec![root.join("stale.txt")]);
    }

    #[test]
    fn test_should_sync_file_options() {
        let update_opt = config::SyncOption {
            update: true,
            ..Default::default()
        };
        assert!(should_sync_file(false, Some(100), None, Some(5), None, &update_opt));
        assert!(!should_sync_file(true, Some(100), Some(100), Some(5), Some(5), &update_opt));
        assert!(!should_sync_file(true, Some(90), Some(100), Some(5), Some(5), &update_opt));
        assert!(should_sync_file(true, Some(101), Some(100), Some(5), Some(5), &update_opt));
        assert!(should_sync_file(true, Some(100), Some(100), Some(6), Some(5), &update_opt));
        assert!(should_sync_file(true, Some(100), None, Some(5), Some(5), &update_opt));

        let ignore_existing = config::SyncOption {
            ignore_existing: true,
            ..Default::default()
        };
        assert!(should_sync_file(false, Some(100), None, Some(5), None, &ignore_existing));
        assert!(!should_sync_file(true, Some(200), Some(100), Some(5), Some(5), &ignore_existing));

        let skip_create = config::SyncOption {
            skip_create: true,
            ..Default::default()
        };
        assert!(!should_sync_file(false, Some(100), None, Some(5), None, &skip_create));
        assert!(should_sync_file(true, Some(100), Some(100), Some(5), Some(5), &skip_create));
    }

    struct MockRemoteFs {
        entries: std::collections::HashMap<String, Vec<transfer::fs_provider::FileStat>>,
    }

    #[async_trait::async_trait]
    impl RemoteFs for MockRemoteFs {
        async fn stat(&self, _: &str) -> Result<transfer::fs_provider::FileStat> {
            anyhow::bail!("unused")
        }
        async fn read_dir(&self, remote_path: &str) -> Result<Vec<transfer::fs_provider::FileStat>> {
            Ok(self.entries.get(remote_path).cloned().unwrap_or_default())
        }
        async fn upload(&self, _: &Path, _: &str, _: bool) -> Result<u64> {
            Ok(0)
        }
        async fn download(&self, _: &str, _: &Path) -> Result<u64> {
            Ok(0)
        }
        async fn remove_file(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn remove_dir(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn mkdir_all(&self, _: &str) -> Result<()> {
            Ok(())
        }
        async fn chmod(&self, _: &str, _: u32) -> Result<()> {
            Ok(())
        }
        async fn disconnect(&self) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_sync_update_skips_unchanged_and_preserves_delete_check() {
        use chrono::{TimeZone, Utc};
        use transfer::fs_provider::FileStat;

        let dir = tempdir().unwrap();
        let root = dir.path();
        std::fs::write(root.join("same.txt"), "same").unwrap();
        std::fs::write(root.join("newer.txt"), "newer-content").unwrap();
        std::fs::write(root.join("new_local.txt"), "new").unwrap();

        let same_mtime = local_file_meta(&root.join("same.txt")).1.unwrap();
        let older_remote_mtime = same_mtime - 60;

        let mut entries = std::collections::HashMap::new();
        entries.insert(
            "/r".to_string(),
            vec![
                FileStat {
                    name: "same.txt".to_string(),
                    path: "/r/same.txt".to_string(),
                    is_dir: false,
                    size: 4,
                    modified: Utc.timestamp_opt(same_mtime, 0).single(),
                    permissions: None,
                },
                FileStat {
                    name: "newer.txt".to_string(),
                    path: "/r/newer.txt".to_string(),
                    is_dir: false,
                    size: 13,
                    modified: Utc.timestamp_opt(older_remote_mtime, 0).single(),
                    permissions: None,
                },
                FileStat {
                    name: "extra_remote.txt".to_string(),
                    path: "/r/extra_remote.txt".to_string(),
                    is_dir: false,
                    size: 5,
                    modified: Utc.timestamp_opt(same_mtime, 0).single(),
                    permissions: None,
                },
            ],
        );
        let mock_fs: Arc<dyn RemoteFs> = Arc::new(MockRemoteFs { entries });

        let mut cfg: SftpConfig = serde_json::from_str(
            r#"{"host":"h","username":"u","remotePath":"/r","syncOption":{"update":true,"delete":true}}"#,
        )
        .unwrap();
        cfg.finalize().unwrap();

        // Local -> Remote: should only upload newer.txt and new_local.txt, and delete extra_remote.txt
        let (uploads, remote_deletes) = compute_local_to_remote_sync(
            Some(Arc::clone(&mock_fs)),
            root.to_path_buf(),
            root.to_path_buf(),
            "/r".to_string(),
            cfg.clone(),
        )
        .await
        .unwrap();

        let upload_names: Vec<&str> = uploads.iter().map(|(_, r)| r.as_str()).collect();
        assert!(!upload_names.contains(&"/r/same.txt"));
        assert!(upload_names.contains(&"/r/newer.txt"));
        assert!(upload_names.contains(&"/r/new_local.txt"));
        assert_eq!(remote_deletes, vec!["/r/extra_remote.txt".to_string()]);

        // Remote -> Local: same.txt is up-to-date so skipped, but must NOT be deleted locally!
        let (downloads, local_deletes) = compute_remote_to_local_sync(
            Arc::clone(&mock_fs),
            root.to_path_buf(),
            "/r".to_string(),
            root.to_path_buf(),
            cfg,
        )
        .await
        .unwrap();

        let download_names: Vec<&str> = downloads.iter().map(|(r, _)| r.as_str()).collect();
        assert!(!download_names.contains(&"/r/same.txt"));
        assert!(!download_names.contains(&"/r/newer.txt"));
        assert!(download_names.contains(&"/r/extra_remote.txt"));
        assert_eq!(local_deletes, vec![root.join("new_local.txt")]);
    }
}
