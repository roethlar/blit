use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

use super::options;
use super::ui;
use blit::url::RemoteDest;

/// Terminal guard that ensures proper cleanup on drop
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Best-effort terminal restoration
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = io::stdout().flush();
    }
}

/// Messages for async UI communication
#[derive(Clone)]
pub enum UiMsg {
    RemoteEntries {
        pane: Focus,
        entries: Vec<ui::Entry>,
    },
    Error(String),
    Toast(String),
    TransferComplete {
        success: bool,
        message: String,
    },
    Loading {
        pane: Focus,
    },
    Discovery(Vec<DiscoveredHost>),
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    Mirror,
    Copy,
    Move,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Focus {
    Left,
    Right,
}

#[derive(Clone, Copy, PartialEq)]
pub enum UiMode {
    Normal,
    Help,
    ServerInput,
    NewFolderInput,
    Options,
    Busy,
    ConfirmMove,
    ConfirmTransfer, // SAFETY: Require explicit confirmation for transfers (Y/N)
    ConfirmTyped,    // SAFETY: Require typing a keyword (e.g., "delete" or "move")
    TextInput,       // Generic text entry for Options editing
}

#[derive(Clone)]
pub enum Pane {
    Local {
        cwd: PathBuf,
        entries: Vec<ui::Entry>,
        selected: usize,
    },
    Remote {
        host: String,
        port: u16,
        cwd: PathBuf,
        entries: Vec<ui::Entry>,
        selected: usize,
    },
}

pub struct AppState {
    pub left: Pane,
    pub right: Pane,
    pub focus: Focus,
    pub mode: Mode,
    pub tar_small: bool,
    pub delta_large: bool,
    pub include_empty: bool,
    pub checksum: bool,
    pub src: Option<ui::PathSpec>,
    pub dest: Option<ui::PathSpec>,
    pub status: String,
    pub log: std::collections::VecDeque<String>,
    pub log_scroll: usize, // 0 = bottom; increases when user scrolls up
    pub log_follow: bool,  // auto-follow when true
    pub running: bool,
    pub spinner_idx: usize,
    pub rx: Option<std::sync::mpsc::Receiver<String>>,
    pub child: Option<std::sync::Arc<std::sync::Mutex<Option<std::process::Child>>>>,
    pub input_buffer: String,
    pub ui_mode: UiMode,
    pub help_visible: bool,
    pub error: Option<String>,
    pub toast: Option<(String, std::time::Instant)>,
    pub tx_ui: Sender<UiMsg>,
    pub rx_ui: Receiver<UiMsg>,
    pub loading_pane: Option<Focus>,
    pub options: options::OptionsState,
    pub options_cursor: usize,
    pub options_tab: usize,
    pub pending_args: Option<Vec<String>>, // Prepared blit argv for execution
    pub confirm_required_input: Option<String>, // Keyword required to confirm (delete/move)
    pub confirm_input: String,             // User-typed confirmation buffer
    pub input_kind: Option<InputKind>,     // What TextInput is editing
    pub show_advanced: bool,               // Reveal advanced/unsafe toggles in Options
    pub theme_name: String,                // Current theme name (Dracula, SolarizedDark, Gruvbox)
    pub discovered: Vec<DiscoveredHost>,   // mDNS discovered hosts
}

impl AppState {
    fn new(remote: Option<RemoteDest>) -> Self {
        let left_cwd = get_initial_directory();
        let left_entries = ui::read_local_dir(&left_cwd);
        let right = if let Some(r) = remote {
            Pane::Remote {
                host: r.host,
                port: r.port,
                cwd: r.path,
                entries: Vec::new(),
                selected: 0,
            }
        } else {
            let cwd = left_cwd.clone();
            Pane::Local {
                cwd,
                entries: ui::read_local_dir(&left_cwd),
                selected: 0,
            }
        };
        let (tx_ui, rx_ui) = channel();
        Self {
            left: Pane::Local {
                cwd: left_cwd,
                entries: left_entries,
                selected: 0,
            },
            right,
            focus: Focus::Left,
            mode: Mode::Copy,
            tar_small: true,
            delta_large: true,
            include_empty: true,
            checksum: false,
            src: None,
            dest: None,
            status: String::new(),
            log: std::collections::VecDeque::with_capacity(256),
            log_scroll: 0,
            log_follow: true,
            running: false,
            spinner_idx: 0,
            rx: None,
            child: None,
            input_buffer: String::new(),
            ui_mode: UiMode::Normal,
            help_visible: false,
            error: None,
            toast: None,
            tx_ui,
            rx_ui,
            loading_pane: None,
            options: options::OptionsState::default(),
            options_cursor: 0,
            options_tab: 0,
            pending_args: None,
            confirm_required_input: None,
            confirm_input: String::new(),
            input_kind: None,
            show_advanced: false,
            theme_name: "Dracula".to_string(),
            discovered: Vec::new(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct DiscoveredHost {
    pub name: String,
    pub host: String,
    pub port: u16,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    AddExcludeFile,
    AddExcludeDir,
    SetLogFile,
}

pub fn run(remote: Option<RemoteDest>) -> Result<()> {
    // Install panic hook to restore terminal on panic
    let original_panic = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Restore terminal state
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = io::stdout().flush();
        // Call original panic handler
        original_panic(info);
    }));

    // Create terminal guard for cleanup on normal || error exit
    let _guard = TerminalGuard;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = AppState::new(remote);
    // Load persisted options (best-effort)
    app.options =
        options::load_options().unwrap_or_else(|_| options::OptionsState::with_safe_defaults());
    // Unsafe mode is CLI-only; do not expose in UI. Read from env set by blitty.rs.
    if std::env::var("BLIT_UNSAFE").ok().as_deref() == Some("1") {
        app.options.never_tell_me_the_odds = true;
    }
    // Sync initial mode from persisted options if present
    match app.options.mode.as_str() {
        "mirror" => app.mode = Mode::Mirror,
        "move" => app.mode = Mode::Move,
        _ => app.mode = Mode::Copy,
    }

    // initial remote load if needed (async helper)
    let mut init_remote: Option<(String, u16, std::path::PathBuf)> = None;
    if let Pane::Remote {
        ref host,
        port,
        ref cwd,
        ..
    } = app.right
    {
        init_remote = Some((host.clone(), port, cwd.clone()));
        app.loading_pane = Some(Focus::Right);
    }
    if let Some((h, p, c)) = init_remote {
        ui::request_remote_dir(&mut app, Focus::Right, h, p, c);
    }

    // Start mDNS discovery in background
    {
        let tx_ui = app.tx_ui.clone();
        std::thread::spawn(move || {
            use mdns_sd::{ServiceDaemon, ServiceEvent};
            let mdns = match ServiceDaemon::new() {
                Ok(d) => d,
                Err(_) => return,
            };
            let ty = "_blit._tcp.local.";
            let receiver = match mdns.browse(ty) {
                Ok(r) => r,
                Err(_) => return,
            };
            use std::collections::HashMap;
            let mut map: HashMap<String, (String, u16)> = HashMap::new();
            loop {
                match receiver.recv() {
                    Ok(ServiceEvent::ServiceResolved(info)) => {
                        let addrs = info.get_addresses();
                        let host = addrs
                            .iter()
                            .next()
                            .map(|a| a.to_string())
                            .unwrap_or_default();
                        let port = info.get_port();
                        let name = info.get_fullname().to_string();
                        map.insert(name.clone(), (host, port));
                        let mut out = Vec::new();
                        for (n, (h, p)) in map.iter() {
                            out.push(DiscoveredHost {
                                name: n.clone(),
                                host: h.clone(),
                                port: *p,
                            });
                        }
                        let _ = tx_ui.send(UiMsg::Discovery(out));
                    }
                    Ok(ServiceEvent::ServiceRemoved(_, service_name)) => {
                        map.retain(|k, _| *k != service_name);
                        let mut out = Vec::new();
                        for (n, (h, p)) in map.iter() {
                            out.push(DiscoveredHost {
                                name: n.clone(),
                                host: h.clone(),
                                port: *p,
                            });
                        }
                        let _ = tx_ui.send(UiMsg::Discovery(out));
                    }
                    _ => {}
                }
            }
        });
    }

    loop {
        // Process UI messages from background tasks
        let mut needs_refresh = false;
        while let Ok(msg) = app.rx_ui.try_recv() {
            match msg {
                UiMsg::RemoteEntries {
                    pane,
                    entries: new_entries,
                } => {
                    match pane {
                        Focus::Left => {
                            if let Pane::Remote { entries, .. } = &mut app.left {
                                *entries = new_entries.clone();
                            }
                        }
                        Focus::Right => {
                            if let Pane::Remote { entries, .. } = &mut app.right {
                                *entries = new_entries;
                            }
                        }
                    }
                    app.loading_pane = None;
                }
                UiMsg::Error(err) => {
                    app.error = Some(err);
                    app.loading_pane = None;
                }
                UiMsg::Toast(msg) => {
                    app.toast = Some((msg, std::time::Instant::now()));
                }
                UiMsg::TransferComplete { success, message } => {
                    app.running = false;
                    app.child = None; // Clear child handle
                    if success {
                        let icon = if ui::is_ascii_mode() { "[OK]" } else { "✓" };
                        app.status = format!("{} {}", icon, message);
                        app.toast = Some((
                            format!("{} Transfer successful!", icon),
                            std::time::Instant::now(),
                        ));
                        needs_refresh = true; // Mark that we need to refresh after the match
                    } else {
                        let icon = if ui::is_ascii_mode() { "[FAIL]" } else { "✗" };
                        app.status = format!("{} {}", icon, message);
                        app.error = Some(message.clone());
                        app.toast =
                            Some((format!("{} {}", icon, message), std::time::Instant::now()));
                    }
                }
                UiMsg::Loading { pane } => {
                    app.loading_pane = Some(pane);
                }
                UiMsg::Discovery(list) => {
                    app.discovered = list;
                }
            }
        }

        // Refresh panes if needed (after successful transfer)
        if needs_refresh {
            ui::refresh_panes(&mut app);
        }

        // Drain any output from background transfer
        if let Some(rx) = &app.rx {
            while let Ok(line) = rx.try_recv() {
                if app.log.len() >= 2000 {
                    let _ = app.log.pop_front();
                } // grow to 2k lines ring buffer
                app.log.push_back(line);
                if app.log_follow {
                    app.log_scroll = 0;
                }
            }
        }
        if app.running {
            app.spinner_idx = (app.spinner_idx + 1) % 10;
        }

        // Clear old toasts (after 3 seconds)
        if let Some((_, instant)) = &app.toast {
            if instant.elapsed() > std::time::Duration::from_secs(3) {
                app.toast = None;
            }
        }

        terminal.draw(|f| ui::draw(f, &app))?;

        if event::poll(std::time::Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(k) => {
                    let code = k.code;
                    let modifiers = k.modifiers;

                    // Handle Ctrl+C for emergency exit from any mode
                    if let KeyCode::Char('c') = code {
                        if modifiers.contains(KeyModifiers::CONTROL) {
                            eprintln!("Emergency exit (Ctrl+C)");
                            break;
                        }
                    }
                    if app.ui_mode == UiMode::ServerInput {
                        // Handle server input mode - only accept text input keys
                        match code {
                            KeyCode::Enter => {
                                ui::process_server_input(&mut app);
                                app.ui_mode = UiMode::Normal;
                            }
                            KeyCode::Esc => {
                                app.input_buffer.clear();
                                app.ui_mode = UiMode::Normal;
                            }
                            KeyCode::Backspace => {
                                app.input_buffer.pop();
                            }
                            KeyCode::Char(c) => {
                                app.input_buffer.push(c);
                            }
                            // IGNORE navigation keys in input mode
                            KeyCode::Up
                            | KeyCode::Down
                            | KeyCode::Left
                            | KeyCode::Right
                            | KeyCode::Tab => {
                                // Ignore navigation keys during text input
                            }
                            _ => {
                                // Ignore all other keys in input mode
                            }
                        }
                    } else if app.ui_mode == UiMode::ConfirmTransfer {
                        // SAFETY: Confirmation dialog mode - only Y/N/Esc allowed
                        eprintln!("DEBUG: In ConfirmTransfer mode, got key: {:?}", code);
                        match code {
                            KeyCode::Char('y') | KeyCode::Char('Y') => {
                                eprintln!("DEBUG: Y pressed, executing transfer");
                                app.ui_mode = UiMode::Normal;
                                if !app.running {
                                    start_transfer(&mut app);
                                }
                            }
                            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                                eprintln!("DEBUG: N/Esc pressed, cancelling transfer");
                                app.ui_mode = UiMode::Normal;
                                app.status = "Transfer cancelled".to_string();
                            }
                            _ => {
                                eprintln!("DEBUG: ConfirmTransfer ignored key: {:?}", code);
                                // SAFETY: Ignore all other keys in confirmation mode
                            }
                        }
                    } else if app.ui_mode == UiMode::ConfirmTyped {
                        match code {
                            KeyCode::Enter => {
                                if let Some(req) = &app.confirm_required_input {
                                    if app.confirm_input.trim().eq_ignore_ascii_case(req) {
                                        app.ui_mode = UiMode::Normal;
                                        if !app.running {
                                            start_transfer(&mut app);
                                        }
                                    } else {
                                        app.status = format!(
                                            "Confirmation text must be '{}'; cancelled",
                                            req
                                        );
                                        app.ui_mode = UiMode::Normal;
                                    }
                                } else {
                                    app.ui_mode = UiMode::Normal;
                                }
                                app.confirm_input.clear();
                            }
                            KeyCode::Esc => {
                                app.confirm_input.clear();
                                app.ui_mode = UiMode::Normal;
                            }
                            KeyCode::Backspace => {
                                app.confirm_input.pop();
                            }
                            KeyCode::Char(c) => {
                                app.confirm_input.push(c);
                            }
                            _ => {}
                        }
                    } else if app.ui_mode == UiMode::NewFolderInput {
                        // Handle new folder input mode
                        match code {
                            KeyCode::Enter => {
                                ui::create_new_folder(&mut app);
                                app.ui_mode = UiMode::Normal;
                            }
                            KeyCode::Esc => {
                                app.input_buffer.clear();
                                app.ui_mode = UiMode::Normal;
                            }
                            KeyCode::Backspace => {
                                app.input_buffer.pop();
                            }
                            KeyCode::Char(c) => {
                                app.input_buffer.push(c);
                            }
                            _ => {}
                        }
                    } else if app.ui_mode == UiMode::Options {
                        match code {
                            KeyCode::Esc => {
                                let _ = options::save_options(&app.options);
                                app.ui_mode = UiMode::Normal;
                            }
                            KeyCode::Left if modifiers.contains(KeyModifiers::CONTROL) => {
                                if app.options_tab > 0 {
                                    app.options_tab -= 1;
                                    app.options_cursor = 0;
                                }
                            }
                            KeyCode::Right if modifiers.contains(KeyModifiers::CONTROL) => {
                                if app.options_tab < 7 {
                                    app.options_tab += 1;
                                    app.options_cursor = 0;
                                }
                            }
                            KeyCode::Char('a') if modifiers.contains(KeyModifiers::CONTROL) => {
                                app.show_advanced = !app.show_advanced;
                            }
                            KeyCode::Up => {
                                if app.options_cursor > 0 {
                                    app.options_cursor -= 1;
                                }
                            }
                            KeyCode::Down => {
                                // Determine max rows by peeking at current UI items length via logical index hack not possible; clamp later on render transitions.
                                app.options_cursor = app.options_cursor.saturating_add(1);
                            }
                            KeyCode::Enter | KeyCode::Char(' ') => {
                                let logical = super::ui::current_options_logical_index();
                                match logical {
                                    50 => {
                                        // Cycle mode Copy -> Mirror -> Move -> Copy
                                        app.mode = match app.mode {
                                            Mode::Copy => Mode::Mirror,
                                            Mode::Mirror => Mode::Move,
                                            Mode::Move => Mode::Copy,
                                        };
                                        // Persist to options snapshot
                                        app.options.mode = match app.mode {
                                            Mode::Copy => "copy",
                                            Mode::Mirror => "mirror",
                                            Mode::Move => "move",
                                        }
                                        .into();
                                    }
                                    220 => {
                                        app.input_buffer.clear();
                                        app.input_kind = Some(InputKind::AddExcludeFile);
                                        app.ui_mode = UiMode::TextInput;
                                    }
                                    221 => {
                                        app.input_buffer.clear();
                                        app.input_kind = Some(InputKind::AddExcludeDir);
                                        app.ui_mode = UiMode::TextInput;
                                    }
                                    230 => {
                                        app.input_buffer.clear();
                                        app.input_kind = Some(InputKind::SetLogFile);
                                        app.ui_mode = UiMode::TextInput;
                                    }
                                    v if (300..400).contains(&v) => {
                                        // Connect to recent host
                                        let idx = v - 300;
                                        if let Some(hc) = app.options.recent_hosts.get(idx).cloned()
                                        {
                                            let cwd = std::path::PathBuf::from("/");
                                            let host = hc.host;
                                            let port = hc.port;
                                            app.right = Pane::Remote {
                                                host: host.clone(),
                                                port,
                                                cwd: cwd.clone(),
                                                entries: vec![],
                                                selected: 0,
                                            };
                                            super::ui::request_remote_dir(
                                                &mut app,
                                                Focus::Right,
                                                host.clone(),
                                                port,
                                                cwd,
                                            );
                                            app.status =
                                                format!("Connecting to {}:{}...", host, port);
                                            let _ = options::save_options(&app.options);
                                            app.ui_mode = UiMode::Normal;
                                        }
                                    }
                                    v if (400..500).contains(&v) => {
                                        // Connect to discovered host
                                        let idx = v - 400;
                                        if let Some(d) = app.discovered.get(idx).cloned() {
                                            let cwd = std::path::PathBuf::from("/");
                                            let host = d.host;
                                            let port = d.port;
                                            app.right = Pane::Remote {
                                                host: host.clone(),
                                                port,
                                                cwd: cwd.clone(),
                                                entries: vec![],
                                                selected: 0,
                                            };
                                            super::ui::request_remote_dir(
                                                &mut app,
                                                Focus::Right,
                                                host.clone(),
                                                port,
                                                cwd,
                                            );
                                            app.status =
                                                format!("Connecting to {}:{}...", host, port);
                                            // stash into recents
                                            options::add_recent_host(&mut app.options, &host, port);
                                            let _ = options::save_options(&app.options);
                                            app.ui_mode = UiMode::Normal;
                                        }
                                    }
                                    500 => {
                                        app.theme_name = "Dracula".to_string();
                                    }
                                    501 => {
                                        app.theme_name = "SolarizedDark".to_string();
                                    }
                                    502 => {
                                        app.theme_name = "Gruvbox".to_string();
                                    }
                                    _ => {
                                        options::toggle_option(&mut app.options, logical);
                                    }
                                }
                            }
                            KeyCode::Left => {
                                let logical = super::ui::current_options_logical_index();
                                if logical == 50 {
                                    app.mode = match app.mode {
                                        Mode::Copy => Mode::Move,
                                        Mode::Mirror => Mode::Copy,
                                        Mode::Move => Mode::Mirror,
                                    };
                                    app.options.mode = match app.mode {
                                        Mode::Copy => "copy",
                                        Mode::Mirror => "mirror",
                                        Mode::Move => "move",
                                    }
                                    .into();
                                } else {
                                    options::adjust_option(&mut app.options, logical, -1);
                                }
                            }
                            KeyCode::Right => {
                                let logical = super::ui::current_options_logical_index();
                                if logical == 50 {
                                    app.mode = match app.mode {
                                        Mode::Copy => Mode::Mirror,
                                        Mode::Mirror => Mode::Move,
                                        Mode::Move => Mode::Copy,
                                    };
                                    app.options.mode = match app.mode {
                                        Mode::Copy => "copy",
                                        Mode::Mirror => "mirror",
                                        Mode::Move => "move",
                                    }
                                    .into();
                                } else {
                                    options::adjust_option(&mut app.options, logical, 1);
                                }
                            }
                            KeyCode::Backspace => {
                                match super::ui::current_options_logical_index() {
                                    100 => app.options.threads = 0,
                                    101 => app.options.net_workers = 0,
                                    102 => app.options.net_chunk_mb = 0,
                                    _ => {}
                                }
                            }
                            KeyCode::Delete => {
                                // Remove selected filter on Filters tab, or clear log file on Logging tab
                                if app.options_tab == 3 {
                                    match super::ui::current_options_logical_index() {
                                        220 => {
                                            let _ = app.options.exclude_files.pop();
                                        }
                                        221 => {
                                            let _ = app.options.exclude_dirs.pop();
                                        }
                                        v if (240..300).contains(&v) => {
                                            // 240.. = files, 260.. = dirs
                                            if v >= 260 {
                                                let idx = v - 260;
                                                if idx < app.options.exclude_dirs.len() {
                                                    let _ = app.options.exclude_dirs.remove(idx);
                                                }
                                            } else if v >= 240 {
                                                let idx = v - 240;
                                                if idx < app.options.exclude_files.len() {
                                                    let _ = app.options.exclude_files.remove(idx);
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                } else if app.options_tab == 5
                                    && super::ui::current_options_logical_index() == 230
                                {
                                    app.options.log_file = None;
                                }
                            }
                            _ => {}
                        }
                    } else if app.ui_mode == UiMode::TextInput {
                        match code {
                            KeyCode::Enter => {
                                let text = app.input_buffer.trim().to_string();
                                match app.input_kind {
                                    Some(InputKind::AddExcludeFile) => {
                                        if !text.is_empty() {
                                            app.options.exclude_files.push(text);
                                        }
                                    }
                                    Some(InputKind::AddExcludeDir) => {
                                        if !text.is_empty() {
                                            app.options.exclude_dirs.push(text);
                                        }
                                    }
                                    Some(InputKind::SetLogFile) => {
                                        if !text.is_empty() {
                                            app.options.log_file =
                                                Some(std::path::PathBuf::from(text));
                                        }
                                    }
                                    None => {}
                                }
                                let _ = options::save_options(&app.options);
                                app.input_buffer.clear();
                                app.input_kind = None;
                                app.ui_mode = UiMode::Options;
                            }
                            KeyCode::Esc => {
                                app.input_buffer.clear();
                                app.input_kind = None;
                                app.ui_mode = UiMode::Options;
                            }
                            KeyCode::Backspace => {
                                app.input_buffer.pop();
                            }
                            KeyCode::Char(c) => {
                                app.input_buffer.push(c);
                            }
                            _ => {}
                        }
                    } else {
                        // Normal mode
                        match (code, modifiers) {
                            (KeyCode::PageUp, _) => {
                                if app.log_scroll < app.log.len() {
                                    app.log_scroll = app.log_scroll.saturating_add(5);
                                    app.log_follow = false;
                                }
                            }
                            (KeyCode::PageDown, _) => {
                                app.log_scroll = app.log_scroll.saturating_sub(5);
                                if app.log_scroll == 0 {
                                    app.log_follow = true;
                                }
                            }
                            (KeyCode::Home, _) => {
                                app.log_scroll = app.log.len();
                                app.log_follow = false;
                            }
                            (KeyCode::End, _) => {
                                app.log_scroll = 0;
                                app.log_follow = true;
                            }
                            (KeyCode::Char('q'), _) => {
                                if app.running {
                                    cancel_transfer(&mut app);
                                    std::thread::sleep(std::time::Duration::from_millis(100));
                                }
                                break;
                            }
                            // Pane switch
                            (KeyCode::Tab, _)
                            | (KeyCode::BackTab, _)
                            | (KeyCode::Char('\t'), _) => {
                                app.ui_mode = UiMode::Normal;
                                app.help_visible = false;
                                app.error = None;
                                app.focus = if app.focus == Focus::Left {
                                    Focus::Right
                                } else {
                                    Focus::Left
                                };
                            }
                            (KeyCode::Left, m) if m.contains(KeyModifiers::ALT) => {
                                app.ui_mode = UiMode::Normal;
                                app.help_visible = false;
                                app.error = None;
                                app.focus = Focus::Left;
                            }
                            (KeyCode::Right, m) if m.contains(KeyModifiers::ALT) => {
                                app.ui_mode = UiMode::Normal;
                                app.help_visible = false;
                                app.error = None;
                                app.focus = Focus::Right;
                            }
                            // Navigation
                            (KeyCode::Up, _) => {
                                ui::move_up(&mut app);
                            }
                            (KeyCode::Down, _) => {
                                ui::move_down(&mut app);
                            }
                            // Space selects current item for current pane role
                            (KeyCode::Char(' '), _) => match app.focus {
                                Focus::Left => {
                                    app.src = Some(ui::current_path(&app));
                                    app.status = "Source selected".to_string();
                                }
                                Focus::Right => {
                                    app.dest = Some(ui::current_path(&app));
                                    app.status = "Target selected".to_string();
                                }
                            },
                            // SAFETY: Enter now only navigates directories - NO MORE IMMEDIATE EXECUTION
                            (KeyCode::Enter, _) => {
                                ui::enter(&mut app);
                            }
                            // Cancel/back: abort transfer if running, otherwise go up one directory
                            (KeyCode::Esc, _) => {
                                if app.running {
                                    cancel_transfer(&mut app);
                                } else {
                                    ui::go_up(&mut app);
                                }
                            }
                            // Swap panes
                            (KeyCode::Backspace, _) => {
                                ui::swap_panes(&mut app);
                            }
                            // Connection dialog
                            (KeyCode::F(2), _) => {
                                app.ui_mode = UiMode::ServerInput;
                            }
                            // Options
                            (KeyCode::Char('o'), _) | (KeyCode::Char('O'), _) => {
                                app.ui_mode = UiMode::Options;
                            }
                            // Theme selector (cycle)
                            (KeyCode::F(4), _) => {
                                fn next_theme(cur: &str) -> &'static str {
                                    match cur {
                                        "Dracula" => "SolarizedDark",
                                        "SolarizedDark" => "Gruvbox",
                                        _ => "Dracula",
                                    }
                                }
                                app.theme_name = next_theme(&app.theme_name).to_string();
                                super::theme::set_theme(&app.theme_name);
                                app.toast = Some((
                                    format!("Theme: {}", app.theme_name),
                                    std::time::Instant::now(),
                                ));
                            }
                            // Help
                            (KeyCode::Char('h'), _) | (KeyCode::F(1), _) => {
                                ui::toggle_help(&mut app);
                            }
                            // Ctrl+G prepares command and initiates confirmation
                            (KeyCode::Char('g'), m) if m.contains(KeyModifiers::CONTROL) => {
                                if app.src.is_some() && app.dest.is_some() && !app.running {
                                    // Build argv using options
                                    let (src, dest) =
                                        (app.src.clone().unwrap(), app.dest.clone().unwrap());
                                    let argv = options::build_blit_args(
                                        app.mode,
                                        &app.options,
                                        &src,
                                        &dest,
                                    );
                                    app.pending_args = Some(argv);
                                    // Determine if typed confirmation is required
                                    if matches!(app.mode, Mode::Mirror) {
                                        app.confirm_required_input = Some("delete".to_string());
                                        app.ui_mode = UiMode::ConfirmTyped;
                                        app.status = "Type 'delete' to confirm mirror deletions, or Esc to cancel".to_string();
                                    } else if matches!(app.mode, Mode::Move) {
                                        app.confirm_required_input = Some("move".to_string());
                                        app.ui_mode = UiMode::ConfirmTyped;
                                        app.status = "Type 'move' to confirm move (source removal), or Esc to cancel".to_string();
                                    } else {
                                        app.confirm_required_input = None;
                                        app.ui_mode = UiMode::ConfirmTransfer;
                                        app.status =
                                            "Press Y to confirm transfer, or Esc to cancel"
                                                .to_string();
                                    }
                                } else if app.src.is_none() || app.dest.is_none() {
                                    app.status = "Select source (Space in left pane) and destination (Space in right pane) first".to_string();
                                } else if app.running {
                                    app.status = "Transfer already in progress".to_string();
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Terminal cleanup handled by TerminalGuard
    terminal.show_cursor()?;
    Ok(())
}

fn start_transfer(app: &mut AppState) {
    // Validate src/dest
    let (src, dest) = match (&app.src, &app.dest) {
        (Some(s), Some(d)) => (s.clone(), d.clone()),
        _ => {
            app.status = "Press Space to set Source/Target, then Enter".to_string();
            return;
        }
    };
    // Guard dangerous roots (local only)
    if let (Mode::Move, ui::PathSpec::Local(p)) = (app.mode, &src) {
        if is_fs_root(p) {
            app.status = "Refusing to move a filesystem root".to_string();
            return;
        }
    }
    if let (Mode::Mirror, ui::PathSpec::Local(p)) = (app.mode, &dest) {
        if is_fs_root(p) {
            app.status = "Refusing to mirror into filesystem root".to_string();
            return;
        }
    }

    // Build argv from options (reuse prepared if present)
    let argv = if let Some(a) = app.pending_args.take() {
        a
    } else {
        super::options::build_blit_args(app.mode, &app.options, &src, &dest)
    };

    // Build command
    let exe = crate::resolve_blit_path();
    let mut cmd = std::process::Command::new(&exe);
    for a in &argv {
        cmd.arg(a);
    }
    // Spawn and capture output
    let mut child = match cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            app.status = format!("Failed to start: {}", e);
            return;
        }
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    // Preview
    let mut preview = format!("[cmd] {}", exe.display());
    for a in &argv {
        preview.push(' ');
        if a.contains(' ') {
            preview.push('"');
            preview.push_str(a);
            preview.push('"');
        } else {
            preview.push_str(a);
        }
    }
    let _ = tx.send(preview);
    app.rx = Some(rx);
    app.running = true;
    app.status = "Running transfer…".to_string();
    let handle = std::sync::Arc::new(std::sync::Mutex::new(Some(child)));
    app.child = Some(handle.clone());
    // Reader helper
    let spawn_reader = |r: std::process::ChildStdout, txc: std::sync::mpsc::Sender<String>| {
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            let br = BufReader::new(r);
            for line in br.lines().flatten() {
                let _ = txc.send(line);
            }
        });
    };
    if let Some(out) = stdout {
        let txc = tx.clone();
        spawn_reader(out, txc);
    }
    if let Some(err) = stderr {
        let txc = tx.clone();
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            let br = BufReader::new(err);
            for line in br.lines().flatten() {
                let _ = txc.send(format!("[err] {}", line));
            }
        });
    }
    // Waiter thread to capture exit status
    let tx_ui = app.tx_ui.clone();
    std::thread::spawn(move || {
        let mut exit_success = false;
        let mut exit_message = String::new();

        // Handle poisoned lock gracefully - if another thread panicked while holding the lock,
        // we still want to try to wait on the child process
        if let Ok(mut guard) = handle.lock() {
            if let Some(mut ch) = guard.take() {
                match ch.wait() {
                    Ok(status) => {
                        exit_success = status.success();
                        if exit_success {
                            exit_message = "Transfer completed successfully".to_string();
                        } else {
                            exit_message = format!(
                                "Transfer failed with exit code: {}",
                                status.code().unwrap_or(-1)
                            );
                        }
                    }
                    Err(e) => {
                        exit_message = format!("Failed to wait for process: {}", e);
                    }
                }
            }
        } else {
            exit_message = "Internal error: lock poisoned".to_string();
        }

        // Send completion message through UI channel
        let _ = tx_ui.send(UiMsg::TransferComplete {
            success: exit_success,
            message: exit_message,
        });
        let _ = tx.send("__DONE__".to_string());
    });
}

fn get_initial_directory() -> PathBuf {
    // Get the current directory, handling Windows network drives properly
    match std::env::current_dir() {
        Ok(dir) => {
            #[cfg(windows)]
            {
                // Convert UNC paths to mapped drive letters if available
                let dir_str = dir.to_string_lossy();
                if dir_str.starts_with("\\\\") {
                    // This is a UNC path - try to find if it's mapped to a drive letter
                    // For now, just use it as is, but show the drive letters in the UI
                    return dir;
                }
            }
            dir
        }
        Err(_) => {
            // Fallback to root || drives on Windows
            #[cfg(windows)]
            return PathBuf::from("\\\\?\\drives"); // Special marker for drive selection
            #[cfg(not(windows))]
            return PathBuf::from("/");
        }
    }
}

fn cancel_transfer(app: &mut AppState) {
    if let Some(h) = &app.child {
        // Handle poisoned lock gracefully - if another thread panicked,
        // we still want to attempt to kill the child process
        if let Ok(mut guard) = h.lock() {
            if let Some(mut ch) = guard.take() {
                let _ = ch.kill();
            }
        }
    }
    app.child = None;
    app.running = false;
    let icon = if ui::is_ascii_mode() { "[X]" } else { "⛔" };
    app.status = format!("{} Transfer canceled", icon);
    app.toast = Some((
        format!("{} Transfer canceled by user", icon),
        std::time::Instant::now(),
    ));
}

fn is_fs_root(p: &std::path::Path) -> bool {
    match p.canonicalize() {
        Ok(cp) => cp.parent().is_none(),
        Err(_) => false,
    }
}
