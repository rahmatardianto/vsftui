use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Gauge, List, ListItem, ListState, Paragraph, Row, Table,
    TableState, Tabs, Wrap,
};

use crate::app::{ActivePane, App, BottomTab, ConnectionStatus, LeftTab, ModalState};
use crate::git_sync::GitBadge;
use crate::transfer::conveyer::JobStatus;

pub fn draw(frame: &mut Frame, app: &App) {
    let size = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),      // Top header bar
            Constraint::Percentage(62), // Main explorers (Left & Right)
            Constraint::Percentage(38), // Bottom pane (Queue & Logs)
        ])
        .split(size);

    draw_header(frame, app, chunks[0]);
    draw_main_split(frame, app, chunks[1]);
    draw_bottom_split(frame, app, chunks[2]);

    if let Some(ref modal) = app.modal {
        draw_modal(frame, app, modal, size);
    }
}

// -----------------------------------------------------------------------------
// Top Header Bar
// -----------------------------------------------------------------------------

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let (status_text, status_color) = match &app.connection_status {
        ConnectionStatus::Connected => ("CONNECTED", Color::Green),
        ConnectionStatus::Connecting => ("CONNECTING...", Color::Yellow),
        ConnectionStatus::Disconnected => ("DISCONNECTED", Color::Gray),
        ConnectionStatus::Error(_) => ("CONN ERROR", Color::Red),
    };

    let protocol_badge = Span::styled(
        format!(" [{}] ", app.active_config.protocol.to_uppercase()),
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    );

    let profile_badge = Span::styled(
        format!(" Profile: {} ", app.active_profile_name),
        Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
    );

    let status_badge = Span::styled(
        format!(" Status: {} ", status_text),
        Style::default().fg(status_color).add_modifier(Modifier::BOLD),
    );

    let header_line = Line::from(vec![
        Span::styled("⚡ SFTUI ", Style::default().fg(Color::White).add_modifier(Modifier::BOLD)),
        profile_badge,
        protocol_badge,
        status_badge,
        Span::raw(" | "),
        Span::styled("[Tab] Pane  [u] Upload  [d] Download  [s] Sync Dir  [G] Batch Git  [p] Profile  [?] Help  [q] Quit", Style::default().fg(Color::DarkGray)),
    ]);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Blue));

    let paragraph = Paragraph::new(header_line).block(block);
    frame.render_widget(paragraph, area);
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
    let border_color = if is_focused { Color::Green } else { Color::DarkGray };

    let tab_titles = vec![
        format!("[1] Git Uncommitted ({})", app.git_changes.len()),
        "[2] Local Files".to_string(),
    ];
    let selected_tab = match app.left_tab {
        LeftTab::GitUncommitted => 0,
        LeftTab::LocalExplorer => 1,
    };

    let inner_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    let tabs = Tabs::new(tab_titles)
        .block(
            Block::default()
                .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
                .border_style(Style::default().fg(border_color))
                .title(if is_focused { " ▶ LOCAL WORKSPACE " } else { " LOCAL WORKSPACE " })
                .title_style(Style::default().fg(if is_focused { Color::Green } else { Color::White }).add_modifier(Modifier::BOLD)),
        )
        .select(selected_tab)
        .style(Style::default().fg(Color::Gray))
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));

    frame.render_widget(tabs, inner_layout[0]);

    match app.left_tab {
        LeftTab::GitUncommitted => draw_git_uncommitted_list(frame, app, inner_layout[1], border_color),
        LeftTab::LocalExplorer => draw_local_explorer_table(frame, app, inner_layout[1], border_color),
    }
}

