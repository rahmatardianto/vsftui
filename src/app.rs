use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tokio::sync::mpsc;

use crate::config::{ConfigStore, SftpConfig};
use crate::git_sync::{FileOpKind, GitChangeItem};
use crate::transfer::conveyer::{Conveyer, ConveyerEvent, TransferJob};
use crate::transfer::fs_provider::{FileStat, RemoteFs};
use crate::watcher::WorkspaceWatcher;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePane {
    Left,
    Right,
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeftTab {
    GitUncommitted,
    LocalExplorer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BottomTab {
    TransferQueue,
    Logs,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectionStatus {
    Disconnected,
    Connecting,
    Connected,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct LocalFileEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone)]
pub struct LogItem {
    pub timestamp: DateTime<Local>,
    pub level: String,
    pub message: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum ModalState {
    ProfileSwitcher { selected: usize },
    PasswordPrompt { buffer: String },
    ChmodPrompt { remote_path: String, name: String, buffer: String },
    Help,
}

#[allow(dead_code)]
pub enum AppAction {
    ConnectProfile(String),
    LoadRemoteDir(String),
    LoadLocalDir(PathBuf),
    RefreshGitStatus,
    EnqueueUpload { local: PathBuf, remote: String, use_temp: bool },
    EnqueueDownload { remote: String, local: PathBuf },
    EnqueueDelete { remote: String },
    SyncAllUncommitted,
    SyncLocalDir { local_dir: PathBuf, remote_dir: String },
    SyncRemoteDir { remote_dir: String, local_dir: PathBuf },
    ChmodRemote { remote_path: String, mode: u32 },
}

pub struct App {
    pub workspace_root: PathBuf,
    pub config_store: ConfigStore,
    pub active_profile_name: String,
    pub active_config: SftpConfig,

    pub remote_fs: Option<Arc<dyn RemoteFs>>,
    pub conveyer: Option<Arc<Conveyer>>,
    pub _watcher: Option<WorkspaceWatcher>,

    pub connection_status: ConnectionStatus,

    // UI state
    pub active_pane: ActivePane,
    pub left_tab: LeftTab,
    pub bottom_tab: BottomTab,

    pub local_current_dir: PathBuf,
    pub local_entries: Vec<LocalFileEntry>,
    pub local_cursor: usize,

    pub remote_current_dir: String,
    pub remote_entries: Vec<FileStat>,
    pub remote_cursor: usize,

    pub git_changes: Vec<GitChangeItem>,
    pub git_cursor: usize,

    pub queue_jobs: Vec<TransferJob>,
    pub queue_cursor: usize,

    pub logs: VecDeque<LogItem>,
    pub logs_scroll: usize,

    pub modal: Option<ModalState>,
    pub should_quit: bool,

    // Communication channels
    pub action_tx: mpsc::UnboundedSender<AppAction>,
    #[allow(dead_code)]
    pub conveyer_event_tx: mpsc::UnboundedSender<ConveyerEvent>,
}

impl App {
    pub fn new(
        workspace_root: PathBuf,
        config_store: ConfigStore,
        initial_profile: Option<String>,
        action_tx: mpsc::UnboundedSender<AppAction>,
        conveyer_event_tx: mpsc::UnboundedSender<ConveyerEvent>,
    ) -> Result<Self> {
        let active_profile_name = initial_profile
            .clone()
            .unwrap_or_else(|| config_store.default_profile.clone());

        let active_config = config_store
            .get_profile(Some(&active_profile_name))
            .cloned()
            .with_context(|| format!("Profile '{}' not found", active_profile_name))?;

        let local_current_dir = workspace_root.clone();
        let remote_current_dir = active_config.remote_path.clone();

        let mut app = Self {
            workspace_root,
            config_store,
            active_profile_name,
            active_config,
            remote_fs: None,
            conveyer: None,
            _watcher: None,
            connection_status: ConnectionStatus::Disconnected,
            active_pane: ActivePane::Left,
            left_tab: LeftTab::GitUncommitted,
            bottom_tab: BottomTab::TransferQueue,
            local_current_dir,
            local_entries: Vec::new(),
            local_cursor: 0,
            remote_current_dir,
            remote_entries: Vec::new(),
            remote_cursor: 0,
            git_changes: Vec::new(),
            git_cursor: 0,
            queue_jobs: Vec::new(),
            queue_cursor: 0,
            logs: VecDeque::with_capacity(200),
            logs_scroll: 0,
            modal: None,
            should_quit: false,
            action_tx,
            conveyer_event_tx,
        };

        app.add_log("INFO", "SFTUI initialized. Connecting to remote...".to_string());
        app.refresh_local_files();
        let _ = app.action_tx.send(AppAction::RefreshGitStatus);

        Ok(app)
    }

    pub fn add_log(&mut self, level: &str, message: String) {
        if self.logs.back().is_some_and(|last| last.level == level && last.message == message) {
            return;
        }
        if self.logs.len() >= 200 {
            self.logs.pop_front();
        }
        self.logs.push_back(LogItem {
            timestamp: Local::now(),
            level: level.to_string(),
            message,
        });
    }

    pub fn abort_if_conn_error(&mut self) -> bool {
        if let ConnectionStatus::Error(err) = &self.connection_status {
            self.add_log("ERROR", format!("Connection error: {}", err));
            return true;
        }
        false
    }

    pub fn refresh_local_files(&mut self) {
        let mut entries = Vec::new();
        if let Ok(read_dir) = std::fs::read_dir(&self.local_current_dir) {
            for entry in read_dir.flatten() {
                let path = entry.path();
                let file_name = entry.file_name().to_string_lossy().to_string();

                let rel = path.strip_prefix(&self.workspace_root).unwrap_or(&path);
                if self.active_config.is_ignored(&rel.to_string_lossy()) {
                    continue;
                }

                let is_dir = entry.file_type().map(|ft| ft.is_dir()).unwrap_or(false);
                let metadata = entry.metadata().ok();
                let size = metadata.as_ref().map(|m| m.len()).unwrap_or(0);
                let modified = metadata
                    .and_then(|m| m.modified().ok())
                    .map(|sys_time| DateTime::<Utc>::from(sys_time));

                entries.push(LocalFileEntry {
                    name: file_name,
                    path,
                    is_dir,
                    size,
                    modified,
                });
            }
        }

        entries.sort_by(|a, b| {
            if a.is_dir != b.is_dir {
                b.is_dir.cmp(&a.is_dir)
            } else {
                a.name.to_lowercase().cmp(&b.name.to_lowercase())
            }
        });

        self.local_entries = entries;
        if self.local_cursor >= self.local_entries.len() {
            self.local_cursor = self.local_entries.len().saturating_sub(1);
        }
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) {
        // Modal takes precedence
        if let Some(modal) = self.modal.clone() {
            match modal {
                ModalState::Help => {
                    if matches!(key.code, KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?')) {
                        self.modal = None;
                    }
                    return;
                }
                ModalState::ProfileSwitcher { selected } => {
                    let profiles = self.config_store.profile_names();
                    match key.code {
                        KeyCode::Esc => {
                            self.modal = None;
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            let new_sel = selected.saturating_sub(1);
                            self.modal = Some(ModalState::ProfileSwitcher { selected: new_sel });
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            let new_sel = (selected + 1).min(profiles.len().saturating_sub(1));
                            self.modal = Some(ModalState::ProfileSwitcher { selected: new_sel });
                        }
                        KeyCode::Enter => {
                            if let Some(p_name) = profiles.get(selected) {
                                let target = p_name.clone();
                                self.modal = None;
                                self.switch_profile(target);
                            }
                        }
                        _ => {}
                    }
                    return;
                }
                ModalState::PasswordPrompt { mut buffer } => {
                    match key.code {
                        KeyCode::Esc => {
                            self.modal = None;
                        }
                        KeyCode::Char(c) => {
                            buffer.push(c);
                            self.modal = Some(ModalState::PasswordPrompt { buffer });
                        }
                        KeyCode::Backspace => {
                            buffer.pop();
                            self.modal = Some(ModalState::PasswordPrompt { buffer });
                        }
                        KeyCode::Enter => {
                            self.active_config.password = Some(buffer);
                            self.modal = None;
                            let prof = self.active_profile_name.clone();
                            let _ = self.action_tx.send(AppAction::ConnectProfile(prof));
                        }
                        _ => {}
                    }
                    return;
                }
                ModalState::ChmodPrompt { remote_path, name, mut buffer } => {
                    match key.code {
                        KeyCode::Esc => {
                            self.modal = None;
                        }
                        KeyCode::Char(c) if ('0'..='7').contains(&c) && buffer.len() < 4 => {
                            buffer.push(c);
                            self.modal = Some(ModalState::ChmodPrompt { remote_path, name, buffer });
                        }
                        KeyCode::Backspace => {
                            buffer.pop();
                            self.modal = Some(ModalState::ChmodPrompt { remote_path, name, buffer });
                        }
                        KeyCode::Enter => {
                            if let Ok(mode) = u32::from_str_radix(buffer.trim(), 8) {
                                self.modal = None;
                                let _ = self.action_tx.send(AppAction::ChmodRemote { remote_path, mode });
                            } else {
                                self.add_log("WARN", format!("Invalid octal mode: '{}'", buffer));
                            }
                        }
                        _ => {}
                    }
                    return;
                }
            }
        }

        // Global keybindings
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
                self.should_quit = true;
                return;
            }
            (KeyCode::Char('?'), KeyModifiers::NONE) | (KeyCode::Char('h'), KeyModifiers::CONTROL) => {
                self.modal = Some(ModalState::Help);
                return;
            }
            (KeyCode::Char('p'), KeyModifiers::NONE) => {
                let profiles = self.config_store.profile_names();
                let current_idx = profiles
                    .iter()
                    .position(|p| p == &self.active_profile_name)
                    .unwrap_or(0);
                self.modal = Some(ModalState::ProfileSwitcher { selected: current_idx });
                return;
            }
            (KeyCode::Tab, KeyModifiers::NONE) => {
                self.active_pane = match self.active_pane {
                    ActivePane::Left => ActivePane::Right,
                    ActivePane::Right => ActivePane::Bottom,
                    ActivePane::Bottom => ActivePane::Left,
                };
                return;
            }
            (KeyCode::Char('1'), KeyModifiers::NONE) => {
                match self.active_pane {
                    ActivePane::Left => self.left_tab = LeftTab::GitUncommitted,
                    ActivePane::Bottom => self.bottom_tab = BottomTab::TransferQueue,
                    _ => {}
                }
                return;
            }
            (KeyCode::Char('2'), KeyModifiers::NONE) => {
                match self.active_pane {
                    ActivePane::Left => self.left_tab = LeftTab::LocalExplorer,
                    ActivePane::Bottom => self.bottom_tab = BottomTab::Logs,
                    _ => {}
                }
                return;
            }
            (KeyCode::Char('G'), KeyModifiers::SHIFT) | (KeyCode::Char('G'), KeyModifiers::NONE) => {
                if self.abort_if_conn_error() {
                    return;
                }
                self.add_log("INFO", "Batch syncing all uncommitted Git changes...".to_string());
                let _ = self.action_tx.send(AppAction::SyncAllUncommitted);
                return;
            }
            (KeyCode::Char('r'), KeyModifiers::NONE) => {
                self.refresh_local_files();
                let _ = self.action_tx.send(AppAction::RefreshGitStatus);
                let _ = self.action_tx.send(AppAction::LoadRemoteDir(self.remote_current_dir.clone()));
                self.add_log("INFO", "Refreshed views".to_string());
                return;
            }
            (KeyCode::Char('c'), KeyModifiers::NONE) => {
                if let Some(ref c) = self.conveyer {
                    c.clear_completed();
                    self.queue_jobs = c.get_jobs();
                    self.add_log("INFO", "Cleared completed jobs".to_string());
                }
                return;
            }
            _ => {}
        }

        // Pane-specific navigation and operations
        match self.active_pane {
            ActivePane::Left => match self.left_tab {
                LeftTab::GitUncommitted => self.handle_git_tab_key(key.code),
                LeftTab::LocalExplorer => self.handle_local_tab_key(key.code),
            },
            ActivePane::Right => self.handle_remote_tab_key(key.code),
            ActivePane::Bottom => match self.bottom_tab {
                BottomTab::TransferQueue => self.handle_queue_tab_key(key.code),
                BottomTab::Logs => self.handle_logs_tab_key(key.code),
            },
        }
    }

    fn handle_git_tab_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.git_cursor = self.git_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.git_changes.is_empty() {
                    self.git_cursor = (self.git_cursor + 1).min(self.git_changes.len() - 1);
                }
            }
            KeyCode::Enter | KeyCode::Char('u') => {
                if let Some(item) = self.git_changes.get(self.git_cursor) {
                    let full_local = self.workspace_root.join(&item.rel_path);
                    let remote_path = format!(
                        "{}/{}",
                        self.active_config.remote_path.trim_end_matches('/'),
                        item.rel_path.to_string_lossy().trim_start_matches('/')
                    );

                    match item.op {
                        FileOpKind::Upload => {
                            let _ = self.action_tx.send(AppAction::EnqueueUpload {
                                local: full_local,
                                remote: remote_path,
                                use_temp: self.active_config.use_temp_file.unwrap_or(false),
                            });
                        }
                        FileOpKind::Delete => {
                            let _ = self.action_tx.send(AppAction::EnqueueDelete {
                                remote: remote_path,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_local_tab_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.local_cursor = self.local_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.local_entries.is_empty() {
                    self.local_cursor = (self.local_cursor + 1).min(self.local_entries.len() - 1);
                }
            }
            KeyCode::Enter => {
                if let Some(entry) = self.local_entries.get(self.local_cursor) {
                    if entry.is_dir {
                        self.local_current_dir = entry.path.clone();
                        self.local_cursor = 0;
                        self.refresh_local_files();
                    } else {
                        // Upload file
                        self.upload_selected_local();
                    }
                }
            }
            KeyCode::Backspace | KeyCode::Char('-') => {
                if let Some(parent) = self.local_current_dir.parent() {
                    if parent.starts_with(&self.workspace_root) || parent == self.workspace_root {
                        self.local_current_dir = parent.to_path_buf();
                        self.local_cursor = 0;
                        self.refresh_local_files();
                    }
                }
            }
            KeyCode::Char('u') | KeyCode::Char('s') => {
                self.upload_selected_local();
            }
            _ => {}
        }
    }

    fn upload_selected_local(&mut self) {
        if self.abort_if_conn_error() {
            return;
        }
        if let Some(entry) = self.local_entries.get(self.local_cursor) {
            let rel = entry.path.strip_prefix(&self.workspace_root).unwrap_or(&entry.path);
            let remote_path = format!(
                "{}/{}",
                self.active_config.remote_path.trim_end_matches('/'),
                rel.to_string_lossy().trim_start_matches('/')
            );

            if entry.is_dir {
                let name = entry.name.clone();
                let _ = self.action_tx.send(AppAction::SyncLocalDir {
                    local_dir: entry.path.clone(),
                    remote_dir: remote_path,
                });
                self.add_log("INFO", format!("Scanning local folder {} for batch upload...", name));
            } else {
                let _ = self.action_tx.send(AppAction::EnqueueUpload {
                    local: entry.path.clone(),
                    remote: remote_path,
                    use_temp: self.active_config.use_temp_file.unwrap_or(false),
                });
            }
        }
    }

    fn handle_remote_tab_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.remote_cursor = self.remote_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.remote_entries.is_empty() {
                    self.remote_cursor = (self.remote_cursor + 1).min(self.remote_entries.len() - 1);
                }
            }
            KeyCode::Enter => {
                if let Some(entry) = self.remote_entries.get(self.remote_cursor) {
                    if entry.is_dir {
                        self.remote_current_dir = entry.path.clone();
                        self.remote_cursor = 0;
                        let _ = self.action_tx.send(AppAction::LoadRemoteDir(self.remote_current_dir.clone()));
                    } else {
                        self.download_selected_remote();
                    }
                }
            }
            KeyCode::Backspace | KeyCode::Char('-') => {
                let p = Path::new(&self.remote_current_dir);
                if let Some(parent) = p.parent() {
                    let parent_str = parent.to_string_lossy().to_string();
                    let target = if parent_str.is_empty() { "/".to_string() } else { parent_str };
                    self.remote_current_dir = target.clone();
                    self.remote_cursor = 0;
                    let _ = self.action_tx.send(AppAction::LoadRemoteDir(target));
                }
            }
            KeyCode::Char('d') | KeyCode::Char('s') => {
                self.download_selected_remote();
            }
            KeyCode::Char('m') => {
                if !self.abort_if_conn_error() {
                    if let Some(entry) = self.remote_entries.get(self.remote_cursor) {
                        let default_perm = entry
                            .permissions
                            .map(|p| format!("{:o}", p & 0o777))
                            .unwrap_or_else(|| if entry.is_dir { "755".to_string() } else { "644".to_string() });
                        self.modal = Some(ModalState::ChmodPrompt {
                            remote_path: entry.path.clone(),
                            name: entry.name.clone(),
                            buffer: default_perm,
                        });
                    }
                }
            }
            _ => {}
        }
    }

    fn download_selected_remote(&mut self) {
        if self.abort_if_conn_error() {
            return;
        }
        if let Some(entry) = self.remote_entries.get(self.remote_cursor) {
            let rel = entry.path.trim_start_matches(&self.active_config.remote_path);
            let local_path = self.workspace_root.join(rel.trim_start_matches('/'));

            if entry.is_dir {
                let name = entry.name.clone();
                let _ = self.action_tx.send(AppAction::SyncRemoteDir {
                    remote_dir: entry.path.clone(),
                    local_dir: local_path,
                });
                self.add_log("INFO", format!("Scanning remote folder {} for batch download...", name));
            } else {
                let _ = self.action_tx.send(AppAction::EnqueueDownload {
                    remote: entry.path.clone(),
                    local: local_path,
                });
            }
        }
    }

    fn handle_queue_tab_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.queue_cursor = self.queue_cursor.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.queue_jobs.is_empty() {
                    self.queue_cursor = (self.queue_cursor + 1).min(self.queue_jobs.len() - 1);
                }
            }
            _ => {}
        }
    }

    fn handle_logs_tab_key(&mut self, code: KeyCode) {
        match code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.logs_scroll = self.logs_scroll.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.logs.is_empty() {
                    self.logs_scroll = (self.logs_scroll + 1).min(self.logs.len().saturating_sub(1));
                }
            }
            _ => {}
        }
    }

    pub fn switch_profile(&mut self, name: String) {
        if let Some(cfg) = self.config_store.get_profile(Some(&name)) {
            self.active_profile_name = name.clone();
            self.active_config = cfg.clone();
            self.remote_current_dir = cfg.remote_path.clone();
            self.add_log("INFO", format!("Switched to profile '{}'", name));

            let _ = self.action_tx.send(AppAction::ConnectProfile(name));
        }
    }

    pub fn handle_file_changed(&mut self, _rel_path: PathBuf) {
        let _ = self.action_tx.send(AppAction::RefreshGitStatus);
    }
}
