use chrono::{DateTime, Local, Utc};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Gauge, List, ListItem, ListState, Padding, Paragraph, Row,
    Table, TableState, Wrap,
};

use crate::app::{ActivePane, App, BottomTab, ChmodTarget, ConnectionStatus, LeftTab, ModalState};
use crate::git_sync::GitBadge;
use crate::transfer::conveyer::JobStatus;

mod theme {
    use ratatui::style::Color;

    pub const BORDER_ACTIVE: Color = Color::Rgb(122, 162, 247);
    pub const BORDER_INACTIVE: Color = Color::Rgb(65, 72, 104);
    pub const ACCENT: Color = Color::Rgb(125, 207, 255);
    pub const ACCENT_WARM: Color = Color::Rgb(224, 175, 104);
    pub const TEXT_PRIMARY: Color = Color::Rgb(192, 202, 245);
    pub const TEXT_MUTED: Color = Color::Rgb(120, 128, 160);
    pub const TEXT_DIM: Color = Color::Rgb(86, 95, 137);
    pub const BG_SELECTED: Color = Color::Rgb(40, 52, 87);
    pub const BG_TAB_ACTIVE: Color = Color::Rgb(41, 46, 66);
    pub const BG_GAUGE: Color = Color::Rgb(30, 35, 52);
    pub const SUCCESS: Color = Color::Rgb(158, 206, 106);
    pub const WARNING: Color = Color::Rgb(224, 175, 104);
    pub const ERROR: Color = Color::Rgb(247, 118, 142);
    pub const INFO: Color = Color::Rgb(125, 207, 255);
    pub const DIR_LOCAL: Color = Color::Rgb(125, 207, 255);
    pub const DIR_REMOTE: Color = Color::Rgb(122, 162, 247);
}

pub fn draw(frame: &mut Frame, app: &App) {
    let size = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),      // Top status bar
            Constraint::Percentage(60), // Main explorers (Left & Right)
            Constraint::Min(8),         // Bottom pane (Queue & Logs)
            Constraint::Length(1),      // Contextual key-hint footer
        ])
        .split(size);

    draw_header(frame, app, chunks[0]);
    draw_main_split(frame, app, chunks[1]);
    draw_bottom_split(frame, app, chunks[2]);
    draw_footer(frame, app, chunks[3]);

    if let Some(ref modal) = app.modal {
        draw_modal(frame, app, modal, size);
    }
}

