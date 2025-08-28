
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Block, Borders, List, ListItem, Paragraph, Clear},
    text::{Span, Line},
    Frame,
};
use std::path::{Path, PathBuf};
use super::{app::{AppState, Focus, Pane, Mode, UiMsg}, theme::Theme, remote};

// Remote operations handled by tui::remote actor

#[derive(Clone)]
pub struct Entry { pub name: String, pub is_dir: bool, pub is_symlink: bool }

#[derive(Clone)]
pub enum PathSpec { Local(PathBuf), Remote{ host: String, port: u16, path: PathBuf } }

pub fn draw(f: &mut Frame, app: &AppState) {
    // Apply Dracula background to entire terminal
    let background = Block::default().style(ratatui::style::Style::default().bg(Theme::BG));
    f.render_widget(background, f.size());
    
    // Layout with header bar
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(1), Constraint::Length(3), Constraint::Length(2)].as_ref())
        .split(f.size());
    
    // Professional header bar
    draw_header(f, chunks[0], app);
    
    // Two-pane layout
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
        .split(chunks[1]);
    draw_pane(f, cols[0], &app.left, app.focus == Focus::Left, app.loading_pane == Some(Focus::Left), true);
    draw_pane(f, cols[1], &app.right, app.focus == Focus::Right, app.loading_pane == Some(Focus::Right), false);

    // Log area (last few lines) with proper styling
    let mut lines: Vec<Line> = Vec::new();
    let max_lines = 3usize;
    let start = app.log.len().saturating_sub(max_lines);
    for l in app.log.iter().skip(start) {
        lines.push(Line::from(Span::styled(l.clone(), ratatui::style::Style::default().fg(Theme::COMMENT))));
    }
    // Pad with empty lines if needed
    while lines.len() < max_lines {
        lines.push(Line::from(""));
    }
    let logp = Paragraph::new(lines)
        .block(Block::default()
            .borders(Borders::NONE)
            .style(ratatui::style::Style::default().bg(Theme::BG)));
    f.render_widget(logp, chunks[2]);

    // Status line with mode info
    let spinner = if app.running {
        if is_ascii_mode() {
            ['|', '/', '-', '\\', '|', '/', '-', '\\', '|', '/'][app.spinner_idx % 10]
        } else {
            ['⠋','⠙','⠹','⠸','⠼','⠴','⠦','⠧','⠇','⠏'][app.spinner_idx % 10]
        }
    } else { ' ' };
    
    // Build status with better formatting
    let mode_icon = if is_ascii_mode() {
        match app.mode {
            Mode::Mirror => "[M]",
            Mode::Copy => "[C]",
            Mode::Move => "[>]",
        }
    } else {
        match app.mode {
            Mode::Mirror => "🔄",
            Mode::Copy => "📋",
            Mode::Move => "➡️",
        }
    };
    
    // First line: status and settings
    let check = if is_ascii_mode() { "Y" } else { "✓" };
    let cross = if is_ascii_mode() { "N" } else { "✗" };
    let status_parts = vec![
        format!("{} {}", mode_icon, mode_str(app.mode)),
        
    ];
    
    let src_dst = format!("SRC:{} → DST:{}", 
        path_short(app.src.as_ref()), 
        path_short(app.dest.as_ref())
    );
    
    let status_text = if app.running {
        format!("{} {} │ {} │ {}", spinner, app.status, status_parts.join(" "), src_dst)
    } else if !app.status.is_empty() {
        format!("  {} │ {} │ {}", app.status, status_parts.join(" "), src_dst)
    } else {
        format!("  Ready │ {} │ {}", status_parts.join(" "), src_dst)
    };
    
    // Second line: command keys with better visual separation
    let keys = if get_show_help(app) {
        " Press [h] to close help"
    } else if app.ui_mode == super::app::UiMode::ServerInput {
        " Type server address • [Enter] connect • [Esc] cancel"
    } else if app.ui_mode == super::app::UiMode::NewFolderInput {
        " Type folder name • [Enter] create • [Esc] cancel"
    } else if app.ui_mode == super::app::UiMode::ConfirmTransfer {
        " ⚠️  Confirm: [Y]es execute • [N]/[Esc] cancel"
    } else if app.ui_mode == super::app::UiMode::ConfirmTyped {
        " Type confirmation text and press Enter • [Esc] cancel"
    } else if app.ui_mode == super::app::UiMode::TextInput {
        " Type value • [Enter] save • [Esc] cancel"
    } else if app.ui_mode == super::app::UiMode::Options {
        " [↑/↓] move  [Space/Enter] toggle  [Esc] close"
    } else {
        " [Tab]switch [↑↓]nav [Space]select [Enter]go [Esc]back [Backspace]swap [F2]connect [Ctrl+G]transfer [h]elp [q]uit"
    };
    
    let status_lines = vec![
        Line::from(Span::styled(status_text, ratatui::style::Style::default().fg(Theme::FG))),
        Line::from(Span::styled(keys, ratatui::style::Style::default().fg(Theme::COMMENT))),
    ];
    
    let p = Paragraph::new(status_lines)
        .block(Block::default()
            .borders(Borders::TOP)
            .border_style(ratatui::style::Style::default().fg(Theme::COMMENT))
            .style(ratatui::style::Style::default().bg(Theme::BG)));
    f.render_widget(p, chunks[3]);
    
    // Toast notifications
    if let Some((msg, _)) = &app.toast {
        let toast_area = centered_rect(40, 5, f.size());
        let toast_style = if msg.starts_with("Error:") || msg.starts_with("✗") {
            ratatui::style::Style::default().fg(Theme::RED).bg(Theme::BG)
        } else {
            ratatui::style::Style::default().fg(Theme::GREEN).bg(Theme::BG)
        };
        let toast_widget = Paragraph::new(msg.as_str())
            .alignment(ratatui::layout::Alignment::Center)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(toast_style)
                .style(toast_style));
        f.render_widget(toast_widget, toast_area);
    }
    
    // Help overlay with proper background
    if get_show_help(app) {
        let area = centered_rect(60, 40, f.size());
        
        // Clear the background area first with Clear widget
        f.render_widget(Clear, area);
        
        let lines = vec![
            Line::from(Span::styled("Navigation:", ratatui::style::Style::default().fg(Theme::CYAN).add_modifier(ratatui::style::Modifier::BOLD))),
            Line::from("  ↑/↓/k/j    Navigate up/down"),
            Line::from("  Enter      Enter directory"),
            Line::from("  Tab/f      Switch pane (focus)"),
            Line::from("  Alt+←/→    Switch to left/right pane"),
            Line::from(""),
            Line::from(Span::styled("File Operations:", ratatui::style::Style::default().fg(Theme::CYAN).add_modifier(ratatui::style::Modifier::BOLD))),
            Line::from("  Space      Select item for current pane"),
            Line::from("  Backspace  Swap panes (Source/Target)"),
            Line::from("  Enter      Enter directory"),
            Line::from("  Ctrl+G     Start transfer"),
            Line::from("  Esc        Abort transfer / go back"),
            Line::from(""),
            Line::from(Span::styled("Options:", ratatui::style::Style::default().fg(Theme::CYAN).add_modifier(ratatui::style::Modifier::BOLD))),
            Line::from("  t          Toggle TAR for small files"),
            Line::from("  r          Toggle delta for large files"),
            Line::from("  e          Toggle empty directories"),
            Line::from("  c          Toggle checksums"),
            Line::from("  O          Open Options modal"),
            Line::from("  Ctrl+←/→   Switch tabs in Options"),
            Line::from(""),
            Line::from(Span::styled("Remote:", ratatui::style::Style::default().fg(Theme::CYAN).add_modifier(ratatui::style::Modifier::BOLD))),
            Line::from("  F2         Connect to remote server"),
            Line::from(Span::styled("Options:", ratatui::style::Style::default().fg(Theme::CYAN).add_modifier(ratatui::style::Modifier::BOLD))),
            Line::from("  O          Open Options"),
            Line::from(""),
            Line::from(Span::styled("General:", ratatui::style::Style::default().fg(Theme::CYAN).add_modifier(ratatui::style::Modifier::BOLD))),
            Line::from("  h          Toggle this help"),
            Line::from("  q          Quit"),
        ];
        
        let help_widget = Paragraph::new(lines)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(ratatui::style::Style::default().fg(Theme::PINK))
                .title(Span::styled(" Help ", ratatui::style::Style::default().fg(Theme::PINK).add_modifier(ratatui::style::Modifier::BOLD)))
                .style(ratatui::style::Style::default().bg(Theme::BG).fg(Theme::FG)));
        f.render_widget(help_widget, area);
    }
    
    // Server input overlay with proper background
    if app.ui_mode == super::app::UiMode::ServerInput {
        let area = centered_rect(50, 12, f.size());
        
        // Clear the background area first with Clear widget
        f.render_widget(Clear, area);
        
        // Build the input display with a cursor
        // Show placeholder text when empty, otherwise show the buffer content with cursor
        let input_display = if app.input_buffer.is_empty() {
            "Type here...█".to_string()
        } else {
            format!("{}█", app.input_buffer)
        };
        
        let input_text = vec![
            Line::from(""),
            Line::from(Span::styled("Enter server address (host:port):", ratatui::style::Style::default().fg(Theme::CYAN))),
            Line::from(""),
            Line::from(vec![
                Span::styled(" ▶ ", ratatui::style::Style::default().fg(Theme::CYAN)),
                Span::styled(&input_display, ratatui::style::Style::default().fg(Theme::GREEN).add_modifier(ratatui::style::Modifier::BOLD)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Enter", ratatui::style::Style::default().fg(Theme::PINK).add_modifier(ratatui::style::Modifier::BOLD)),
                Span::raw(" to connect, "),
                Span::styled("Esc", ratatui::style::Style::default().fg(Theme::PINK).add_modifier(ratatui::style::Modifier::BOLD)),
                Span::raw(" to cancel"),
            ]),
        ];
        
        let input_block = Paragraph::new(input_text)
            .alignment(ratatui::layout::Alignment::Center)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(ratatui::style::Style::default().fg(Theme::CYAN))
                .title(Span::styled(" Remote Server Connection ", ratatui::style::Style::default().fg(Theme::CYAN).add_modifier(ratatui::style::Modifier::BOLD)))
                .style(ratatui::style::Style::default().bg(Theme::BG).fg(Theme::FG)));
        f.render_widget(input_block, area);
    }

    // HACK: Communicate the logical index of the selected option row to the key handler
    unsafe { CURRENT_LOGICAL_INDEX = usize::MAX; }
    // Options overlay (tabs: Basics, Safety, Performance, Filters, Links, Logging, Network)
    if app.ui_mode == super::app::UiMode::Options {
        let area = centered_rect(60, 60, f.size());
        f.render_widget(Clear, area);

        let on = |b: bool| if b { if is_ascii_mode() { "on" } else { "✓" } } else { if is_ascii_mode() { "off" } else { "✗" } };

        // Tabs header
        let tabs = ["Basics", "Safety", "Performance", "Filters", "Links", "Logging", "Network"];
        let mut header = String::new();
        for (i, t) in tabs.iter().enumerate() {
            if i == app.options_tab { header.push_str(&format!("[{}] ", t)); } else { header.push_str(&format!(" {}  ", t)); }
        }

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled(header, ratatui::style::Style::default().fg(Theme::CYAN).add_modifier(ratatui::style::Modifier::BOLD))));
        lines.push(Line::from(""));

        // Build items for current tab; set cursor range by writing into app.options_cursor limit via OPTIONS_COUNT
        let mut items: Vec<(String, String, usize)> = Vec::new(); // (label, value, logical index)
        match app.options_tab {
            0 => { // Basics
                // Mode selection
                let mode_label = match app.mode { Mode::Copy => "Copy", Mode::Mirror => "Mirror", Mode::Move => "Move" };
                items.push((format!("Mode: {} (Left/Right/Enter to cycle)", mode_label), "".into(), 50));
                items.push(("Verbose output".into(), on(app.options.verbose).into(), 0));
                items.push(("Show progress".into(), on(app.options.progress).into(), 1));
                items.push(("Include empty directories".into(), on(app.options.include_empty).into(), 2));
                items.push(("Update only (no delete)".into(), on(app.options.update).into(), 3));
                items.push(("Ludicrous speed (safe-fast)".into(), on(app.options.ludicrous_speed).into(), 7));
                items.push(("  Boosts workers/chunk sizes; disables verify and resume".into(), "".into(), usize::MAX));
                items.push(("  TLS stays enabled; use Unsafe Mode to disable".into(), "".into(), usize::MAX));
                // keep cursor bounds enforced in event handler
            }
            1 => { // Safety
                items.push(("Compare by checksum".into(), on(app.options.checksum).into(), 4));
                items.push(("Skip post-verify".into(), on(app.options.no_verify).into(), 5));
                items.push(("Disable resume (not recommended)".into(), on(app.options.no_restart).into(), 6));
                // Unsafe mode is CLI-only. If active, banner appears in header.
                // keep cursor bounds enforced in event handler
            }
            2 => { // Performance
                let thr = if app.options.threads == 0 { "auto".to_string() } else { app.options.threads.to_string() };
                let nws = if app.options.net_workers == 0 { "auto".to_string() } else { app.options.net_workers.to_string() };
                let ncm = if app.options.net_chunk_mb == 0 { "auto".to_string() } else { app.options.net_chunk_mb.to_string() };
                items.push((format!("Threads (-t): {} (Left/Right adjust, Backspace auto)", thr), "".into(), 100));
                items.push((format!("Net workers: {} (Left/Right adjust, Backspace auto)", nws), "".into(), 101));
                items.push((format!("Net chunk MB: {} (Left/Right adjust, Backspace auto)", ncm), "".into(), 102));
                // keep cursor bounds enforced in event handler
            }
            3 => { // Filters
                items.push((format!("Exclude files (xf): {} (Enter=add, Del=remove selected)", app.options.exclude_files.len()), "".into(), 220));
                for (i, pat) in app.options.exclude_files.iter().enumerate() {
                    items.push((format!("  - {}", pat), "".into(), 240 + i));
                }
                items.push((format!("Exclude dirs (xd): {} (Enter=add, Del=remove selected)", app.options.exclude_dirs.len()), "".into(), 221));
                for (i, pat) in app.options.exclude_dirs.iter().enumerate() {
                    items.push((format!("  - {}", pat), "".into(), 260 + i));
                }
                // keep cursor bounds enforced in event handler
            }
            4 => { // Links
                // Blit-native phrasing for clarity
                items.push(("Copy symlinks as links (do not follow)".into(), on(app.options.sl).into(), 200));
                #[cfg(windows)]
                {
                    items.push(("Copy junctions as junctions (Windows)".into(), on(app.options.sj).into(), 201));
                    items.push(("Windows symlink privileges required (Dev Mode/admin)".into(), "".into(), usize::MAX));
                }
                items.push(("Exclude all symlinks".into(), on(app.options.xj).into(), 202));
                items.push(("Exclude directory symlinks".into(), on(app.options.xjd).into(), 203));
                items.push(("Exclude file symlinks".into(), on(app.options.xjf).into(), 204));
                // keep cursor bounds enforced in event handler
            }
            5 => { // Logging
                let path = app.options.log_file.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "(none)".into());
                items.push((format!("Log file: {} (Enter=set, Del=clear)", path), "".into(), 230));
                // keep cursor bounds enforced in event handler
            }
            6 => { // Network
                items.push(("TLS: enabled (TOFU)".into(), "".into(), usize::MAX));
                items.push((format!("Recent hosts: {}", app.options.recent_hosts.len()), "".into(), usize::MAX));
                for (i, h) in app.options.recent_hosts.iter().take(5).enumerate() {
                    items.push((format!("- {}:{} (Enter=connect)", h.host, h.port), "".into(), 300 + i));
                }
                // keep cursor bounds enforced in event handler
            }
            _ => {}
        }

        for (idx, (label, val, logical)) in items.iter().enumerate() {
            let marker = if idx == app.options_cursor { if is_ascii_mode() { ">" } else { "➤" } } else { " " };
            let mut segs = vec![
                Span::raw(format!(" {} ", marker)),
                Span::styled(label, ratatui::style::Style::default().fg(Theme::FG)),
            ];
            if !val.is_empty() {
                segs.push(Span::raw("  "));
                segs.push(Span::styled(val, ratatui::style::Style::default().fg(Theme::PINK).add_modifier(ratatui::style::Modifier::BOLD)));
            }
            // Update the global cursor index to match logical index so handler can toggle/adjust
            if idx == app.options_cursor { unsafe { CURRENT_LOGICAL_INDEX = *logical; } }
            lines.push(Line::from(segs));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled("Ctrl+←/→ switch tab • Esc save & close", ratatui::style::Style::default().fg(Theme::COMMENT))));

        let p = Paragraph::new(lines)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(ratatui::style::Style::default().fg(Theme::CYAN))
                .title(Span::styled(" Options ", ratatui::style::Style::default().fg(Theme::CYAN).add_modifier(ratatui::style::Modifier::BOLD)))
                .style(ratatui::style::Style::default().bg(Theme::BG).fg(Theme::FG)));
        f.render_widget(p, area);
    }

    // Text input overlay for Options edits
    if app.ui_mode == super::app::UiMode::TextInput {
        let area = centered_rect(60, 12, f.size());
        f.render_widget(Clear, area);
        let title = match app.input_kind {
            Some(super::app::InputKind::AddExcludeFile) => "Add exclude file pattern (glob)",
            Some(super::app::InputKind::AddExcludeDir) => "Add exclude dir pattern (glob)",
            Some(super::app::InputKind::SetLogFile) => "Set log file path",
            None => "Input",
        };
        let display = if app.input_buffer.is_empty() { "█".to_string() } else { format!("{}█", app.input_buffer) };
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(title, ratatui::style::Style::default().fg(Theme::CYAN))),
            Line::from(""),
            Line::from(display),
            Line::from(""),
            Line::from(Span::styled("Enter to save • Esc to cancel", ratatui::style::Style::default().fg(Theme::COMMENT))),
        ];
        let p = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).border_style(ratatui::style::Style::default().fg(Theme::CYAN))
                .title(Span::styled(" Input ", ratatui::style::Style::default().fg(Theme::CYAN).add_modifier(ratatui::style::Modifier::BOLD)))
                .style(ratatui::style::Style::default().bg(Theme::BG).fg(Theme::FG)));
        f.render_widget(p, area);
    }

    // Confirm typed overlay with command preview
    if app.ui_mode == super::app::UiMode::ConfirmTyped || app.ui_mode == super::app::UiMode::ConfirmTransfer {
        let area = centered_rect(70, 40, f.size());
        f.render_widget(Clear, area);

        // Build preview command in readable multi-line format
        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(Span::styled("Ready to run:", ratatui::style::Style::default().fg(Theme::CYAN).add_modifier(ratatui::style::Modifier::BOLD))));
        if let Some(argv) = &app.pending_args {
            let exe = crate::resolve_blit_path().display().to_string();
            lines.push(Line::from(Span::styled(exe, ratatui::style::Style::default().fg(Theme::FG))));
            for a in argv {
                let arg = if a.contains(' ') { format!("\"{}\"", a) } else { a.clone() };
                lines.push(Line::from(format!("  {}", arg)));
            }
        }
        lines.push(Line::from(""));
        if let Some(req) = &app.confirm_required_input {
            lines.push(Line::from(Span::styled(format!("Type '{}' to confirm:", req), ratatui::style::Style::default().fg(Theme::PINK))));
            let display = format!("{}█", app.confirm_input);
            lines.push(Line::from(display));
        } else {
            lines.push(Line::from(Span::styled("Press Y to execute, N/Esc to cancel", ratatui::style::Style::default().fg(Theme::COMMENT))));
        }

        let p = Paragraph::new(lines)
            .block(Block::default().borders(Borders::ALL).border_style(ratatui::style::Style::default().fg(Theme::PINK))
                .title(Span::styled(" Confirmation ", ratatui::style::Style::default().fg(Theme::PINK).add_modifier(ratatui::style::Modifier::BOLD)))
                .style(ratatui::style::Style::default().bg(Theme::BG).fg(Theme::FG)));
        f.render_widget(p, area);
    }
    
    // New folder input overlay with proper background
    if app.ui_mode == super::app::UiMode::NewFolderInput {
        let area = centered_rect(50, 12, f.size());
        
        // Clear the background area first with Clear widget
        f.render_widget(Clear, area);
        
        // Build the input display with a cursor
        let input_display = if app.input_buffer.is_empty() {
            "New folder name...█".to_string()
        } else {
            format!("{}█", app.input_buffer)
        };
        
        // Show where the folder will be created
        let location = match &app.right {
            Pane::Local { cwd, .. } => format!("in: {}", cwd.display()),
            Pane::Remote { host, port, cwd, .. } => format!("in: {}:{}{}", host, port, cwd.display()),
        };
        
        let input_text = vec![
            Line::from(""),
            Line::from(Span::styled("Enter new folder name:", ratatui::style::Style::default().fg(Theme::GREEN))),
            Line::from(Span::styled(location, ratatui::style::Style::default().fg(Theme::COMMENT))),
            Line::from(""),
            Line::from(vec![
                Span::styled(" ▶ ", ratatui::style::Style::default().fg(Theme::GREEN)),
                Span::styled(&input_display, ratatui::style::Style::default().fg(Theme::YELLOW).add_modifier(ratatui::style::Modifier::BOLD)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled("Enter", ratatui::style::Style::default().fg(Theme::PINK).add_modifier(ratatui::style::Modifier::BOLD)),
                Span::raw(" to create, "),
                Span::styled("Esc", ratatui::style::Style::default().fg(Theme::PINK).add_modifier(ratatui::style::Modifier::BOLD)),
                Span::raw(" to cancel"),
            ]),
        ];
        
        let input_block = Paragraph::new(input_text)
            .alignment(ratatui::layout::Alignment::Center)
            .block(Block::default()
                .borders(Borders::ALL)
                .border_style(ratatui::style::Style::default().fg(Theme::GREEN))
                .title(Span::styled(" Create New Folder ", ratatui::style::Style::default().fg(Theme::GREEN).add_modifier(ratatui::style::Modifier::BOLD)))
                .style(ratatui::style::Style::default().bg(Theme::BG).fg(Theme::FG)));
        f.render_widget(input_block, area);
    }
}

