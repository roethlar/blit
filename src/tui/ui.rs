#![cfg(feature = "tui")]

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    text::{Span, Line},
    Frame,
};
use std::path::{Path, PathBuf};
use crate::url;
use crate::protocol;
use tokio::net::TcpStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use super::{app::{AppState, Focus, Pane, Mode}, theme::Theme};

#[derive(Clone)]
pub struct Entry { pub name: String, pub is_dir: bool, pub is_symlink: bool }

#[derive(Clone)]
pub enum PathSpec { Local(PathBuf), Remote{ host: String, port: u16, path: PathBuf } }

pub fn draw(f: &mut Frame, app: &AppState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(3), Constraint::Length(1)].as_ref())
        .split(f.size());
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)].as_ref())
        .split(chunks[0]);
    draw_pane(f, cols[0], &app.left, app.focus == Focus::Left);
    draw_pane(f, cols[1], &app.right, app.focus == Focus::Right);

    // Log area (last few lines)
    let mut lines: Vec<Line> = Vec::new();
    let max_lines = 3usize;
    let start = app.log.len().saturating_sub(max_lines);
    for l in app.log.iter().skip(start) {
        lines.push(Line::from(Span::styled(l.clone(), Theme::status())));
    }
    let logp = Paragraph::new(lines).block(Block::default().borders(Borders::NONE));
    f.render_widget(logp, chunks[1]);

    // Status line
    let spinner = if app.running {
        let s = ['⠋','⠙','⠹','⠸','⠼','⠴','⠦','⠧','⠇','⠏'][app.spinner_idx % 10];
        // advance spinner
        // NOTE: we cannot mutate here; app.spinner_idx is advanced by caller if desired
        s
    } else { ' ' };
    let status = format!(
        "{} mode:{} TAR:{} DELTA:{} EMPTY:{} CHKSUM:{} SRC:{} DST:{}  {}",
        spinner,
        mode_str(app.mode), onoff(app.tar_small), onoff(app.delta_large), onoff(app.include_empty), onoff(app.checksum),
        path_short(app.src.as_ref()), path_short(app.dest.as_ref()), app.status
    );
    let p = Paragraph::new(Line::from(Span::raw(status))).block(Block::default().borders(Borders::NONE));
    f.render_widget(p, chunks[2]);
    // Help overlay
    if get_show_help(app) {
        let area = centered_rect(60, 40, f.size());
        let lines = vec![
            Line::from("Keys: q quit | Tab switch pane | ↑/↓/Enter navigate"),
            Line::from("s set src | d set dest | m mode (mirror/copy/move)"),
            Line::from("R toggle right remote | g run | x cancel | h help"),
        ];
        let w = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Help"));
        f.render_widget(w, area);
    }
}

fn draw_pane(f: &mut Frame, area: Rect, pane: &Pane, focused: bool) {
    let (title, entries, selected) = match pane {
        Pane::Local { cwd, entries, selected } => (format!("Local: {}", cwd.display()), entries, *selected),
        Pane::Remote { host, port, cwd, entries, selected } => (format!("Remote: {}:{}/{}", host, port, cwd.display()), entries, *selected),
    };
    let items: Vec<ListItem> = entries.iter().enumerate().map(|(i,e)| {
        let style = if i == selected { Theme::selected() } else if e.is_dir { Theme::dir() } else if e.is_symlink { Theme::symlink() } else { Theme::file() };
        ListItem::new(Span::styled(e.name.clone(), style))
    }).collect();
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(Span::styled(title, Theme::header(focused))));
    f.render_widget(list, area);
}