// -----------------------------------------------------------------------------
// Top Status Bar & Contextual Footer
// -----------------------------------------------------------------------------

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let (status_icon, status_text, status_color) = match &app.connection_status {
        ConnectionStatus::Connected => ("●", "Connected", theme::SUCCESS),
        ConnectionStatus::Connecting => ("◐", "Connecting…", theme::WARNING),
        ConnectionStatus::Disconnected => ("○", "Disconnected", theme::TEXT_MUTED),
        ConnectionStatus::Error(_) => ("✕", "Conn Error", theme::ERROR),
    };

    let endpoint = format!(
        "{}://{}@{}:{}",
        app.active_config.protocol.to_lowercase(),
        app.active_config.username,
        app.active_config.host,
        app.active_config.port
    );

    let mut spans = vec![
        Span::styled(
            " ⚡ VSFTUI ",
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(theme::TEXT_DIM)),
        Span::styled("Profile: ", Style::default().fg(theme::TEXT_MUTED)),
        Span::styled(
            app.active_profile_name.as_str(),
            Style::default()
                .fg(theme::ACCENT_WARM)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(theme::TEXT_DIM)),
        Span::styled(endpoint, Style::default().fg(theme::TEXT_PRIMARY)),
        Span::styled(" │ ", Style::default().fg(theme::TEXT_DIM)),
        Span::styled(
            format!("{} {}", status_icon, status_text),
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    if let ConnectionStatus::Error(err) = &app.connection_status {
        let short_err: String = err.chars().take(40).collect();
        spans.push(Span::styled(
            format!(" ({})", short_err),
            Style::default().fg(theme::ERROR),
        ));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme::BORDER_INACTIVE));

    let paragraph = Paragraph::new(Line::from(spans)).block(block);
    frame.render_widget(paragraph, area);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let mut hints: Vec<(&str, &str)> = vec![("Tab", "Pane"), ("j/k", "Move")];

    match app.active_pane {
        ActivePane::Left => match app.left_tab {
            LeftTab::GitUncommitted => {
                hints.extend([("1/2", "Tab"), ("Enter/u", "Upload"), ("G", "Batch Git")]);
            }
            LeftTab::LocalExplorer => {
                hints.extend([
                    ("1/2", "Tab"),
                    ("Enter", "Open/Upload"),
                    ("Bksp", "Up"),
                    ("u/s", "Upload"),
                    ("m", "Chmod"),
                ]);
            }
        },
        ActivePane::Right => {
            hints.extend([
                ("Enter", "Open/Download"),
                ("Bksp", "Up"),
                ("d/s", "Download"),
                ("m", "Chmod"),
            ]);
        }
        ActivePane::Bottom => match app.bottom_tab {
            BottomTab::TransferQueue => {
                hints.extend([("1/2", "Tab"), ("c", "Clear Done"), ("C", "Clear All")]);
            }
            BottomTab::Logs => {
                hints.extend([("1/2", "Tab"), ("c", "Clear Logs"), ("C", "Clear All")]);
            }
        },
    }

    hints.extend([("r", "Refresh"), ("p", "Profile"), ("?", "Help"), ("q", "Quit")]);

    let mut spans = vec![Span::raw(" ")];
    for (idx, (key, label)) in hints.into_iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled("  ", Style::default()));
        }
        spans.push(Span::styled(
            format!("[{}]", key),
            Style::default()
                .fg(theme::ACCENT_WARM)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {}", label),
            Style::default().fg(theme::TEXT_MUTED),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

// -----------------------------------------------------------------------------
// Pane Helpers
// -----------------------------------------------------------------------------

fn pane_block(title: &str, is_focused: bool) -> Block<'_> {
    let border_color = if is_focused {
        theme::BORDER_ACTIVE
    } else {
        theme::BORDER_INACTIVE
    };
    let title_color = if is_focused {
        theme::ACCENT
    } else {
        theme::TEXT_MUTED
    };
    let marker = if is_focused { "▶ " } else { "" };

    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(format!(" {}{} ", marker, title))
        .title_style(Style::default().fg(title_color).add_modifier(Modifier::BOLD))
}

fn render_tab_bar(
    frame: &mut Frame,
    area: Rect,
    tabs: &[(&str, usize)],
    selected: usize,
    trailing: Option<(&str, Color)>,
) {
    let mut spans = vec![Span::raw(" ")];
    for (i, (label, _)) in tabs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("  ", Style::default()));
        }
        let is_active = i == selected;
        let style = if is_active {
            Style::default()
                .fg(theme::ACCENT_WARM)
                .bg(theme::BG_TAB_ACTIVE)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::TEXT_MUTED)
        };
        spans.push(Span::styled(format!(" {} ", label), style));
    }

    if let Some((text, color)) = trailing {
        spans.push(Span::styled("  │  ", Style::default().fg(theme::TEXT_DIM)));
        spans.push(Span::styled(text, Style::default().fg(color)));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_divider(frame: &mut Frame, area: Rect) {
    let line = "─".repeat(area.width as usize);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().fg(theme::TEXT_DIM)),
        area,
    );
}

// -----------------------------------------------------------------------------
// Main Left & Right Split
// -----------------------------------------------------------------------------

fn draw_main_split(frame: &mut Frame, app: &App, area: Rect) {
    let h_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(area);

    draw_left_pane(frame, app, h_chunks[0]);
    draw_right_pane(frame, app, h_chunks[1]);
}

fn draw_left_pane(frame: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.active_pane == ActivePane::Left;
    let outer = pane_block("LOCAL WORKSPACE", is_focused);
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    if inner.height < 3 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Inline tabs + path
            Constraint::Length(1), // Divider
            Constraint::Min(0),    // Content
        ])
        .split(inner);

    let git_label = format!("[1] Git ({})", app.git_changes.len());
    let selected_tab = match app.left_tab {
        LeftTab::GitUncommitted => 0,
        LeftTab::LocalExplorer => 1,
    };

    let rel_cwd = app
        .local_current_dir
        .strip_prefix(&app.workspace_root)
        .unwrap_or(&app.local_current_dir);
    let path_str = if rel_cwd.as_os_str().is_empty() {
        "./".to_string()
    } else {
        format!("./{}", rel_cwd.to_string_lossy())
    };
    let trailing = match app.left_tab {
        LeftTab::LocalExplorer => Some((path_str.as_str(), theme::ACCENT)),
        LeftTab::GitUncommitted => None,
    };

    render_tab_bar(
        frame,
        chunks[0],
        &[(git_label.as_str(), 0), ("[2] Local Files", 1)],
        selected_tab,
        trailing,
    );
    render_divider(frame, chunks[1]);

    match app.left_tab {
        LeftTab::GitUncommitted => draw_git_uncommitted_list(frame, app, chunks[2], is_focused),
        LeftTab::LocalExplorer => draw_local_explorer_table(frame, app, chunks[2], is_focused),
    }
}