// HACK: A minimal bridge to pass the logical index for options toggling/adjustment
// In a later refactor, move options rendering and event handling into a single module.
static mut CURRENT_LOGICAL_INDEX: usize = usize::MAX;
pub fn current_options_logical_index() -> usize { unsafe { CURRENT_LOGICAL_INDEX } }

pub fn is_ascii_mode() -> bool {
    // Check if we should use ASCII mode (for non-UTF8 terminals)
    std::env::var("BLIT_ASCII").is_ok() || std::env::var("TERM").map_or(false, |t| t.contains("dumb"))
}

fn make_breadcrumb(path: &Path, max_len: usize) -> String {
    let path_str = path.display().to_string();
    
    // Special handling for Windows paths
    #[cfg(windows)]
    {
        if path_str == "\\\\?\\drives" || path_str == "/drives" {
            return "Drive Selection".to_string();
        }
        // Show drive letter prominently for Windows paths
        if path_str.len() >= 3 && path_str.chars().nth(1) == Some(':') {
            // This is a Windows path with drive letter
            if path_str.len() <= max_len {
                return path_str;
            }
        }
    }
    
    if path_str.len() <= max_len {
        path_str
    } else {
        // Show last parts of path that fit
        let parts: Vec<&str> = path_str.split('/').filter(|s| !s.is_empty()).collect();
        let mut result = String::new();
        for part in parts.iter().rev() {
            let candidate = if result.is_empty() {
                part.to_string()
            } else {
                format!(".../{}/{}", part, result)
            };
            if candidate.len() <= max_len {
                result = candidate;
            } else {
                if result.is_empty() {
                    result = format!("...{}", &part[part.len().saturating_sub(max_len-3)..]);
                } else {
                    result = format!(".../{}", result);
                }
                break;
            }
        }
        if result.is_empty() { "...".to_string() } else { result }
    }
}

