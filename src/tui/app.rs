#![cfg(feature = "tui")]

use anyhow::Result;
use ratatui::{
    backend::CrosstermBackend,
    Terminal,
};
    use crossterm::{
        execute,
        terminal::{enable_raw_mode, disable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
        event::{self, Event, KeyCode, KeyEvent, KeyModifiers, KeyEventKind},
    };
use std::io::{self, Write};
use std::path::{PathBuf};
use std::sync::mpsc::{Sender, Receiver, channel};

use crate::url::{RemoteDest};
use super::ui;

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
    RemoteEntries { pane: Focus, entries: Vec<ui::Entry> },
    Error(String),
    Toast(String),
    TransferComplete { success: bool, message: String },
    Loading { pane: Focus },
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mode { Mirror, Copy, Move }

#[derive(Clone, Copy, PartialEq)]
pub enum Focus { Left, Right }

#[derive(Clone, Copy, PartialEq)]
pub enum UiMode { 
    Normal, 
    Help,
    ServerInput,
    NewFolderInput,
    Busy,
    ConfirmMove,
}

#[derive(Clone)]
pub enum Pane {
    Local { cwd: PathBuf, entries: Vec<ui::Entry>, selected: usize },
    Remote { host: String, port: u16, cwd: PathBuf, entries: Vec<ui::Entry>, selected: usize },
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
}

impl AppState {
    fn new(remote: Option<RemoteDest>) -> Self {
        let left_cwd = get_initial_directory();
        let left_entries = ui::read_local_dir(&left_cwd);
        let right = if let Some(r) = remote {
            Pane::Remote { host: r.host, port: r.port, cwd: r.path, entries: Vec::new(), selected: 0 }
        } else {
            let cwd = left_cwd.clone();
            Pane::Local { cwd, entries: ui::read_local_dir(&left_cwd), selected: 0 }
        };
        let (tx_ui, rx_ui) = channel();
        Self {
            left: Pane::Local { cwd: left_cwd, entries: left_entries, selected: 0 },
            right,
            focus: Focus::Left,
            mode: Mode::Mirror,
            tar_small: true,
            delta_large: true,
            include_empty: true,
            checksum: false,
            src: None,
            dest: None,
            status: String::new(),
            log: std::collections::VecDeque::with_capacity(256),
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
        }
    }
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
    
    // Create terminal guard for cleanup on normal or error exit
    let _guard = TerminalGuard;
    
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = AppState::new(remote);

    // initial remote load if needed (async helper)
    let mut init_remote: Option<(String, u16, std::path::PathBuf)> = None;
    if let Pane::Remote { ref host, port, ref cwd, .. } = app.right {
        init_remote = Some((host.clone(), port, cwd.clone()));
        app.loading_pane = Some(Focus::Right);
    }
    if let Some((h, p, c)) = init_remote {
        ui::request_remote_dir(&mut app, Focus::Right, h, p, c);
    }

    loop {
        // Process UI messages from background tasks
        let mut needs_refresh = false;
        while let Ok(msg) = app.rx_ui.try_recv() {
            match msg {
                UiMsg::RemoteEntries { pane, entries: new_entries } => {
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
                        app.toast = Some((format!("{} Transfer successful!", icon), std::time::Instant::now()));
                        needs_refresh = true; // Mark that we need to refresh after the match
                    } else {
                        let icon = if ui::is_ascii_mode() { "[FAIL]" } else { "✗" };
                        app.status = format!("{} {}", icon, message);
                        app.error = Some(message.clone());
                        app.toast = Some((format!("{} {}", icon, message), std::time::Instant::now()));
                    }
                }
                UiMsg::Loading { pane } => {
                    app.loading_pane = Some(pane);
                }
            }
        }
        
        // Refresh panes if needed (after successful transfer)
        if needs_refresh {
            ui::refresh_panes(&mut app);
        }
        
        // Drain any output from background transfer
        if let Some(rx) = &app.rx { while let Ok(line) = rx.try_recv() { if app.log.len() >= 256 { let _ = app.log.pop_front(); } app.log.push_back(line); } }
        if app.running { app.spinner_idx = (app.spinner_idx + 1) % 10; }
        
        // Clear old toasts (after 3 seconds)
        if let Some((_, instant)) = &app.toast {
            if instant.elapsed() > std::time::Duration::from_secs(3) {
                app.toast = None;
            }
        }
        
        terminal.draw(|f| ui::draw(f, &app))?;

        if event::poll(std::time::Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(k) if k.kind == KeyEventKind::Press => {
                    let code = k.code;
                    let modifiers = k.modifiers;
                    if app.ui_mode == UiMode::ServerInput {
                        // Handle server input mode
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
                    } else {
                        // Normal mode
                        match (code, modifiers) {
                            (KeyCode::Char('q'), _) => {
                                // Clean up any running transfers before quitting
                                if app.running {
                                    cancel_transfer(&mut app);
                                    // Give it a moment to clean up
                                    std::thread::sleep(std::time::Duration::from_millis(100));
                                }
                                break;
                            }
                            // Tab or Ctrl+Tab or Left/Right arrows to switch panes
                            (KeyCode::Tab, _) | (KeyCode::BackTab, _) | (KeyCode::Char('\t'), _) => {
                                app.ui_mode = UiMode::Normal;
                                app.ui_mode = UiMode::Normal;
                                app.help_visible = false;
                                
                                app.focus = if app.focus == Focus::Left { Focus::Right } else { Focus::Left };
                                // Add toast to confirm focus change
                                let pane_name = if app.focus == Focus::Left { "Left" } else { "Right" };
                                app.toast = Some((format!("Focused: {} pane", pane_name), std::time::Instant::now()));
                            }
                            // Alternative: Use Left/Right arrows with Alt modifier to switch panes
                            (KeyCode::Left, m) if m.contains(KeyModifiers::ALT) => {
                                app.ui_mode = UiMode::Normal;
                                app.ui_mode = UiMode::Normal;
                                app.help_visible = false;
                                app.error = None;
                                app.focus = Focus::Left;
                                app.toast = Some(("Focused: Left pane".to_string(), std::time::Instant::now()));
                            }
                            (KeyCode::Right, m) if m.contains(KeyModifiers::ALT) => {
                                app.ui_mode = UiMode::Normal;
                                app.ui_mode = UiMode::Normal;
                                app.help_visible = false;
                                app.error = None;
                                app.focus = Focus::Right;
                                app.toast = Some(("Focused: Right pane".to_string(), std::time::Instant::now()));
                            }
                            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => { ui::move_up(&mut app); }
                            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => { ui::move_down(&mut app); }
                            (KeyCode::PageUp, _) => { ui::move_page_up(&mut app); }
                            (KeyCode::PageDown, _) => { ui::move_page_down(&mut app); }
                            (KeyCode::Home, _) => { ui::move_home(&mut app); }
                            (KeyCode::End, _) => { ui::move_end(&mut app); }
                            (KeyCode::Enter, _) => { ui::enter(&mut app); }
                            (KeyCode::Char('s'), _) => { app.src = Some(ui::current_path(&app)); }
                            (KeyCode::Char('d'), _) => { app.dest = Some(ui::current_path(&app)); }
                            (KeyCode::Char('m'), _) => { app.mode = match app.mode { Mode::Mirror => Mode::Copy, Mode::Copy => Mode::Move, Mode::Move => Mode::Mirror }; }
                            (KeyCode::Char('t'), _) => { app.tar_small = !app.tar_small; }
                            (KeyCode::Char('r'), _) => { app.delta_large = !app.delta_large; }
                            (KeyCode::Char('e'), _) => { app.include_empty = !app.include_empty; }
                            (KeyCode::Char('c'), _) => { app.checksum = !app.checksum; }
                            (KeyCode::Char('R'), _) | (KeyCode::Char('r'), m) if m.contains(KeyModifiers::CONTROL) => { 
                                app.ui_mode = UiMode::ServerInput;
                                app.input_buffer = "127.0.0.1:9031".to_string(); // Default value
                            }
                            (KeyCode::Char('h'), _) | (KeyCode::F(1), _) => { ui::toggle_help(&mut app); }
                            // Add 'f' key to toggle focus between panes (f for focus)
                            (KeyCode::Char('f'), _) => {
                                app.focus = if app.focus == Focus::Left { Focus::Right } else { Focus::Left };
                                let pane_name = if app.focus == Focus::Left { "Left" } else { "Right" };
                                app.toast = Some((format!("Focused: {} pane", pane_name), std::time::Instant::now()));
                            }
                            (KeyCode::Char('n'), _) => { 
                                app.ui_mode = UiMode::NewFolderInput;
                                app.input_buffer.clear();
                            }
                            (KeyCode::Char('g'), _) => { if !app.running { start_transfer(&mut app); } }
                            (KeyCode::Char('x'), _) => { if app.running { cancel_transfer(&mut app); } }
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
        _ => { app.status = "Select src (s) and dest (d) first".to_string(); return; }
    };
    // Build command
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("blit"));
    let sub = match app.mode { Mode::Mirror => "mirror", Mode::Copy => "copy", Mode::Move => "move" };
    let srcs = ui::pathspec_to_string(&src);
    let dests = ui::pathspec_to_string(&dest);
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("-v").arg(sub).arg(&srcs).arg(&dests);
    // Spawn and capture output
    let mut child = match cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(e) => { app.status = format!("Failed to start: {}", e); return; }
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    let _ = tx.send(format!("[cmd] blit {} {} {}", sub, srcs, dests));
    app.rx = Some(rx);
    app.running = true;
    app.status = format!("Running {}…", sub);
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
    if let Some(out) = stdout { let txc = tx.clone(); spawn_reader(out, txc); }
    if let Some(err) = stderr { let txc = tx.clone(); std::thread::spawn(move || { use std::io::{BufRead, BufReader}; let br = BufReader::new(err); for line in br.lines().flatten() { let _ = txc.send(format!("[err] {}", line)); } }); }
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
                            exit_message = format!("Transfer failed with exit code: {}", 
                                status.code().unwrap_or(-1));
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
            message: exit_message 
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
            // Fallback to root or drives on Windows
            #[cfg(windows)]
            return PathBuf::from("\\\\?\\drives");  // Special marker for drive selection
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
    app.toast = Some((format!("{} Transfer canceled by user", icon), std::time::Instant::now()));
}