fn draw_git_uncommitted_list(frame: &mut Frame, app: &App, area: Rect, is_focused: bool) {
    if app.git_changes.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  ✔ Working tree clean",
                Style::default()
                    .fg(theme::SUCCESS)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "  No uncommitted Git changes. Press [2] for Local Files.",
                Style::default().fg(theme::TEXT_MUTED),
            )),
        ]);
        frame.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = app
        .git_changes
        .iter()
        .map(|item| {
            let (badge_str, badge_color) = match item.badge {
                GitBadge::Modified => (" M  ", theme::WARNING),
                GitBadge::Added => (" A  ", theme::SUCCESS),
                GitBadge::Untracked => (" ?? ", theme::INFO),
                GitBadge::Deleted => (" D  ", theme::ERROR),
                GitBadge::Renamed => (" R  ", Color::Magenta),
            };

            let staged_marker = if item.is_staged {
                Span::styled("STAGED   ", Style::default().fg(theme::SUCCESS))
            } else {
                Span::styled("UNSTAGED ", Style::default().fg(theme::TEXT_MUTED))
            };

            let line = Line::from(vec![
                Span::styled(
                    badge_str,
                    Style::default()
                        .fg(badge_color)
                        .add_modifier(Modifier::BOLD),
                ),
                staged_marker,
                Span::styled(
                    item.rel_path.to_string_lossy().to_string(),
                    Style::default().fg(theme::TEXT_PRIMARY),
                ),
            ]);

            ListItem::new(line)
        })
        .collect();

    let highlight_style = if is_focused {
        Style::default()
            .bg(theme::BG_SELECTED)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(theme::BG_TAB_ACTIVE)
    };

    let list = List::new(items)
        .highlight_style(highlight_style)
        .highlight_symbol("▶ ");
    let mut state =
        ListState::default().with_selected((!app.git_changes.is_empty()).then_some(app.git_cursor));
    frame.render_stateful_widget(list, area, &mut state);
}

struct FileRowView<'a> {
    name: &'a str,
    is_dir: bool,
    size: u64,
    permissions: Option<u32>,
    modified: Option<DateTime<Utc>>,
}

fn render_file_table(
    frame: &mut Frame,
    area: Rect,
    entries: &[FileRowView<'_>],
    cursor: usize,
    is_focused: bool,
    dir_color: Color,
    empty_lines: Vec<Line<'static>>,
) {
    if entries.is_empty() {
        frame.render_widget(Paragraph::new(empty_lines), area);
        return;
    }

    let rows: Vec<Row> = entries
        .iter()
        .map(|entry| {
            let icon = if entry.is_dir { "📁 " } else { "📄 " };
            let size_str = if entry.is_dir {
                "-".to_string()
            } else {
                format_size(entry.size)
            };
            let perm_str = entry
                .permissions
                .map(|p| format!("{:03o}", p & 0o777))
                .unwrap_or_else(|| "-".to_string());
            let mtime_str = format_modified(entry.modified);

            let row_style = if entry.is_dir {
                Style::default().fg(dir_color)
            } else {
                Style::default().fg(theme::TEXT_PRIMARY)
            };

            Row::new(vec![
                format!("{}{}", icon, entry.name),
                perm_str,
                size_str,
                mtime_str,
            ])
            .style(row_style)
        })
        .collect();

    let highlight_style = if is_focused {
        Style::default()
            .bg(theme::BG_SELECTED)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().bg(theme::BG_TAB_ACTIVE)
    };

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(50),
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Length(17),
        ],
    )
    .header(
        Row::new(vec!["Name", "Perm", "Size", "Modified"])
            .style(
                Style::default()
                    .fg(theme::ACCENT_WARM)
                    .add_modifier(Modifier::BOLD),
            )
            .bottom_margin(1),
    )
    .row_highlight_style(highlight_style)
    .highlight_symbol("▶ ");

    let mut state = TableState::default().with_selected(Some(cursor));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_local_explorer_table(frame: &mut Frame, app: &App, area: Rect, is_focused: bool) {
    let views: Vec<FileRowView<'_>> = app
        .local_entries
        .iter()
        .map(|e| FileRowView {
            name: &e.name,
            is_dir: e.is_dir,
            size: e.size,
            permissions: e.permissions,
            modified: e.modified,
        })
        .collect();

    let empty = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Directory is empty.",
            Style::default().fg(theme::TEXT_MUTED),
        )),
    ];

    render_file_table(
        frame,
        area,
        &views,
        app.local_cursor,
        is_focused,
        theme::DIR_LOCAL,
        empty,
    );
}