fn draw_header(f: &mut Frame, area: Rect, app: &AppState) {
    let mode_str = match app.mode {
        Mode::Mirror => "Mirror",
        Mode::Copy => "Copy",
        Mode::Move => "Move",
    };
    
    let check = if is_ascii_mode() { "on" } else { "✓" };
    let cross = if is_ascii_mode() { "off" } else { "✗" };
    
    // Prominent source/destination display for header
    let src_display = match &app.src {
        Some(PathSpec::Local(path)) => format!("📂 {}", make_breadcrumb(path, 25)),
        Some(PathSpec::Remote { host, port, path }) => format!("🌐 {}:{}{}", host, port, make_breadcrumb(path, 20)),
        None => "❌ No source selected".to_string(),
    };
    
    let dest_display = match &app.dest {
        Some(PathSpec::Local(path)) => format!("📁 {}", make_breadcrumb(path, 25)),
        Some(PathSpec::Remote { host, port, path }) => format!("🌐 {}:{}{}", host, port, make_breadcrumb(path, 20)),
        None => "❌ No destination selected".to_string(),
    };

    let mut header_text = vec![
        Line::from(vec![
            Span::styled("SOURCE: ", ratatui::style::Style::default().fg(Theme::CYAN).add_modifier(ratatui::style::Modifier::BOLD)),
            Span::styled(&src_display, ratatui::style::Style::default().fg(if app.src.is_some() { Theme::GREEN } else { Theme::RED })),
            Span::raw("  "),
            Span::styled("MODE: ", ratatui::style::Style::default().fg(Theme::CYAN).add_modifier(ratatui::style::Modifier::BOLD)),
            Span::styled(mode_str, ratatui::style::Style::default().fg(Theme::PINK).add_modifier(ratatui::style::Modifier::BOLD)),
            Span::raw(" | "),
            Span::raw("TAR:"),
            Span::styled(if app.tar_small { check } else { cross }, 
                if app.tar_small { ratatui::style::Style::default().fg(Theme::GREEN) } else { ratatui::style::Style::default().fg(Theme::COMMENT) }),
            Span::raw(" Delta:"),
            Span::styled(if app.delta_large { check } else { cross },
                if app.delta_large { ratatui::style::Style::default().fg(Theme::GREEN) } else { ratatui::style::Style::default().fg(Theme::COMMENT) }),
            Span::raw(" Empty:"),
            Span::styled(if app.include_empty { check } else { cross },
                if app.include_empty { ratatui::style::Style::default().fg(Theme::GREEN) } else { ratatui::style::Style::default().fg(Theme::COMMENT) }),
            Span::raw(" Checksum:"),
            Span::styled(if app.checksum { check } else { cross },
                if app.checksum { ratatui::style::Style::default().fg(Theme::GREEN) } else { ratatui::style::Style::default().fg(Theme::COMMENT) }),
        ]),
        Line::from(vec![
            Span::styled("TARGET: ", ratatui::style::Style::default().fg(Theme::CYAN).add_modifier(ratatui::style::Modifier::BOLD)),
            Span::styled(&dest_display, ratatui::style::Style::default().fg(if app.dest.is_some() { Theme::GREEN } else { Theme::RED })),
        ]),
    ];
    if app.options.never_tell_me_the_odds {
        header_text.push(Line::from(Span::styled(
            "UNSAFE MODE ACTIVE — TLS and safety checks DISABLED",
            ratatui::style::Style::default().fg(Theme::RED).add_modifier(ratatui::style::Modifier::BOLD)
        )));
    }
    
    let header_widget = Paragraph::new(header_text)
        .alignment(ratatui::layout::Alignment::Center)
        .block(Block::default()
            .borders(Borders::BOTTOM)
            .border_style(ratatui::style::Style::default().fg(Theme::COMMENT))
            .style(ratatui::style::Style::default().bg(Theme::BG)));
    f.render_widget(header_widget, area);
}