pub fn read_local_dir(cwd: &Path) -> Vec<Entry> {
    let mut out = Vec::new();
    out.push(Entry { name: "..".to_string(), is_dir: true, is_symlink: false });
    
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

pub fn read_remote_dir(host: &str, port: u16, path: &Path) -> Vec<Entry> {
    // Use a short timeout to prevent blocking the UI
    let mut out = Vec::new();
    out.push(Entry { name: "..".to_string(), is_dir: true, is_symlink: false });
    
    // Build a runtime with timeout to prevent blocking
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build();
    if let Ok(rt) = rt {
        // Use a 500ms timeout to keep UI responsive
        let res = rt.block_on(async move {
            tokio::time::timeout(std::time::Duration::from_millis(500), async move {
            let addr = format!("{}:{}", host, port);
            let mut stream = TcpStream::connect(&addr).await.ok()?;
            // Build header + payload
            let p = path.to_string_lossy();
            let mut payload = Vec::with_capacity(2 + p.len());
            payload.extend_from_slice(&(p.len() as u16).to_le_bytes());
            payload.extend_from_slice(p.as_bytes());
            let mut hdr = Vec::with_capacity(11);
            hdr.extend_from_slice(protocol::MAGIC);
            hdr.extend_from_slice(&protocol::VERSION.to_le_bytes());
            hdr.push(protocol::frame::LIST_REQ);
            hdr.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            let _ = stream.write_all(&hdr).await.ok()?;
            let _ = stream.write_all(&payload).await.ok()?;
            // Read response header
            let mut h = [0u8; 11];
            let _ = stream.read_exact(&mut h).await.ok()?;
            if &h[0..4] != protocol::MAGIC { return Some(vec![]); }
            let _ver = u16::from_le_bytes([h[4], h[5]]);
            let typ = h[6];
            let len = u32::from_le_bytes([h[7],h[8],h[9],h[10]]) as usize;
            let mut resp = vec![0u8; len];
            let _ = stream.read_exact(&mut resp).await.ok()?;
            if typ != protocol::frame::LIST_RESP { return Some(vec![]); }
            let mut v = Vec::new();
            v.push(Entry { name: "..".to_string(), is_dir: true, is_symlink: false });
            let mut off = 0usize;
            if resp.len() < 4 { return Some(v); }
            let mut u32b = [0u8;4]; u32b.copy_from_slice(&resp[off..off+4]);
            let count = u32::from_le_bytes(u32b) as usize; off+=4;
            for _ in 0..count {
                if off >= resp.len() { break; }
                let kind = resp[off]; off+=1;
                if off+2 > resp.len() { break; }
                let mut u16b = [0u8;2]; u16b.copy_from_slice(&resp[off..off+2]);
                let nlen = u16::from_le_bytes(u16b) as usize; off+=2;
                if off+nlen > resp.len() { break; }
                let name = std::str::from_utf8(&resp[off..off+nlen]).unwrap_or("").to_string(); off+=nlen;
                // kind: 0=file, 1=dir, 2=truncation marker
                if kind == 2 {
                    // Special truncation marker - show as informational entry
                    v.push(Entry { name, is_dir: false, is_symlink: false });
                } else {
                    v.push(Entry { name, is_dir: kind==1, is_symlink: false });
                }
            }
            Some(v)
            }).await.ok()
        });
        
        if let Some(Some(mut v)) = res {
            if !v.is_empty() {
                // No need to re-insert ".." as it's already at index 0 in v
                return v;
            }
        } else {
            // Connection timed out or failed - show error in list
            out.push(Entry { name: "[Connection failed or timed out]".to_string(), is_dir: false, is_symlink: false });
        }
    }
    out
}

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

pub fn enter(app: &mut super::app::AppState) {
    let (entries, selected, cwd, is_remote, host, port) = match app.focus {
        Focus::Left => match &mut app.left {
            Pane::Local { entries, selected, cwd } => (entries, selected, cwd, false, String::new(), 0u16),
            _ => unreachable!(),
        },
        Focus::Right => match &mut app.right {
            Pane::Local { entries, selected, cwd } => (entries, selected, cwd, false, String::new(), 0u16),
            Pane::Remote { entries, selected, cwd, host, port } => (entries, selected, cwd, true, host.clone(), *port),
        },
    };
    if *selected >= entries.len() { return; }
    let name = entries[*selected].name.clone();
    if name == ".." {
        if let Some(parent) = cwd.parent() { *cwd = parent.to_path_buf(); }
    } else if entries[*selected].is_dir {
        *cwd = cwd.join(name);
    }
    // refresh entries
    if is_remote {
        *entries = read_remote_dir(&host, port, cwd);
    } else {
        *entries = read_local_dir(cwd);
    }
    if *selected >= entries.len() { *selected = entries.len().saturating_sub(1); }
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
            _ => unreachable!(),
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

pub fn toggle_remote_right(app: &mut super::app::AppState) {
    match &mut app.right {
        Pane::Local { .. } => {
            // prompt for host:port and root? minimal: use localhost:9031 and "/"
            let host = "127.0.0.1".to_string();
            let port = 9031u16;
            let cwd = PathBuf::from("/");
            let entries = read_remote_dir(&host, port, &cwd);
            app.right = Pane::Remote { host, port, cwd, entries, selected: 0 };
        }
        Pane::Remote { .. } => {
            let cwd = std::env::current_dir().unwrap_or(PathBuf::from("/"));
            app.right = Pane::Local { cwd: cwd.clone(), entries: read_local_dir(&cwd), selected: 0 };
        }
    }
}

pub fn toggle_help(app: &mut super::app::AppState) {
    // store as a status toggle in AppState via status prefix hack
    let flag = "[HELP]";
    if app.status.starts_with(flag) { app.status = app.status.trim_start_matches(flag).trim_start().to_string(); } else { app.status = format!("{} {}", flag, app.status); }
}

fn get_show_help(app: &AppState) -> bool { app.status.starts_with("[HELP]") }

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
fn onoff(b: bool) -> &'static str { if b { "on" } else { "off" } }
fn path_short(p: Option<&PathSpec>) -> String {
    match p {
        None => "-".to_string(),
        Some(PathSpec::Local(pb)) => pb.display().to_string(),
        Some(PathSpec::Remote{host,port,path}) => format!("{}:{}/{}", host, port, path.display()),
    }
}

pub fn pathspec_to_string(p: &PathSpec) -> String {
    match p {
        PathSpec::Local(pb) => pb.display().to_string(),
        PathSpec::Remote{host,port,path} => {
            let mut s = String::new();
            s.push_str("robosync://");
            s.push_str(host);
            s.push(':');
            s.push_str(&port.to_string());
            let mut pstr = path.display().to_string();
            if !pstr.starts_with('/') { pstr = format!("/{}", pstr); }
            s.push_str(&pstr);
            s
        }
    }
}