// -----------------------------------------------------------------------------
// Right Pane: Remote Explorer
// -----------------------------------------------------------------------------

fn draw_right_pane(frame: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.active_pane == ActivePane::Right;
    let outer = pane_block("REMOTE EXPLORER", is_focused);
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    if inner.height < 3 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Path bar
            Constraint::Length(1), // Divider
            Constraint::Min(0),    // File table
        ])
        .split(inner);

    let path_line = Line::from(vec![
        Span::styled(" Path: ", Style::default().fg(theme::TEXT_MUTED)),
        Span::styled(
            app.remote_current_dir.as_str(),
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(path_line), chunks[0]);
    render_divider(frame, chunks[1]);

    let empty = match &app.connection_status {
        ConnectionStatus::Connecting => vec![
            Line::from(""),
            Line::from(Span::styled(
                "  ◐ Connecting to remote server…",
                Style::default().fg(theme::WARNING),
            )),
        ],
        ConnectionStatus::Error(err) => vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  ✕ Connection error: {}", err),
                Style::default().fg(theme::ERROR),
            )),
            Line::from(Span::styled(
                "  Press [r] to retry or [p] to switch profile.",
                Style::default().fg(theme::TEXT_MUTED),
            )),
        ],
        ConnectionStatus::Disconnected => vec![
            Line::from(""),
            Line::from(Span::styled(
                "  ○ Not connected. Press [r] to connect or [p] to pick a profile.",
                Style::default().fg(theme::TEXT_MUTED),
            )),
        ],
        ConnectionStatus::Connected => vec![
            Line::from(""),
            Line::from(Span::styled(
                "  Remote directory is empty.",
                Style::default().fg(theme::TEXT_MUTED),
            )),
        ],
    };

    let views: Vec<FileRowView<'_>> = app
        .remote_entries
        .iter()
        .map(|e| FileRowView {
            name: &e.name,
            is_dir: e.is_dir,
            size: e.size,
            permissions: e.permissions,
            modified: e.modified,
        })
        .collect();

    render_file_table(
        frame,
        chunks[2],
        &views,
        app.remote_cursor,
        is_focused,
        theme::DIR_REMOTE,
        empty,
    );
}

fn format_modified(modified: Option<DateTime<Utc>>) -> String {
    modified
        .map(|t| t.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "-".to_string())
}

// -----------------------------------------------------------------------------
// Bottom Pane: Transfer Queue & Structured Logs
// -----------------------------------------------------------------------------

fn draw_bottom_split(frame: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.active_pane == ActivePane::Bottom;
    let outer = pane_block("CONVEYER & ACTIVITY", is_focused);
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    if inner.height < 3 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Inline tabs
            Constraint::Length(1), // Divider
            Constraint::Min(0),    // Content
        ])
        .split(inner);

    let active_transfers = app
        .queue_jobs
        .iter()
        .filter(|j| matches!(j.status, JobStatus::Running | JobStatus::Retrying(_)))
        .count();

    let queue_label = format!("[1] Queue ({}/{})", active_transfers, app.queue_jobs.len());
    let logs_label = format!("[2] Logs ({})", app.logs.len());

    let selected_tab = match app.bottom_tab {
        BottomTab::TransferQueue => 0,
        BottomTab::Logs => 1,
    };

    render_tab_bar(
        frame,
        chunks[0],
        &[(queue_label.as_str(), 0), (logs_label.as_str(), 1)],
        selected_tab,
        None,
    );
    render_divider(frame, chunks[1]);

    match app.bottom_tab {
        BottomTab::TransferQueue => draw_queue_pane(frame, app, chunks[2], is_focused),
        BottomTab::Logs => draw_logs_pane(frame, app, chunks[2]),
    }
}