fn draw_pane(f: &mut Frame, area: Rect, pane: &Pane, focused: bool, is_loading: bool, is_source: bool) {
    let (title, entries, selected) = match pane {
        Pane::Local { cwd, entries, selected } => {
            let icon = if is_ascii_mode() { if is_source {"[S]"} else {"[T]"} } else { if is_source {"📤"} else {"📥"} };
            let breadcrumb = make_breadcrumb(cwd, 40);
            let label = if is_source {"Source"} else {"Target"}; let title = format!(" {} {}: {} ", icon, label, breadcrumb);
            (title, entries, *selected)
        },
        Pane::Remote { host, port, cwd, entries, selected } => {
            let icon = if is_ascii_mode() { if is_source {"[S]"} else {"[T]"} } else { if is_source {"📤"} else {"📥"} };
            let breadcrumb = make_breadcrumb(cwd, 30);
            // Avoid duplicating :port if host already includes it
            let host_port = if host.contains(':') { host.clone() } else { format!("{}:{}", host, port) };
            let label = if is_source {"Source"} else {"Target"}; let title = format!(" {} {} {} {} ", icon, label, host_port, breadcrumb);
            (title, entries, *selected)
        },
    };
    
    // Build list items with icons
    let items: Vec<ListItem> = if is_loading {
        vec![ListItem::new(Span::styled(
            if is_ascii_mode() { "Loading..." } else { "⏳ Loading..." },
            ratatui::style::Style::default().fg(Theme::COMMENT)
        ))]
    } else {
        entries.iter().enumerate().map(|(i, e)| {
            let icon = if is_ascii_mode() {
                if e.name == ".." {
                    "[..] "
                } else if e.is_dir {
                    "[D] "
                } else if e.is_symlink {
                    "[L] "
                } else {
                    "    "
                }
            } else {
                if e.name == ".." {
                    "⬆ "
                } else if e.is_dir {
                    "📁 "
                } else if e.is_symlink {
                    "🔗 "
                } else {
                    "📄 "
                }
            };
            
            let name_with_icon = format!("{}{}", icon, e.name);
            
            let style = if i == selected { 
                Theme::selected() 
            } else if e.is_dir { 
                Theme::dir() 
            } else if e.is_symlink { 
                Theme::symlink() 
            } else { 
                Theme::file() 
            };
            
            ListItem::new(Span::styled(name_with_icon, style))
        }).collect()
    };
    
    // Apply Dracula theme to the block with better border styling
    let (border_style, title_style) = if focused {
        (
            ratatui::style::Style::default()
                .fg(Theme::PINK)
                .bg(Theme::BG)
                .add_modifier(ratatui::style::Modifier::BOLD),
            ratatui::style::Style::default()
                .fg(Theme::PINK)
                .bg(Theme::BG)
                .add_modifier(ratatui::style::Modifier::BOLD)
        )
    } else {
        (
            ratatui::style::Style::default()
                .fg(Theme::COMMENT)
                .bg(Theme::BG),
            ratatui::style::Style::default()
                .fg(Theme::COMMENT)
                .bg(Theme::BG)
        )
    };
    
    // Create a stateful list to properly handle selection
    let mut list_state = ratatui::widgets::ListState::default();
    list_state.select(Some(selected));
    
    let list = List::new(items)
        .block(Block::default()
            .borders(Borders::ALL)
            .border_style(border_style)
            .title(Span::styled(title, title_style))
            .style(ratatui::style::Style::default().bg(Theme::BG)))
        .highlight_style(if focused {
            ratatui::style::Style::default()
                .bg(Theme::PINK)
                .fg(Theme::BG)
                .add_modifier(ratatui::style::Modifier::BOLD)
        } else {
            ratatui::style::Style::default()
                .bg(Theme::COMMENT)
                .fg(Theme::FG)
        })
        .highlight_symbol(if focused { "▶ " } else { "  " });
    
    f.render_stateful_widget(list, area, &mut list_state);
}