fn draw_git_uncommitted_list(frame: &mut Frame, app: &App, area: Rect, border_color: Color) {
    let block = Block::default()
        .borders(Borders::BOTTOM | Borders::LEFT | Borders::RIGHT)
        .border_style(Style::default().fg(border_color));

    if app.git_changes.is_empty() {
        let msg = Paragraph::new("\n  ✔ Working tree clean. No uncommitted Git changes.")
            .style(Style::default().fg(Color::Green))
            .block(block);
        frame.render_widget(msg, area);
        return;
    }

    let items: Vec<ListItem> = app
        .git_changes
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let is_selected = i == app.git_cursor && app.active_pane == ActivePane::Left;
            let (badge_str, badge_style) = match item.badge {
                GitBadge::Modified => (" M  ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                GitBadge::Added => (" A  ", Style::default().fg(Color::Green).add_modifier(Modifier::BOLD)),
                GitBadge::Untracked => (" ?? ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
                GitBadge::Deleted => (" D  ", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                GitBadge::Renamed => (" R  ", Style::default().fg(Color::Magenta).add_modifier(Modifier::BOLD)),
            };

            let staged_marker = if item.is_staged {
                Span::styled("[STAGED] ", Style::default().fg(Color::Green))
            } else {
                Span::styled("[UNSTAGED] ", Style::default().fg(Color::DarkGray))
            };

            let line = Line::from(vec![
                Span::styled(badge_str, badge_style),
                staged_marker,
                Span::styled(
                    item.rel_path.to_string_lossy().to_string(),
                    if is_selected {
                        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::White)
                    },
                ),
            ]);

            if is_selected {
                ListItem::new(line).style(Style::default().bg(Color::Rgb(30, 50, 70)))
            } else {
                ListItem::new(line)
            }
        })
        .collect();

    let list = List::new(items).block(block);
    let mut state = ListState::default().with_selected((!app.git_changes.is_empty()).then_some(app.git_cursor));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_local_explorer_table(frame: &mut Frame, app: &App, area: Rect, border_color: Color) {
    let rel_cwd = app.local_current_dir.strip_prefix(&app.workspace_root).unwrap_or(&app.local_current_dir);
    let title = format!(" Path: ./{} ", rel_cwd.to_string_lossy());

    let block = Block::default()
        .borders(Borders::BOTTOM | Borders::LEFT | Borders::RIGHT)
        .border_style(Style::default().fg(border_color))
        .title(title);

    let rows: Vec<Row> = app
        .local_entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let is_selected = i == app.local_cursor && app.active_pane == ActivePane::Left;
            let icon = if entry.is_dir { "📁 " } else { "📄 " };
            let size_str = if entry.is_dir {
                "-".to_string()
            } else {
                format_size(entry.size)
            };
            let mtime_str = entry
                .modified
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "-".to_string());

            let row_style = if is_selected {
                Style::default().fg(Color::White).bg(Color::Rgb(30, 50, 70)).add_modifier(Modifier::BOLD)
            } else if entry.is_dir {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::White)
            };

            Row::new(vec![
                format!("{}{}", icon, entry.name),
                size_str,
                mtime_str,
            ])
            .style(row_style)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(60),
            Constraint::Length(12),
            Constraint::Length(18),
        ],
    )
    .header(
        Row::new(vec!["Name", "Size", "Modified"])
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            .bottom_margin(1),
    )
    .block(block);

    let mut state = TableState::default().with_selected((!app.local_entries.is_empty()).then_some(app.local_cursor));
    frame.render_stateful_widget(table, area, &mut state);
}

// -----------------------------------------------------------------------------
// Right Pane: Remote Explorer
// -----------------------------------------------------------------------------