fn draw_queue_pane(frame: &mut Frame, app: &App, area: Rect, is_focused: bool) {
    if app.queue_jobs.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  Transfer queue is idle.",
                Style::default().fg(theme::TEXT_MUTED),
            )),
            Line::from(Span::styled(
                "  Select a file and press [u] to upload, [d] to download, or [G] to sync Git changes.",
                Style::default().fg(theme::TEXT_DIM),
            )),
        ]);
        frame.render_widget(msg, area);
        return;
    }

    let running_job = app
        .queue_jobs
        .iter()
        .find(|j| matches!(j.status, JobStatus::Running));

    let layout = if running_job.is_some() {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(2), Constraint::Min(0)])
            .split(area)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(0), Constraint::Min(0)])
            .split(area)
    };

    if let Some(active) = running_job {
        let percent = if active.size > 0 {
            ((active.transferred as f64 / active.size as f64) * 100.0) as u16
        } else {
            0
        };
        let gauge_label = format!(
            " {} #{}: {} ({}/{}) {}% ",
            active.kind.as_str(),
            active.id,
            active.remote_path,
            format_size(active.transferred),
            format_size(active.size),
            percent.min(100)
        );
        let gauge = Gauge::default()
            .block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .border_style(Style::default().fg(theme::TEXT_DIM)),
            )
            .gauge_style(
                Style::default()
                    .fg(theme::ACCENT)
                    .bg(theme::BG_GAUGE)
                    .add_modifier(Modifier::BOLD),
            )
            .use_unicode(true)
            .percent(percent.min(100))
            .label(gauge_label);
        frame.render_widget(gauge, layout[0]);
    }

    let rows: Vec<Row> = app
        .queue_jobs
        .iter()
        .rev()
        .map(|job| {
            let (status_str, status_style) = match &job.status {
                JobStatus::Pending => ("⏳ Pending", Style::default().fg(theme::TEXT_MUTED)),
                JobStatus::Running => (
                    "▶ Running",
                    Style::default()
                        .fg(theme::WARNING)
                        .add_modifier(Modifier::BOLD),
                ),
                JobStatus::Success => ("✔ Done", Style::default().fg(theme::SUCCESS)),
                JobStatus::Failed(_) => ("✕ Failed", Style::default().fg(theme::ERROR)),
                JobStatus::Retrying(_) => ("↻ Retry", Style::default().fg(theme::WARNING)),
            };

            let speed_str = if job.speed_bytes_per_sec > 0.0 {
                format!("{}/s", format_size(job.speed_bytes_per_sec as u64))
            } else {
                "-".to_string()
            };

            Row::new(vec![
                format!("#{}", job.id),
                job.kind.as_str().to_string(),
                job.remote_path.clone(),
                format_size(job.size),
                status_str.to_string(),
                speed_str,
            ])
            .style(status_style)
        })
        .collect();

    let highlight_style = if is_focused {
        Style::default().bg(theme::BG_SELECTED)
    } else {
        Style::default()
    };

    let table = Table::new(
        rows,
        [
            Constraint::Length(6),
            Constraint::Length(9),
            Constraint::Percentage(48),
            Constraint::Length(10),
            Constraint::Length(11),
            Constraint::Length(12),
        ],
    )
    .header(
        Row::new(vec!["ID", "Action", "Remote Target", "Size", "Status", "Speed"])
            .style(
                Style::default()
                    .fg(theme::ACCENT_WARM)
                    .add_modifier(Modifier::BOLD),
            ),
    )
    .row_highlight_style(highlight_style);

    let mut state = TableState::default().with_selected(Some(app.queue_cursor));
    frame.render_stateful_widget(table, layout[1], &mut state);
}