pub fn read_local_dir(cwd: &Path) -> Vec<Entry> {
    // Special case for Windows drive listing
    #[cfg(windows)]
    {
        let cwd_str = cwd.to_string_lossy();
        if cwd_str == "\\\\?\\drives" || cwd_str == "/drives" || cwd.as_os_str().is_empty() {
            return get_windows_drives();
        }
    }
    
    let mut out = Vec::new();
    
    // Only add .. if not at drive root on Windows
    #[cfg(windows)]
    {
        let cwd_str = cwd.to_string_lossy();
        if !cwd_str.ends_with(":\\") && cwd != Path::new("/drives") {
            out.push(Entry { name: "..".to_string(), is_dir: true, is_symlink: false });
        }
    }
    #[cfg(not(windows))]
    {
        out.push(Entry { name: "..".to_string(), is_dir: true, is_symlink: false });
    }
    
    // TUI pagination: Cap at 1000 entries to keep UI responsive
    const MAX_LIST_ENTRIES: usize = 1000;
    let mut entries = Vec::new();
    let mut entry_count = 0;
    
    if let Ok(rd) = std::fs::read_dir(cwd) {
        for e in rd.flatten() {
            if entry_count >= MAX_LIST_ENTRIES {
                // Add truncation marker
                entries.push(Entry { 
                    name: format!("... ({} entries max)", MAX_LIST_ENTRIES), 
                    is_dir: false, 
                    is_symlink: false 
                });
                break;
            }
            let ft = e.file_type().ok();
            let name = e.file_name().to_string_lossy().to_string();
            let is_dir = ft.as_ref().map(|t| t.is_dir()).unwrap_or(false);
            let is_symlink = ft.as_ref().map(|t| t.is_symlink()).unwrap_or(false);
            entries.push(Entry { name, is_dir, is_symlink });
            entry_count += 1;
        }
    }
    
    // Sort entries: directories first, then files, alphabetically within each
    entries.sort_by(|a, b| {
        match (a.is_dir, b.is_dir) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.name.cmp(&b.name),
        }
    });
    
    // Add sorted entries after ".."
    out.extend(entries);
    out
}

