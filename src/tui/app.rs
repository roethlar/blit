#![cfg(feature = "tui")]

use anyhow::Result;
use ratatui::{
    backend::CrosstermBackend,
    Terminal,
};
    use crossterm::{
        execute,
        terminal::{enable_raw_mode, disable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
        event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    };
use std::io::{self, Stdout};
use std::path::{PathBuf};

use crate::url::{self, RemoteDest};
use super::ui;

#[derive(Clone, Copy, PartialEq)]
pub enum Mode { Mirror, Copy, Move }

#[derive(Clone, Copy, PartialEq)]
pub enum Focus { Left, Right }

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
}

impl AppState {
    fn new(remote: Option<RemoteDest>) -> Self {
        let left_cwd = std::env::current_dir().unwrap_or(PathBuf::from("/"));
        let left_entries = ui::read_local_dir(&left_cwd);
        let right = if let Some(r) = remote {
            Pane::Remote { host: r.host, port: r.port, cwd: r.path, entries: Vec::new(), selected: 0 }
        } else {
            let cwd = left_cwd.clone();
            Pane::Local { cwd, entries: ui::read_local_dir(&left_cwd), selected: 0 }
        };
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
        }
    }
}

pub fn run(remote: Option<RemoteDest>) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    let mut app = AppState::new(remote);

    // initial remote load if needed
    if let Pane::Remote { ref host, port, ref cwd, ref mut entries, .. } = app.right {
        *entries = ui::read_remote_dir(host, port, cwd);
    }

    loop {
        // Drain any output from background transfer
        if let Some(rx) = &app.rx { while let Ok(line) = rx.try_recv() { if line == "__DONE__" { app.running = false; app.status = "Done".to_string(); } else { if app.log.len() >= 256 { let _ = app.log.pop_front(); } app.log.push_back(line); } } }
        if app.running { app.spinner_idx = (app.spinner_idx + 1) % 10; }
        terminal.draw(|f| ui::draw(f, &app))?;

        if event::poll(std::time::Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(KeyEvent{ code, modifiers, .. }) => {
                    match (code, modifiers) {
                        (KeyCode::Char('q'), _) => break,
                        (KeyCode::Tab, _) => { app.focus = if app.focus == Focus::Left { Focus::Right } else { Focus::Left }; }
                        (KeyCode::Up, _) | (KeyCode::Char('k'), _) => { ui::move_up(&mut app); }
                        (KeyCode::Down, _) | (KeyCode::Char('j'), _) => { ui::move_down(&mut app); }
                        (KeyCode::Enter, _) => { ui::enter(&mut app); }
                        (KeyCode::Char('s'), _) => { app.src = Some(ui::current_path(&app)); }
                        (KeyCode::Char('d'), _) => { app.dest = Some(ui::current_path(&app)); }
                        (KeyCode::Char('m'), _) => { app.mode = match app.mode { Mode::Mirror => Mode::Copy, Mode::Copy => Mode::Move, Mode::Move => Mode::Mirror }; }
                        (KeyCode::Char('t'), _) => { app.tar_small = !app.tar_small; }
                        (KeyCode::Char('r'), _) => { app.delta_large = !app.delta_large; }
                        (KeyCode::Char('e'), _) => { app.include_empty = !app.include_empty; }
                        (KeyCode::Char('c'), _) => { app.checksum = !app.checksum; }
                        (KeyCode::Char('R'), _) => { ui::toggle_remote_right(&mut app); }
                        (KeyCode::Char('h'), _) => { ui::toggle_help(&mut app); }
                        (KeyCode::Char('g'), _) => { if !app.running { start_transfer(&mut app); } }
                        (KeyCode::Char('x'), _) => { if app.running { cancel_transfer(&mut app); } }
                        _ => {}
                    }
                }
                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
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
    let exe = std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("robosync"));
    let sub = match app.mode { Mode::Mirror => "mirror", Mode::Copy => "copy", Mode::Move => "move" };
    let srcs = ui::pathspec_to_string(&src);
    let dests = ui::pathspec_to_string(&dest);
    let mut cmd = std::process::Command::new(exe);
    cmd.arg(sub).arg(&srcs).arg(&dests).arg("-v");
    // Spawn and capture output
    let mut child = match cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(e) => { app.status = format!("Failed to start: {}", e); return; }
    };
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (tx, rx) = std::sync::mpsc::channel::<String>();
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
    // Waiter
    std::thread::spawn(move || {
        // Handle poisoned lock gracefully - if another thread panicked while holding the lock,
        // we still want to try to wait on the child process
        if let Ok(mut guard) = handle.lock() {
            if let Some(mut ch) = guard.take() { 
                let _ = ch.wait(); 
            }
        }
        let _ = tx.send("__DONE__".to_string());
    });
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
    app.status = "Canceled".to_string();
}