fn draw_logs_pane(frame: &mut Frame, app: &App, area: Rect) {
    if app.logs.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  No logs yet. Press [c] to clear logs, [C] to clear queue & logs.",
                Style::default().fg(theme::TEXT_MUTED),
            )),
        ]);
        frame.render_widget(msg, area);
        return;
    }

    let visible_height = area.height as usize;
    let items: Vec<ListItem> = app
        .logs
        .iter()
        .rev()
        .skip(app.logs_scroll)
        .take(visible_height.max(1))
        .map(|log| {
            let (lvl_str, lvl_color) = match log.level.as_str() {
                "ERROR" => ("ERR", theme::ERROR),
                "WARN" => ("WRN", theme::WARNING),
                "INFO" => ("INF", theme::INFO),
                _ => ("DBG", theme::TEXT_MUTED),
            };

            let line = Line::from(vec![
                Span::styled(
                    log.timestamp.format(" %H:%M:%S ").to_string(),
                    Style::default().fg(theme::TEXT_DIM),
                ),
                Span::styled(
                    format!("[{}] ", lvl_str),
                    Style::default()
                        .fg(lvl_color)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(&log.message, Style::default().fg(theme::TEXT_PRIMARY)),
            ]);

            ListItem::new(line)
        })
        .collect();

    frame.render_widget(List::new(items), area);
}

// -----------------------------------------------------------------------------
// Modals: Profile Switcher, Password Prompt, Chmod Prompt, Help
// -----------------------------------------------------------------------------