/// Request remote directory listing asynchronously
pub fn request_remote_dir(app: &mut super::app::AppState, pane: super::app::Focus, host: String, port: u16, path: PathBuf) {
    let _ = app.tx_ui.send(super::app::UiMsg::Loading { pane });
    remote::request_remote_dir(&app.tx_ui, pane, host, port, path);
}

/// Async version of remote directory reading
// Remote read functions moved to tui::remote

pub fn move_up(app: &mut super::app::AppState) {
    let pane = if app.focus == Focus::Left { &mut app.left } else { &mut app.right };
    match pane {
        Pane::Local { selected, .. } | Pane::Remote { selected, .. } => {
            if *selected > 0 { *selected -= 1; }
        }
    }
}

pub fn move_down(app: &mut super::app::AppState) {
    let pane = if app.focus == Focus::Left { &mut app.left } else { &mut app.right };
    match pane {
        Pane::Local { entries, selected, .. } | Pane::Remote { entries, selected, .. } => {
            if *selected + 1 < entries.len() { *selected += 1; }
        }
    }
}


pub fn move_page_up(app: &mut super::app::AppState) {
    let page = 10usize;
    let pane = if app.focus == Focus::Left { &mut app.left } else { &mut app.right };
    match pane {
        Pane::Local { selected, .. } | Pane::Remote { selected, .. } => {
            *selected = selected.saturating_sub(page);
        }
    }
}

pub fn move_page_down(app: &mut super::app::AppState) {
    let page = 10usize;
    let pane = if app.focus == Focus::Left { &mut app.left } else { &mut app.right };
    match pane {
        Pane::Local { entries, selected, .. } | Pane::Remote { entries, selected, .. } => {
            let max = entries.len().saturating_sub(1);
            let mut idx = selected.saturating_add(page);
            if idx > max { idx = max; }
            *selected = idx;
        }
    }
}

pub fn move_home(app: &mut super::app::AppState) {
    let pane = if app.focus == Focus::Left { &mut app.left } else { &mut app.right };
    match pane {
        Pane::Local { selected, .. } | Pane::Remote { selected, .. } => { *selected = 0; }
    }
}

pub fn move_end(app: &mut super::app::AppState) {
    let pane = if app.focus == Focus::Left { &mut app.left } else { &mut app.right };
    match pane {
        Pane::Local { entries, selected, .. } | Pane::Remote { entries, selected, .. } => {
            if !entries.is_empty() { *selected = entries.len() - 1; }
        }
    }
}