fn draw_right_pane(frame: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.active_pane == ActivePane::Right;
    let border_color = if is_focused { Color::Green } else { Color::DarkGray };

    let title = format!(
        "{} REMOTE EXPLORER: {} ",
        if is_focused { "▶" } else { "" },
        app.remote_current_dir
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(title)
        .title_style(Style::default().fg(if is_focused { Color::Green } else { Color::White }).add_modifier(Modifier::BOLD));

    if app.remote_entries.is_empty() {
        let msg = Paragraph::new("\n  (Directory empty or remote not connected)")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        frame.render_widget(msg, area);
        return;
    }

    let rows: Vec<Row> = app
        .remote_entries
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let is_selected = i == app.remote_cursor && app.active_pane == ActivePane::Right;
            let icon = if entry.is_dir { "📁 " } else { "📄 " };
            let size_str = if entry.is_dir {
                "-".to_string()
            } else {
                format_size(entry.size)
            };
            let perm_str = entry
                .permissions
                .map(|p| format!("{:o}", p & 0o777))
                .unwrap_or_else(|| "-".to_string());
            let mtime_str = entry
                .modified
                .map(|t| t.format("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_else(|| "-".to_string());

            let row_style = if is_selected {
                Style::default().fg(Color::White).bg(Color::Rgb(30, 50, 70)).add_modifier(Modifier::BOLD)
            } else if entry.is_dir {
                Style::default().fg(Color::LightBlue)
            } else {
                Style::default().fg(Color::White)
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

    let table = Table::new(
        rows,
        [
            Constraint::Percentage(50),
            Constraint::Length(8),
            Constraint::Length(12),
            Constraint::Length(18),
        ],
    )
    .header(
        Row::new(vec!["Name", "Perm", "Size", "Modified"])
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
            .bottom_margin(1),
    )
    .block(block);

    let mut state = TableState::default().with_selected((!app.remote_entries.is_empty()).then_some(app.remote_cursor));
    frame.render_stateful_widget(table, area, &mut state);
}

// -----------------------------------------------------------------------------
// Bottom Pane: Transfer Queue & Structured Logs
// -----------------------------------------------------------------------------

fn draw_bottom_split(frame: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.active_pane == ActivePane::Bottom;
    let border_color = if is_focused { Color::Green } else { Color::DarkGray };

    let active_transfers = app
        .queue_jobs
        .iter()
        .filter(|j| matches!(j.status, JobStatus::Running | JobStatus::Retrying(_)))
        .count();

    let tab_titles = vec![
        format!("[1] Transfer Queue ({}/{})", active_transfers, app.queue_jobs.len()),
        format!("[2] Logs ({})", app.logs.len()),
    ];

    let selected_tab = match app.bottom_tab {
        BottomTab::TransferQueue => 0,
        BottomTab::Logs => 1,
    };

    let inner_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    let tabs = Tabs::new(tab_titles)
        .block(
            Block::default()
                .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
                .border_style(Style::default().fg(border_color))
                .title(if is_focused { " ▶ CONVEYER & ACTIVITY " } else { " CONVEYER & ACTIVITY " })
                .title_style(Style::default().fg(if is_focused { Color::Green } else { Color::White }).add_modifier(Modifier::BOLD)),
        )
        .select(selected_tab)
        .style(Style::default().fg(Color::Gray))
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));

    frame.render_widget(tabs, inner_layout[0]);

    match app.bottom_tab {
        BottomTab::TransferQueue => draw_queue_pane(frame, app, inner_layout[1], border_color),
        BottomTab::Logs => draw_logs_pane(frame, app, inner_layout[1], border_color),
    }
}

fn draw_queue_pane(frame: &mut Frame, app: &App, area: Rect, border_color: Color) {
    let block = Block::default()
        .borders(Borders::BOTTOM | Borders::LEFT | Borders::RIGHT)
        .border_style(Style::default().fg(border_color));

    if app.queue_jobs.is_empty() {
        let msg = Paragraph::new("\n  Transfer queue idle. (Press 'u' to upload, 'd' to download, 'g' for Git auto-sync)")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
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
            .constraints([Constraint::Length(3), Constraint::Min(0)])
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
            50
        };
        let gauge_label = format!(
            "Transferring #{}: {} ({}/{})",
            active.id,
            active.remote_path,
            format_size(active.transferred),
            format_size(active.size)
        );
        let gauge = Gauge::default()
            .block(Block::default().borders(Borders::BOTTOM).border_style(Style::default().fg(Color::DarkGray)))
            .gauge_style(Style::default().fg(Color::Cyan).bg(Color::Rgb(20, 30, 40)))
            .percent(percent.min(100))
            .label(gauge_label);
        frame.render_widget(gauge, layout[0]);
    }

    let rows: Vec<Row> = app
        .queue_jobs
        .iter()
        .rev()
        .take(50)
        .map(|job| {
            let (status_str, status_style) = match &job.status {
                JobStatus::Pending => ("PENDING", Style::default().fg(Color::Gray)),
                JobStatus::Running => ("RUNNING", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                JobStatus::Success => ("SUCCESS", Style::default().fg(Color::Green)),
                JobStatus::Failed(_) => ("FAILED", Style::default().fg(Color::Red)),
                JobStatus::Retrying(_) => ("RETRY", Style::default().fg(Color::LightRed)),
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

    let table = Table::new(
        rows,
        [
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Percentage(50),
            Constraint::Length(12),
            Constraint::Length(10),
            Constraint::Length(14),
        ],
    )
    .header(
        Row::new(vec!["ID", "Action", "Remote Target", "Size", "Status", "Speed"])
            .style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
    )
    .block(block);

    frame.render_widget(table, layout[1]);
}

fn draw_logs_pane(frame: &mut Frame, app: &App, area: Rect, border_color: Color) {
    let block = Block::default()
        .borders(Borders::BOTTOM | Borders::LEFT | Borders::RIGHT)
        .border_style(Style::default().fg(border_color));

    let items: Vec<ListItem> = app
        .logs
        .iter()
        .rev()
        .skip(app.logs_scroll)
        .take(30)
        .map(|log| {
            let (lvl_str, lvl_style) = match log.level.as_str() {
                "ERROR" => ("ERROR", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
                "WARN" => ("WARN ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
                "INFO" => ("INFO ", Style::default().fg(Color::Cyan)),
                _ => ("DEBUG", Style::default().fg(Color::DarkGray)),
            };

            let line = Line::from(vec![
                Span::styled(log.timestamp.format("%H:%M:%S ").to_string(), Style::default().fg(Color::DarkGray)),
                Span::styled(format!("[{}] ", lvl_str), lvl_style),
                Span::styled(&log.message, Style::default().fg(Color::White)),
            ]);

            ListItem::new(line)
        })
        .collect();

    let list = List::new(items).block(block);
    let mut state = ListState::default().with_selected((!app.git_changes.is_empty()).then_some(app.git_cursor));
    frame.render_stateful_widget(list, area, &mut state);
}

// -----------------------------------------------------------------------------
// Modals: Profile Switcher, Password Prompt, Help
// -----------------------------------------------------------------------------

fn draw_modal(frame: &mut Frame, app: &App, modal: &ModalState, screen_size: Rect) {
    match modal {
        ModalState::Help => {
            let area = centered_rect(60, 60, screen_size);
            frame.render_widget(Clear, area);

            let help_text = vec![
                Line::from(Span::styled("⚡ SFTUI Keybindings Guide", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
                Line::from(""),
                Line::from(vec![Span::styled("Tab       ", Style::default().fg(Color::Cyan)), Span::raw("Cycle active pane (Left -> Right -> Bottom)")]),
                Line::from(vec![Span::styled("1, 2      ", Style::default().fg(Color::Cyan)), Span::raw("Switch tabs in current pane")]),
                Line::from(vec![Span::styled("j / Down  ", Style::default().fg(Color::Cyan)), Span::raw("Navigate down in list/table")]),
                Line::from(vec![Span::styled("k / Up    ", Style::default().fg(Color::Cyan)), Span::raw("Navigate up in list/table")]),
                Line::from(vec![Span::styled("Enter     ", Style::default().fg(Color::Cyan)), Span::raw("Open folder / Upload selected / Download selected")]),
                Line::from(vec![Span::styled("Backspace ", Style::default().fg(Color::Cyan)), Span::raw("Navigate to parent directory")]),
                Line::from(vec![Span::styled("u         ", Style::default().fg(Color::Cyan)), Span::raw("Upload selected file or folder (batch)")]),
                Line::from(vec![Span::styled("d         ", Style::default().fg(Color::Cyan)), Span::raw("Download selected remote file or folder (batch)")]),
                Line::from(vec![Span::styled("s         ", Style::default().fg(Color::Cyan)), Span::raw("Batch sync selected local/remote folder")]),
                Line::from(vec![Span::styled("m         ", Style::default().fg(Color::Cyan)), Span::raw("Change remote permissions (chmod) on selected item")]),
                Line::from(vec![Span::styled("G         ", Style::default().fg(Color::Cyan)), Span::raw("One-shot Batch Upload of all uncommitted Git changes")]),
                Line::from(vec![Span::styled("p         ", Style::default().fg(Color::Cyan)), Span::raw("Open Profile Switcher (sftp.json profiles)")]),
                Line::from(vec![Span::styled("r         ", Style::default().fg(Color::Cyan)), Span::raw("Refresh directory listings and Git status")]),
                Line::from(vec![Span::styled("c         ", Style::default().fg(Color::Cyan)), Span::raw("Clear completed queue jobs")]),
                Line::from(vec![Span::styled("? / h     ", Style::default().fg(Color::Cyan)), Span::raw("Toggle this Help modal")]),
                Line::from(vec![Span::styled("q / Ctrl+C", Style::default().fg(Color::Cyan)), Span::raw("Quit gracefully")]),
                Line::from(""),
                Line::from(Span::styled("Press [Esc] or [q] to close this help window.", Style::default().fg(Color::DarkGray))),
            ];

            let block = Block::default()
                .title(" HELP & KEYBINDINGS ")
                .borders(Borders::ALL)
                .border_type(BorderType::Double)
                .border_style(Style::default().fg(Color::Yellow));

            let p = Paragraph::new(help_text).block(block).wrap(Wrap { trim: true });
            frame.render_widget(p, area);
        }

        ModalState::ProfileSwitcher { selected } => {
            let area = centered_rect(50, 40, screen_size);
            frame.render_widget(Clear, area);

            let profiles = app.config_store.profile_names();
            let items: Vec<ListItem> = profiles
                .iter()
                .enumerate()
                .map(|(i, name)| {
                    let is_active = name == &app.active_profile_name;
                    let is_cursor = i == *selected;

                    let prefix = if is_cursor { " ▶ " } else { "   " };
                    let active_tag = if is_active { " [ACTIVE]" } else { "" };

                    let style = if is_cursor {
                        Style::default().fg(Color::Yellow).bg(Color::Rgb(40, 50, 70)).add_modifier(Modifier::BOLD)
                    } else if is_active {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default().fg(Color::White)
                    };

                    ListItem::new(format!("{}{}{}", prefix, name, active_tag)).style(style)
                })
                .collect();

            let block = Block::default()
                .title(" SWITCH PROFILE (↑/↓ to select, [Enter] apply, [Esc] cancel) ")
                .borders(Borders::ALL)
                .border_type(BorderType::Double)
                .border_style(Style::default().fg(Color::Cyan));

            let list = List::new(items).block(block);
            frame.render_widget(list, area);
        }

        ModalState::PasswordPrompt { buffer } => {
            let area = centered_rect(50, 20, screen_size);
            frame.render_widget(Clear, area);

            let masked: String = "*".repeat(buffer.len());
            let text = vec![
                Line::from("Enter SSH / SFTP Password:"),
                Line::from(""),
                Line::from(Span::styled(format!("> {}_", masked), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
                Line::from(""),
                Line::from(Span::styled("[Enter] Confirm   [Esc] Cancel", Style::default().fg(Color::DarkGray))),
            ];

            let block = Block::default()
                .title(" AUTHENTICATION REQUIRED ")
                .borders(Borders::ALL)
                .border_type(BorderType::Double)
                .border_style(Style::default().fg(Color::Red));

            let p = Paragraph::new(text).block(block).alignment(Alignment::Center);
            frame.render_widget(p, area);
        }

        ModalState::ChmodPrompt { name, buffer, .. } => {
            let area = centered_rect(50, 25, screen_size);
            frame.render_widget(Clear, area);

            let text = vec![
                Line::from(format!("Set octal permissions (chmod) for '{}':", name)),
                Line::from(""),
                Line::from(Span::styled(format!("> {}_", buffer), Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))),
                Line::from(""),
                Line::from(Span::styled("[Enter] Apply   [Esc] Cancel", Style::default().fg(Color::DarkGray))),
            ];

            let block = Block::default()
                .title(" REMOTE PERMISSIONS (CHMOD) ")
                .borders(Borders::ALL)
                .border_type(BorderType::Double)
                .border_style(Style::default().fg(Color::Cyan));

            let p = Paragraph::new(text).block(block).alignment(Alignment::Center);
            frame.render_widget(p, area);
        }
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
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