fn draw_modal(frame: &mut Frame, app: &App, modal: &ModalState, screen_size: Rect) {
    match modal {
        ModalState::Help => {
            let area = centered_rect_clamped(64, 60, 72, 24, screen_size);
            frame.render_widget(Clear, area);

            let key_row = |key: &'static str, desc: &'static str| {
                Line::from(vec![
                    Span::styled(
                        format!("  {:<15}", key),
                        Style::default()
                            .fg(theme::ACCENT)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(desc, Style::default().fg(theme::TEXT_PRIMARY)),
                ])
            };
            let section = |title: &'static str| {
                Line::from(Span::styled(
                    format!(" {}", title),
                    Style::default()
                        .fg(theme::ACCENT_WARM)
                        .add_modifier(Modifier::BOLD),
                ))
            };

            let help_text = vec![
                section("NAVIGATION"),
                key_row("Tab / S-Tab", "Cycle active pane (Local / Remote / Bottom)"),
                key_row("1 / 2", "Switch tabs in active pane"),
                key_row("j / k / ↑ / ↓", "Move cursor or scroll"),
                key_row("Enter", "Open folder, or upload/download selected file"),
                key_row("Bksp / -", "Go to parent directory"),
                Line::from(""),
                section("TRANSFERS & SYNC"),
                key_row("u / d", "Upload (local) or download (remote) selected item"),
                key_row("s", "Sync selected local or remote folder"),
                key_row("G", "Batch upload all uncommitted Git changes"),
                key_row("U / D", "Sync entire workspace (U: up, D: down)"),
                key_row("m", "Change permissions (chmod) on selected item"),
                Line::from(""),
                section("GENERAL"),
                key_row("p", "Switch sftp.json profile"),
                key_row("r", "Refresh files, Git status, and remote directory"),
                key_row("c / C", "Clear completed queue/logs (C clears both)"),
                key_row("? / Ctrl+h", "Toggle this Help window"),
                key_row("q / Ctrl+c", "Quit VSFTUI"),
                Line::from(""),
                Line::from(Span::styled(
                    "  Press [Esc], [?], or [q] to close.",
                    Style::default().fg(theme::TEXT_MUTED),
                )),
            ];

            let block = Block::default()
                .title(" ⚡ HELP & KEYBINDINGS ")
                .title_style(
                    Style::default()
                        .fg(theme::ACCENT_WARM)
                        .add_modifier(Modifier::BOLD),
                )
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::BORDER_ACTIVE))
                .padding(Padding::uniform(1));

            let p = Paragraph::new(help_text)
                .block(block)
                .wrap(Wrap { trim: false });
            frame.render_widget(p, area);
        }

        ModalState::ProfileSwitcher { selected } => {
            let area = centered_rect_clamped(54, 48, 45, 12, screen_size);
            frame.render_widget(Clear, area);

            let profiles = app.config_store.profile_names();
            let items: Vec<ListItem> = profiles
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let is_active = name == &app.active_profile_name;
                    let is_cursor = i == *selected;

                    let host_info = app
                        .config_store
                        .get_profile(Some(name))
                        .map(|c| format!(" ({}://{}:{})", c.protocol, c.host, c.port))
                        .unwrap_or_default();

                    let prefix = if is_cursor { " ▶ " } else { "   " };
                    let active_tag = if is_active { "  ● ACTIVE" } else { "" };

                    let style = if is_cursor {
                        Style::default()
                            .fg(Color::White)
                            .bg(theme::BG_SELECTED)
                            .add_modifier(Modifier::BOLD)
                    } else if is_active {
                        Style::default().fg(theme::SUCCESS)
                    } else {
                        Style::default().fg(theme::TEXT_PRIMARY)
                    };

                    ListItem::new(Line::from(vec![
                        Span::styled(format!("{}{}", prefix, name), style),
                        Span::styled(host_info, Style::default().fg(theme::TEXT_MUTED)),
                        Span::styled(
                            active_tag,
                            Style::default()
                                .fg(theme::SUCCESS)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]))
                })
                .collect();

            let block = Block::default()
                .title(" SWITCH PROFILE  [Enter] Apply  [Esc] Cancel ")
                .title_style(
                    Style::default()
                        .fg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD),
                )
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::BORDER_ACTIVE))
                .padding(Padding::uniform(1));

            frame.render_widget(List::new(items).block(block), area);
        }

        ModalState::PasswordPrompt { buffer } => {
            let area = centered_rect_clamped(50, 44, 24, 9, screen_size);
            frame.render_widget(Clear, area);

            let masked: String = "•".repeat(buffer.len());
            let text = vec![
                Line::from(Span::styled(
                    "Enter SSH / SFTP Password:",
                    Style::default().fg(theme::TEXT_PRIMARY),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    format!("  {}▏ ", masked),
                    Style::default()
                        .fg(theme::ACCENT_WARM)
                        .bg(theme::BG_TAB_ACTIVE)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "[Enter] Confirm   [Esc] Cancel",
                    Style::default().fg(theme::TEXT_MUTED),
                )),
            ];

            let block = Block::default()
                .title(" AUTHENTICATION REQUIRED ")
                .title_style(
                    Style::default()
                        .fg(theme::ERROR)
                        .add_modifier(Modifier::BOLD),
                )
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::ERROR))
                .padding(Padding::uniform(1));

            let p = Paragraph::new(text)
                .block(block)
                .alignment(Alignment::Center);
            frame.render_widget(p, area);
        }

        ModalState::ChmodPrompt {
            target,
            name,
            buffer,
        } => {
            let area = centered_rect_clamped(50, 44, 26, 9, screen_size);
            frame.render_widget(Clear, area);

            let text = vec![
                Line::from(vec![
                    Span::styled("Octal mode for ", Style::default().fg(theme::TEXT_PRIMARY)),
                    Span::styled(
                        name.as_str(),
                        Style::default()
                            .fg(theme::ACCENT)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    format!("  {}▏ ", buffer),
                    Style::default()
                        .fg(theme::ACCENT_WARM)
                        .bg(theme::BG_TAB_ACTIVE)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "[Enter] Apply   [Esc] Cancel",
                    Style::default().fg(theme::TEXT_MUTED),
                )),
            ];

            let title = match target {
                ChmodTarget::Local(_) => " LOCAL PERMISSIONS (CHMOD) ",
                ChmodTarget::Remote(_) => " REMOTE PERMISSIONS (CHMOD) ",
            };

            let block = Block::default()
                .title(title)
                .title_style(
                    Style::default()
                        .fg(theme::ACCENT)
                        .add_modifier(Modifier::BOLD),
                )
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(theme::BORDER_ACTIVE))
                .padding(Padding::uniform(1));

            let p = Paragraph::new(text)
                .block(block)
                .alignment(Alignment::Center);
            frame.render_widget(p, area);
        }
    }
}

fn centered_rect_clamped(
    percent_x: u16,
    min_w: u16,
    percent_y: u16,
    min_h: u16,
    r: Rect,
) -> Rect {
    let width = ((r.width as u32 * percent_x as u32) / 100)
        .max(min_w as u32)
        .min(r.width as u32) as u16;
    let height = ((r.height as u32 * percent_y as u32) / 100)
        .max(min_h as u32)
        .min(r.height as u32) as u16;

    let x = r.x + r.width.saturating_sub(width) / 2;
    let y = r.y + r.height.saturating_sub(height) / 2;

    Rect::new(x, y, width, height)
}

fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
}