pub fn enter(app: &mut super::app::AppState) {
    let focused_pane = if app.focus == Focus::Left { &mut app.left } else { &mut app.right };
    
    match focused_pane {
        Pane::Local { entries, selected, cwd } => {
            if *selected >= entries.len() { return; }
            let name = entries[*selected].name.clone();
            
            // Handle Windows drive navigation safely
            #[cfg(windows)]
            if name.ends_with(":") || name.ends_with(":\\") {
                // User selected a drive letter - extract safely
                if let Some(first_char) = name.chars().next() {
                    *cwd = PathBuf::from(format!("{}:\\", first_char));
                    *entries = read_local_dir(cwd);
                    *selected = 0;
                }
                return;
            }
            
            if name == ".." {
                if let Some(parent) = cwd.parent() {
                    *cwd = parent.to_path_buf();
                } else {
                    // At root, show drive list on Windows
                    #[cfg(windows)]
                    {
                        // Use a special marker path
                        *cwd = PathBuf::from("\\\\?\\drives");
                        *entries = get_windows_drives();
                        *selected = 0;
                        return;
                    }
                }
            } else if entries[*selected].is_dir {
                *cwd = cwd.join(name);
            }
            
            *entries = read_local_dir(cwd);
            if *selected >= entries.len() { *selected = entries.len().saturating_sub(1); }
        }
        Pane::Remote { entries, selected, cwd, host, port } => {
            if *selected >= entries.len() { return; }
            let name = entries[*selected].name.clone();
            
            if name == ".." {
                if let Some(parent) = cwd.parent() { *cwd = parent.to_path_buf(); }
            } else if entries[*selected].is_dir {
                *cwd = cwd.join(name);
            }
                        if *selected >= entries.len() { *selected = entries.len().saturating_sub(1); }
        }
    }

    // After navigation, refresh remote pane asynchronously if applicable
    if let super::app::Focus::Left = app.focus {
        if let Pane::Remote { host, port, cwd, .. } = &app.left {
            let h = host.clone(); let p = *port; let c = cwd.clone();
            request_remote_dir(app, super::app::Focus::Left, h, p, c);
        }
    } else {
        if let Pane::Remote { host, port, cwd, .. } = &app.right {
            let h = host.clone(); let p = *port; let c = cwd.clone();
            request_remote_dir(app, super::app::Focus::Right, h, p, c);
        }
    }

}

pub fn current_path(app: &AppState) -> PathSpec {
    match app.focus {
        Focus::Left => match &app.left {
            Pane::Local { cwd, entries, selected } => {
                let mut p = cwd.clone();
                if *selected < entries.len() {
                    let name = &entries[*selected].name;
                    if name != ".." { p = p.join(name); }
                }
                PathSpec::Local(p)
            }
            Pane::Remote { host, port, cwd, entries, selected } => {
                let mut p = cwd.clone();
                if *selected < entries.len() {
                    let name = &entries[*selected].name;
                    if name != ".." { p = p.join(name); }
                }
                PathSpec::Remote{ host: host.clone(), port: *port, path: p }
            }
        },
        Focus::Right => match &app.right {
            Pane::Local { cwd, entries, selected } => {
                let mut p = cwd.clone();
                if *selected < entries.len() {
                    let name = &entries[*selected].name;
                    if name != ".." { p = p.join(name); }
                }
                PathSpec::Local(p)
            }
            Pane::Remote { host, port, cwd, entries, selected } => {
                let mut p = cwd.clone();
                if *selected < entries.len() {
                    let name = &entries[*selected].name;
                    if name != ".." { p = p.join(name); }
                }
                PathSpec::Remote{ host: host.clone(), port: *port, path: p }
            }
        }
    }
}

pub fn process_server_input(app: &mut super::app::AppState) {
    // Parse the input buffer for host:port
    let input = app.input_buffer.trim();

    // Parse host and port
    let (host, port) = if let Some(colon_pos) = input.rfind(':') {
        let host_part = &input[..colon_pos];
        let port_part = &input[colon_pos + 1..];

        if let Ok(p) = port_part.parse::<u16>() {
            (host_part.to_string(), p)
        } else {
            app.status = format!("Invalid port: {}", port_part);
            return;
        }
    } else {
        // No port specified, use default
        (input.to_string(), 9031)
    };

    // Initialize remote pane and trigger async load
    let cwd = PathBuf::from("/");
    app.right = Pane::Remote { host: host.clone(), port, cwd: cwd.clone(), entries: vec![], selected: 0 };
    app.status = format!("Connecting to {}:{}...", host, port);
    // Save recent host and trigger async directory listing
    super::options::add_recent_host(&mut app.options, &host, port);
    let _ = super::options::save_options(&app.options);
    request_remote_dir(app, super::app::Focus::Right, host, port, cwd);
    app.input_buffer.clear();
}

pub fn create_new_folder(app: &mut super::app::AppState) {
    let folder_name = app.input_buffer.trim();
    if folder_name.is_empty() {
        app.status = "Folder name cannot be empty".to_string();
        app.input_buffer.clear();
        return;
    }
    // Determine target pane (right pane is the destination)
    match &app.right {
        Pane::Local { cwd, .. } => {
            let new_folder_path = cwd.join(folder_name);
            match std::fs::create_dir(&new_folder_path) {
                Ok(_) => {
                    app.status = format!("Created folder: {}", folder_name);
                    if let Pane::Local { cwd, ref mut entries, .. } = &mut app.right {
                        *entries = read_local_dir(cwd);
                    }
                }
                Err(e) => {
                    app.status = format!("Failed to create folder: {}", e);
                }
            }
        }
        Pane::Remote { host, port, cwd, .. } => {
            let remote_path = cwd.join(folder_name);
            let exe = crate::resolve_blit_path();
            let remote_url = format!("blit://{}:{}{}", host, port, remote_path.display());
            if let Ok(temp_dir) = std::env::temp_dir().canonicalize() {
                let temp_path = temp_dir.join(format!("blit_mkdir_{}", std::process::id()));
                if std::fs::create_dir(&temp_path).is_ok() {
                    let mut cmd = std::process::Command::new(&exe);
                    let _ = cmd
                        .arg("copy")
                        .arg("--empty-dirs")
                        .arg(temp_path.to_string_lossy().as_ref())
                        .arg(&remote_url)
                        .output();
                    let _ = std::fs::remove_dir(&temp_path);
                    app.status = format!("Created remote folder: {}", folder_name);
                    let mut req: Option<(super::app::Focus, String, u16, PathBuf)> = None;
                    if let Pane::Remote { host, port, cwd, .. } = &app.right {
                        let pane_focus = if app.focus == super::app::Focus::Left { super::app::Focus::Left } else { super::app::Focus::Right };
                        req = Some((pane_focus, host.clone(), *port, cwd.clone()));
                    }
                    if let Some((pane_focus, h, p, c)) = req {
                        request_remote_dir(app, pane_focus, h, p, c);
                    }
                } else {
                    app.status = "Failed to create temporary directory".to_string();
                }
            } else {
                app.status = "Failed to get temp directory".to_string();
            }
        }
    }
    app.input_buffer.clear();
}

pub fn toggle_remote_right(app: &mut super::app::AppState) {
    // This function is now only called when toggling OFF remote
    // The R key in app.rs directly sets InputMode::ServerInput
    match &mut app.right {
        Pane::Remote { .. } => {
            let cwd = std::env::current_dir().unwrap_or(PathBuf::from("/"));
            app.right = Pane::Local { cwd: cwd.clone(), entries: read_local_dir(&cwd), selected: 0 };
        }
        Pane::Local { .. } => {
            // Do nothing - server input is handled by the input mode
        }
    }
}

pub fn toggle_help(app: &mut super::app::AppState) {
    app.help_visible = !app.help_visible;
    if app.help_visible {
        app.ui_mode = super::app::UiMode::Help;
    } else {
        app.ui_mode = super::app::UiMode::Normal;
    }
}

fn get_show_help(app: &AppState) -> bool { app.help_visible }

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2)
        ]).split(r);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2)
        ]).split(popup_layout[1]);
    horizontal[1]
}

fn mode_str(m: Mode) -> &'static str { match m { Mode::Mirror => "mirror", Mode::Copy => "copy", Mode::Move => "move" } }
fn path_short(p: Option<&PathSpec>) -> String {
    match p {
        None => "-".to_string(),
        Some(PathSpec::Local(pb)) => pb.display().to_string(),
        Some(PathSpec::Remote{host,port,path}) => {
            let mut pstr = path.display().to_string().replace('\\', "/");
            if !pstr.starts_with('/') { pstr = format!("/{}", pstr); }
            format!("{}:{}{}", host, port, pstr)
        },
    }
}

pub fn pathspec_to_string(p: &PathSpec) -> String {
    match p {
        PathSpec::Local(pb) => pb.display().to_string(),
        PathSpec::Remote{host,port,path} => {
            let mut s = String::new();
            s.push_str("blit://");
            s.push_str(host);
            s.push(':');
            s.push_str(&port.to_string());
            let mut pstr = path.display().to_string().replace("\\", "/");
            if !pstr.starts_with('/') { pstr = format!("/{}", pstr); }
            s.push_str(&pstr);
            s
        }


    }
}

#[cfg(windows)]
pub fn get_windows_drives() -> Vec<Entry> {
    let mut drives = Vec::new();
    
    // Add a back option to return to parent
    drives.push(Entry { name: "..".to_string(), is_dir: true, is_symlink: false });
    
    // Check all drive letters A-Z
    for letter in b'A'..=b'Z' {
        let drive_path = format!("{}:\\", letter as char);
        if std::path::Path::new(&drive_path).exists() {
            drives.push(Entry {
                name: format!("{}:", letter as char),
                is_dir: true,
                is_symlink: false,
            });
        }
    }
    
    // Also check for network drives that might be mapped
    // These would already be included above if mapped to a letter
    
    drives
}

#[cfg(not(windows))]
pub fn get_windows_drives() -> Vec<Entry> {
    // Not applicable on non-Windows systems
    vec![]
}

pub fn refresh_panes(app: &mut super::app::AppState) {
    // Refresh left pane
    match &mut app.left {
        Pane::Local { cwd, entries, .. } => {
            *entries = read_local_dir(cwd);
        }
        Pane::Remote { host, port, cwd, .. } => {
            let h = host.clone();
            let p = *port;
            let c = cwd.clone();
            request_remote_dir(app, Focus::Left, h, p, c);
        }
    }
    
    // Refresh right pane  
    match &mut app.right {
        Pane::Local { cwd, entries, .. } => {
            *entries = read_local_dir(cwd);
        }
        Pane::Remote { host, port, cwd, .. } => {
            let h = host.clone();
            let p = *port;
            let c = cwd.clone();
            request_remote_dir(app, Focus::Right, h, p, c);
        }
    }
}


pub fn pane_cwd(app: &AppState) -> PathSpec {
    match app.focus {
        Focus::Left => match &app.left {
            Pane::Local { cwd, .. } => PathSpec::Local(cwd.clone()),
            Pane::Remote { host, port, cwd, .. } => PathSpec::Remote { host: host.clone(), port: *port, path: cwd.clone() },
        },
        Focus::Right => match &app.right {
            Pane::Local { cwd, .. } => PathSpec::Local(cwd.clone()),
            Pane::Remote { host, port, cwd, .. } => PathSpec::Remote { host: host.clone(), port: *port, path: cwd.clone() },
        },
    }
}


pub fn go_up(app: &mut super::app::AppState) {
    // Extract the info we need before borrowing
    let (is_remote, host_opt, port_opt, parent_path) = {
        let pane = if app.focus == Focus::Left { &app.left } else { &app.right };
        match pane {
            Pane::Local { cwd, .. } => {
                (false, None, None, cwd.parent().map(|p| p.to_path_buf()))
            }
            Pane::Remote { cwd, host, port, .. } => {
                (true, Some(host.clone()), Some(*port), cwd.parent().map(|p| p.to_path_buf()))
            }
        }
    };
    
    if let Some(parent) = parent_path {
        // Now we can mutate without issues
        let pane = if app.focus == Focus::Left { &mut app.left } else { &mut app.right };
        match pane {
            Pane::Local { cwd, entries, selected } => {
                *cwd = parent;
                *entries = read_local_dir(cwd);
                *selected = 0;
            }
            Pane::Remote { cwd, selected, .. } => {
                *cwd = parent.clone();
                *selected = 0;
                if let (Some(h), Some(p)) = (host_opt, port_opt) {
                    request_remote_dir(app, app.focus.clone(), h, p, parent);
                }
            }
        }
    }
}

pub fn swap_panes(app: &mut super::app::AppState) {
    let left = std::mem::replace(&mut app.left, Pane::Local { cwd: PathBuf::new(), entries: vec![], selected: 0 });
    let right = std::mem::replace(&mut app.right, Pane::Local { cwd: PathBuf::new(), entries: vec![], selected: 0 });
    app.left = right; app.right = left;
    app.focus = if app.focus == Focus::Left { Focus::Right } else { Focus::Left };
}
