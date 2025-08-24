use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::stdout;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
thread_local! {
    static MTIME_STORE: std::cell::RefCell<std::collections::HashMap<String, i64>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

fn apply_preserved_mtime(path: &Path) -> Result<()> {
    use filetime::{FileTime, set_file_mtime};
    let rel = path.to_string_lossy().to_string();
    let mtime_opt = MTIME_STORE.with(|mt| mt.borrow_mut().remove(&rel));
    if let Some(secs) = mtime_opt {
        let ft = FileTime::from_unix_time(secs, 0);
        set_file_mtime(path, ft).ok();
    }
    Ok(())
}
use std::thread;
use std::time::{Duration, Instant};

const MAGIC: &[u8; 4] = b"RSNC";
const VERSION: u16 = 1;

#[repr(u8)]
enum FrameType {
    Start = 1,
    Ok = 2,
    Error = 3,
    FileStart = 4,
    FileData = 5,
    FileEnd = 6,
    Done = 7,
    TarStart = 8,
    TarData = 9,
    TarEnd = 10,
    PFileStart = 11,
    PFileData = 12,
    PFileEnd = 13,
    ManifestStart = 14,
    ManifestEntry = 15,
    ManifestEnd = 16,
    NeedList = 17,
    Symlink = 18,
    MkDir = 19,
    // Note: Directories are conveyed via manifest entries (kind=2), no frame needed
}

fn write_u16(buf: &mut Vec<u8>, v: u16) {
    buf.extend_from_slice(&v.to_le_bytes());
}
fn write_u32(buf: &mut Vec<u8>, v: u32) {
    buf.extend_from_slice(&v.to_le_bytes());
}

fn write_frame(stream: &mut TcpStream, t: FrameType, payload: &[u8]) -> Result<()> {
    let mut hdr = Vec::with_capacity(4 + 2 + 1 + 4);
    hdr.extend_from_slice(MAGIC);
    write_u16(&mut hdr, VERSION);
    hdr.push(t as u8);
    write_u32(&mut hdr, payload.len() as u32);
    stream.write_all(&hdr)?;
    stream.write_all(payload)?;
    Ok(())
}

fn read_exact(stream: &mut TcpStream, n: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}

fn read_frame(stream: &mut TcpStream) -> Result<(u8, Vec<u8>)> {
    let mut hdr = [0u8; 11];
    stream.read_exact(&mut hdr)?;
    if &hdr[0..4] != MAGIC {
        anyhow::bail!("bad magic");
    }
    let _ver = u16::from_le_bytes([hdr[4], hdr[5]]);
    let typ = hdr[6];
    let len = u32::from_le_bytes([hdr[7], hdr[8], hdr[9], hdr[10]]) as usize;
    let payload = read_exact(stream, len)?;
    Ok((typ, payload))
}

// Reader that pulls TAR_DATA frames from the TCP stream until TAR_END
struct TarFrameReader<'a> {
    stream: &'a mut TcpStream,
    buffer: Vec<u8>,
    pos: usize,
    done: bool,
}

impl<'a> TarFrameReader<'a> {
    fn new(stream: &'a mut TcpStream) -> Self {
        Self {
            stream,
            buffer: Vec::new(),
            pos: 0,
            done: false,
        }
    }
}

impl<'a> Read for TarFrameReader<'a> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        if self.pos >= self.buffer.len() {
            // Refill buffer from next TAR_* frame
            let (typ, payload) = read_frame(self.stream)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            if typ == FrameType::TarData as u8 {
                self.buffer = payload;
                self.pos = 0;
            } else if typ == FrameType::TarEnd as u8 {
                self.done = true;
                return Ok(0);
            } else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "unexpected frame while reading tar",
                ));
            }
        }
        let n = std::cmp::min(out.len(), self.buffer.len() - self.pos);
        out[..n].copy_from_slice(&self.buffer[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

fn handle_tar_stream(stream: &mut TcpStream, base: &Path, received: &mut HashSet<PathBuf>) -> Result<()> {
    // Unpack tar stream under base while tracking received file paths
    let mut reader = TarFrameReader::new(stream);
    let mut archive = tar::Archive::new(&mut reader);
    archive.set_overwrite(true);
    let mut entries = archive.entries()?;
    while let Some(res) = entries.next() {
        let mut entry = res?;
        let et = entry.header().entry_type();
        if et.is_block_special() || et.is_character_special() || et.is_fifo() {
            // Skip special device/FIFO entries for safety
            continue;
        }
        // On Windows, create symlinks explicitly to avoid tar crate failures when privileges are missing
        #[cfg(windows)]
        if et.is_symlink() {
            if let Some(target) = entry.link_name()? {
                let rel = entry.path()?.to_path_buf();
                let mut dst = PathBuf::from(base);
                use std::path::Component::{CurDir, Normal, RootDir, ParentDir, Prefix};
                for comp in rel.components() {
                    match comp {
                        CurDir => {}
                        Normal(s) => dst.push(s),
                        RootDir | Prefix(_) => {}
                        ParentDir => anyhow::bail!("tar symlink contains parent component"),
                    }
                }
                if let Some(parent) = dst.parent() { fs::create_dir_all(parent).ok(); }
                let t = target.into_owned();
                let created = std::os::windows::fs::symlink_file(&t, &dst)
                    .or_else(|_| std::os::windows::fs::symlink_dir(&t, &dst));
                if created.is_ok() {
                    received.insert(dst);
                    continue;
                }
            }
            // If we couldn't handle symlink specially, fall back to default unpack
        }
        // Unpack within base safely (allows files, dirs, symlinks, hardlinks)
        entry.unpack_in(base)?;
        // Track created path under base without resolving symlinks to avoid false escapes
        let rel = entry.path()?.to_path_buf();
        use std::path::Component;
        for comp in rel.components() {
            if matches!(comp, Component::ParentDir) {
                anyhow::bail!("tar entry contains parent component");
            }
        }
        // Join under base and normalize out any CurDir components without resolving symlinks
        use std::path::Component::{CurDir, Normal, RootDir};
        let mut joined = PathBuf::from(base);
        for comp in rel.components() {
            match comp {
                CurDir => { /* skip ./ */ }
                Normal(s) => joined.push(s),
                RootDir => { /* ignore absolute root, we already started from base */ }
                _ => { /* ParentDir already rejected above; others not expected on Unix */ }
            }
        }
        received.insert(joined);
    }
    // Drain any remaining TAR frames until TAR_END (the archive reader may stop at end-of-archive blocks)
    drop(entries);
    drop(archive);
    drop(reader);
    loop {
        let (typ, _pl) = read_frame(stream)?;
        if typ == FrameType::TarEnd as u8 { break; }
        if typ != FrameType::TarData as u8 {
            anyhow::bail!("unexpected frame while finishing tar: {}", typ);
        }
    }
    // Ack TAR sequence complete
    write_frame(stream, FrameType::Ok, b"TAR_OK")?;
    Ok(())
}

pub fn serve(bind: &str, root: &Path) -> Result<()> {
    let listener = TcpListener::bind(bind).with_context(|| format!("bind {}", bind))?;
    eprintln!(
        "robosync daemon listening on {} root={}",
        bind,
        root.display()
    );
    for conn in listener.incoming() {
        match conn {
            Ok(mut stream) => {
                let peer = stream.peer_addr().map(|a| a.to_string()).unwrap_or_else(|_| "unknown".to_string());
                eprintln!("conn from {}", peer);
                if let Err(e) = handle_conn(&mut stream, root) {
                    eprintln!("connection error: {}", e);
                    let _ = write_frame(&mut stream, FrameType::Error, format!("{}", e).as_bytes());
                }
            }
            Err(e) => {
                eprintln!("accept error: {}", e);
            }
        }
    }
    Ok(())
}

fn handle_conn(stream: &mut TcpStream, root: &Path) -> Result<()> {
    let (typ, payload) = read_frame(stream)?;
    if typ != FrameType::Start as u8 {
        anyhow::bail!("expected START");
    }
    // payload encoding: dest_len u16 | dest_bytes | flags u8 (optional; bit0 mirror)
    if payload.len() < 2 {
        anyhow::bail!("bad START payload");
    }
    let dlen = u16::from_le_bytes([payload[0], payload[1]]) as usize;
    if payload.len() < 2 + dlen {
        anyhow::bail!("bad START payload len");
    }
    let dest = std::str::from_utf8(&payload[2..2 + dlen]).context("utf8 dest")?;
    let flags = if payload.len() >= 2 + dlen + 1 { payload[2 + dlen] } else { 0 };
    let mirror = (flags & 0b0000_0001) != 0;
    let pull = (flags & 0b0000_0010) != 0;
    let include_dirs = (flags & 0b0000_0100) != 0;
    let dest_rel = PathBuf::from(dest);
    let base = normalize_under_root(root, &dest_rel)?;
    fs::create_dir_all(&base).ok();
    eprintln!("start dest={} mirror={}", base.display(), mirror);
    write_frame(stream, FrameType::Ok, b"OK")?;

    // Optional manifest: client may send a manifest to decide what to transfer
    let mut need_set: Option<std::collections::HashSet<String>> = None;
    // Tracks all relative paths the client reported in its manifest (files, symlinks, dirs)
    let mut client_present: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut expected_paths: HashSet<PathBuf> = HashSet::new();
    let mut pending: Option<(u8, Vec<u8>)> = None;
    if let Ok((t0, pl0)) = read_frame(stream) {
        if t0 == FrameType::ManifestStart as u8 {
            let mut needed: std::collections::HashSet<String> = std::collections::HashSet::new();
            loop {
                let (t2, pl2) = read_frame(stream)?;
                if t2 == FrameType::ManifestEntry as u8 {
                    if pl2.len() < 1 + 2 { anyhow::bail!("bad MANIFEST_ENTRY"); }
                    let kind = pl2[0];
                    let nlen = u16::from_le_bytes([pl2[1], pl2[2]]) as usize;
                    if pl2.len() < 3 + nlen { anyhow::bail!("bad MANIFEST_ENTRY path len"); }
                    let rel = std::str::from_utf8(&pl2[3..3 + nlen]).context("utf8 rel")?;
                    let relp = PathBuf::from(rel);
                    let dst = normalize_under_root(&base, &relp)?;
                    // Record that the client has this path
                    client_present.insert(rel.to_string());
                    match kind {
                        0 => {
                            // file: size u64 | mtime i64
                            if pl2.len() < 3 + nlen + 8 + 8 { anyhow::bail!("bad MANIFEST_ENTRY file fields"); }
                            let off = 3 + nlen;
                            let size = u64::from_le_bytes(pl2[off..off + 8].try_into().unwrap());
                            let mtime = i64::from_le_bytes(pl2[off + 8..off + 16].try_into().unwrap());
                            let mut need = true;
                            if let Ok(md) = std::fs::metadata(&dst) {
                                if md.is_file() {
                                    let dsize = md.len();
                                    let dmtime = md
                                        .modified()
                                        .ok()
                                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                        .map(|d| d.as_secs() as i64)
                                        .unwrap_or(0);
                                    let dt = (dmtime - mtime).abs();
                                    need = !(dsize == size && dt <= 2);
                                }
                            }
                            if need { needed.insert(rel.to_string()); }
                            expected_paths.insert(dst.clone());
                        }
                        1 => {
                            // symlink: tlen u16 | target bytes
                            if pl2.len() < 3 + nlen + 2 { anyhow::bail!("bad MANIFEST_ENTRY symlink fields"); }
                            let off = 3 + nlen;
                            let tlen = u16::from_le_bytes([pl2[off], pl2[off + 1]]) as usize;
                            if pl2.len() < off + 2 + tlen { anyhow::bail!("bad MANIFEST_ENTRY symlink target len"); }
                            let target = std::str::from_utf8(&pl2[off + 2..off + 2 + tlen]).unwrap_or("");
                            let mut need = true;
                            if let Ok(smd) = std::fs::symlink_metadata(&dst) {
                                if smd.file_type().is_symlink() {
                                    if let Ok(cur) = std::fs::read_link(&dst) {
                                        if cur.as_os_str() == std::ffi::OsStr::new(target) { need = false; }
                                    }
                                }
                            }
                            if need { needed.insert(rel.to_string()); }
                            expected_paths.insert(dst.clone());
                        }
                        2 => {
                            // directory: create it to preserve empty dirs; never needed for transfer
                            fs::create_dir_all(&dst).ok();
                            expected_paths.insert(dst.clone());
                        }
                        2 => {
                            // directory: ensure exists; never needed for transfer
                            fs::create_dir_all(&dst).ok();
                            expected_paths.insert(dst.clone());
                        }
                        _ => {}
                    }
                } else if t2 == FrameType::ManifestEnd as u8 {
                    let mut resp = Vec::with_capacity(4 + needed.len() * 4);
                    resp.extend_from_slice(&(needed.len() as u32).to_le_bytes());
                    for p in &needed {
                        let b = p.as_bytes();
                        resp.extend_from_slice(&(b.len() as u16).to_le_bytes());
                        resp.extend_from_slice(b);
                    }
                    write_frame(stream, FrameType::NeedList, &resp)?;
                    need_set = Some(needed);
                    break;
                } else {
                    anyhow::bail!("unexpected frame during manifest: {}", t2);
                }
            }
            // If pull mode, send needed entries now
            if pull {
                let needed = need_set.clone().unwrap_or_default();
                // Send all if there was no manifest; otherwise send paths that are either needed (changed)
                // or missing on the client (not present in client manifest).
                let send_all = need_set.is_none();
                // Send directory creation frames to ensure empty dirs exist on client
                if include_dirs {
                    for ent in walkdir::WalkDir::new(&base).follow_links(false).into_iter().filter_map(|e| e.ok()) {
                        if ent.file_type().is_dir() {
                            let rel = ent.path().strip_prefix(&base).unwrap_or(ent.path());
                            if rel.as_os_str().is_empty() { continue; }
                            let rels = rel.to_string_lossy();
                            let mut pl = Vec::with_capacity(2 + rels.len());
                            pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                            pl.extend_from_slice(rels.as_bytes());
                            write_frame(stream, FrameType::MkDir, &pl)?;
                        }
                    }
                }
                for ent in walkdir::WalkDir::new(&base).follow_links(false).into_iter().filter_map(|e| e.ok()) {
                    let rel = ent.path().strip_prefix(&base).unwrap_or(ent.path());
                    if rel.as_os_str().is_empty() { continue; }
                    let rels = rel.to_string_lossy();
                    let rels_owned = rels.to_string();
                    if !send_all {
                        if !(needed.contains(&rels_owned) || !client_present.contains(&rels_owned)) { continue; }
                    }
                    if ent.file_type().is_symlink() {
                        if let Ok(t) = std::fs::read_link(ent.path()) {
                            let targ = t.to_string_lossy();
                            let mut pl = Vec::with_capacity(2 + rels.len() + 2 + targ.len());
                            pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                            pl.extend_from_slice(rels.as_bytes());
                            pl.extend_from_slice(&(targ.len() as u16).to_le_bytes());
                            pl.extend_from_slice(targ.as_bytes());
                            write_frame(stream, FrameType::Symlink, &pl)?;
                        }
                    } else if ent.file_type().is_file() {
                        let md = ent.metadata()?;
                        let size = md.len();
                        let mtime = md.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
                        let mut pl = Vec::with_capacity(2 + rels.len() + 8 + 8);
                        pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                        pl.extend_from_slice(rels.as_bytes());
                        pl.extend_from_slice(&size.to_le_bytes());
                        pl.extend_from_slice(&mtime.to_le_bytes());
                        write_frame(stream, FrameType::FileStart, &pl)?;
                        let mut f = File::open(ent.path())?;
                        let mut buf = vec![0u8; 1024 * 1024];
                        loop {
                            let n = f.read(&mut buf)?;
                            if n == 0 { break; }
                            write_frame(stream, FrameType::FileData, &buf[..n])?;
                        }
                        write_frame(stream, FrameType::FileEnd, &[])?;
                    }
                }
                write_frame(stream, FrameType::Done, &[])?;
                // Wait for client OK and return
                let (tt, _pl) = read_frame(stream)?;
                if tt != FrameType::Ok as u8 { anyhow::bail!("client did not ack DONE"); }
                return Ok(());
            }
        } else {
            // Not a manifest; treat as first pending data frame
            pending = Some((t0, pl0));
        }
    }

    // Receive files until DONE
    let mut cur_file: Option<(PathBuf, File, u64, u64)> = None; // (path, handle, size, written)
    let mut p_files: HashMap<u8, (PathBuf, File, u64, u64)> = HashMap::new();
    let mut received_paths: HashSet<PathBuf> = HashSet::new();

    loop {
        let (t, pl) = if let Some((t1, pl1)) = pending.take() { (t1, pl1) } else { read_frame(stream)? };
        match t {
            x if x == FrameType::Symlink as u8 => {
                if pl.len() < 2 { anyhow::bail!("bad SYMLINK"); }
                let nlen = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                if pl.len() < 2 + nlen + 2 { anyhow::bail!("bad SYMLINK payload"); }
                let rel = std::str::from_utf8(&pl[2..2 + nlen]).context("utf8 sym path")?;
                let off = 2 + nlen;
                let tlen = u16::from_le_bytes([pl[off], pl[off + 1]]) as usize;
                if pl.len() < off + 2 + tlen { anyhow::bail!("bad SYMLINK target len"); }
                let target = std::str::from_utf8(&pl[off + 2..off + 2 + tlen]).context("utf8 sym target")?;
                let relp = PathBuf::from(rel);
                let dst_path = normalize_under_root(&base, &relp)?;
                if let Some(parent) = dst_path.parent() { fs::create_dir_all(parent).ok(); }
                #[cfg(unix)]
                {
                    let _ = std::fs::remove_file(&dst_path);
                    std::os::unix::fs::symlink(target, &dst_path)
                        .with_context(|| format!("symlink {} -> {}", dst_path.display(), target))?;
                }
                #[cfg(windows)]
                {
                    let _ = std::fs::remove_file(&dst_path);
                    let _ = std::fs::remove_dir(&dst_path);
                    // Try file, then dir symlink
                    let _ = std::os::windows::fs::symlink_file(target, &dst_path)
                        .or_else(|_| std::os::windows::fs::symlink_dir(target, &dst_path));
                }
                received_paths.insert(dst_path.clone());
                expected_paths.insert(dst_path);
            }
            x if x == FrameType::FileStart as u8 => {
                if pl.len() < 2 + 8 + 8 {
                    anyhow::bail!("bad FILE_START");
                }
                let nlen = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                if pl.len() < 2 + nlen + 8 + 8 {
                    anyhow::bail!("bad FILE_START len");
                }
                let rel = std::str::from_utf8(&pl[2..2 + nlen]).context("utf8 path")?;
                let mut off = 2 + nlen;
                let size = u64::from_le_bytes(pl[off..off + 8].try_into().unwrap());
                off += 8;
                let mtime = i64::from_le_bytes(pl[off..off + 8].try_into().unwrap());
                let relp = PathBuf::from(rel);
                let dst_path = normalize_under_root(&base, &relp)?;
                if let Some(parent) = dst_path.parent() {
                    fs::create_dir_all(parent).ok();
                }
                let f = File::create(&dst_path)
                    .with_context(|| format!("create {}", dst_path.display()))?;
                // Preallocate
                f.set_len(size).ok();
                cur_file = Some((dst_path, f, size, 0));
                // Store desired mtime in a side map keyed by absolute path
                // We'll apply it on FileEnd
                MTIME_STORE.with(|mt| {
                    let mut m = mt.borrow_mut();
                    if let Some((ref p_abs, _, _, _)) = cur_file {
                        m.insert(p_abs.to_string_lossy().to_string(), mtime);
                    }
                });
                if let Some((p, _, _, _)) = &cur_file { received_paths.insert(p.clone()); expected_paths.insert(p.clone()); }
                write_frame(stream, FrameType::Ok, b"FILE_START OK")?;
            }
            x if x == FrameType::TarStart as u8 => {
                // Receive a tar stream and unpack under base
                handle_tar_stream(stream, &base, &mut received_paths)?;
                // Ack TAR_END handling inside handler; continue to next frame
            }
            x if x == FrameType::FileData as u8 => {
                if let Some((_p, fh, _sz, ref mut written)) = cur_file.as_mut() {
                    fh.write_all(&pl)?;
                    *written += pl.len() as u64;
                } else {
                    anyhow::bail!("FILE_DATA without FILE_START");
                }
            }
            x if x == FrameType::FileEnd as u8 => {
                // Close current file
                if let Some((path, _fh, size, written)) = cur_file.take() {
                    if written != size {
                        eprintln!("short write: {} {}/{}", path.display(), written, size);
                    }
                    // Apply preserved mtime if available
                    apply_preserved_mtime(&path)?;
                }
                write_frame(stream, FrameType::Ok, b"FILE_END OK")?;
            }
            x if x == FrameType::PFileStart as u8 => {
                if pl.len() < 1 + 2 + 8 + 8 {
                    anyhow::bail!("bad PFILE_START");
                }
                let stream_id = pl[0];
                let nlen = u16::from_le_bytes([pl[1], pl[2]]) as usize;
                if pl.len() < 1 + 2 + nlen + 8 + 8 {
                    anyhow::bail!("bad PFILE_START len");
                }
                let rel = std::str::from_utf8(&pl[3..3 + nlen]).context("utf8 path")?;
                let mut off = 3 + nlen;
                let size = u64::from_le_bytes(pl[off..off + 8].try_into().unwrap());
                off += 8;
                let mtime = i64::from_le_bytes(pl[off..off + 8].try_into().unwrap());
                let relp = PathBuf::from(rel);
                let dst_path = normalize_under_root(&base, &relp)?;
                if let Some(parent) = dst_path.parent() {
                    fs::create_dir_all(parent).ok();
                }
                let f = File::create(&dst_path)
                    .with_context(|| format!("create {}", dst_path.display()))?;
                f.set_len(size).ok();
                p_files.insert(stream_id, (dst_path, f, size, 0));
                MTIME_STORE.with(|mt| {
                    let mut m = mt.borrow_mut();
                    if let Some((p_abs, _, _, _)) = p_files.get(&stream_id) {
                        m.insert(p_abs.to_string_lossy().to_string(), mtime);
                    }
                });
                if let Some((p, _, _, _)) = p_files.get(&stream_id) { received_paths.insert(p.clone()); expected_paths.insert(p.clone()); }
            }
            x if x == FrameType::PFileData as u8 => {
                if pl.len() < 1 {
                    anyhow::bail!("bad PFILE_DATA");
                }
                let stream_id = pl[0];
                if let Some((_p, fh, _sz, ref mut written)) = p_files.get_mut(&stream_id) {
                    fh.write_all(&pl[1..])?;
                    *written += (pl.len() - 1) as u64;
                } else {
                    anyhow::bail!("PFILE_DATA for unknown stream {}", stream_id);
                }
            }
            x if x == FrameType::PFileEnd as u8 => {
                if pl.len() < 1 {
                    anyhow::bail!("bad PFILE_END");
                }
                let stream_id = pl[0];
                if let Some((path, _fh, size, written)) = p_files.remove(&stream_id) {
                    if written != size {
                        eprintln!("short write: {} {}/{}", path.display(), written, size);
                    }
                    apply_preserved_mtime(&path)?;
                } else {
                    anyhow::bail!("PFILE_END for unknown stream {}", stream_id);
                }
            }
            x if x == FrameType::Done as u8 => {
                // Mirror delete on server if requested
                if mirror {
                    let use_set = if !expected_paths.is_empty() { &expected_paths } else { &received_paths };
                    if let Err(e) = mirror_delete_under(&base, use_set) {
                        eprintln!("mirror delete error: {}", e);
                    }
                }
                write_frame(stream, FrameType::Ok, b"DONE OK")?;
                break;
            }
            _ => {
                anyhow::bail!("unexpected frame type: {}", t);
            }
        }
    }
    Ok(())
}

#[cfg(not(windows))]
fn normalize_under_root(root: &Path, p: &Path) -> Result<PathBuf> {
    // Ensure the destination stays under root (no traversal)
    use std::path::Component;
    for comp in p.components() {
        if matches!(comp, Component::ParentDir) {
            anyhow::bail!("destination contains parent component");
        }
    }
    let joined = if p.is_absolute() {
        root.join(p.strip_prefix("/").unwrap_or(p))
    } else {
        root.join(p)
    };
    let canon_root = std::fs::canonicalize(root).unwrap_or(root.to_path_buf());
    let canon = std::fs::canonicalize(&joined).unwrap_or(joined.clone());
    if !canon.starts_with(&canon_root) {
        anyhow::bail!("destination escapes root");
    }
    Ok(canon)
}

#[cfg(windows)]
fn normalize_under_root(root: &Path, p: &Path) -> Result<PathBuf> {
    // Windows-safe normalization: strip any drive/UNC prefix and root components; reject ParentDir
    use std::path::Component::{CurDir, Normal, ParentDir, Prefix, RootDir};
    let mut joined = PathBuf::from(root);
    for comp in p.components() {
        match comp {
            ParentDir => anyhow::bail!("destination contains parent component"),
            Normal(s) => joined.push(s),
            CurDir => {}
            Prefix(_) | RootDir => {}
        }
    }
    Ok(joined)
}

fn mirror_delete_under(base: &Path, received: &HashSet<PathBuf>) -> Result<(u64, u64)> {
    let mut files_deleted = 0u64;
    let mut dirs_deleted = 0u64;
    // Collect directories for bottom-up deletion
    let mut all_dirs: Vec<PathBuf> = Vec::new();
    for entry in walkdir::WalkDir::new(base).into_iter().filter_map(|e| e.ok()) {
        let p = entry.path().to_path_buf();
        if entry.file_type().is_dir() {
            all_dirs.push(p);
            continue;
        }
        if entry.file_type().is_file() {
            if !received.contains(&p) {
                match std::fs::remove_file(&p) {
                    Ok(_) => files_deleted += 1,
                    Err(e) => eprintln!("delete file failed {}: {}", p.display(), e),
                }
            }
        }
    }
    // Bottom-up directory cleanup
    all_dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
    for d in all_dirs {
        if d == *base { continue; }
        // Preserve directories that are expected for this session (mirror should keep them)
        if received.contains(&d) { continue; }
        match std::fs::remove_dir(&d) {
            Ok(()) => dirs_deleted += 1,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::DirectoryNotEmpty {
                    // Log other errors
                    eprintln!("delete dir failed {}: {}", d.display(), e);
                }
            }
        }
    }
    eprintln!("mirror delete: removed {} files, {} dirs", files_deleted, dirs_deleted);
    Ok((files_deleted, dirs_deleted))
}

pub fn client_start(
    host: &str,
    port: u16,
    dest: &Path,
    src_root: &Path,
    args: &crate::Args,
) -> Result<()> {
    let addr = format!("{}:{}", host, port);
    print!("Connecting {}... ", addr);
    let _ = stdout().flush();
    let stream = TcpStream::connect(&addr).with_context(|| format!("connect {}", addr))?;
    let stream = Arc::new(Mutex::new(stream));

    // START payload: dest_len u16 | dest_bytes | flags u8 (bit0 mirror, bit2 include_empty_dirs)
    let dest_s = dest.to_string_lossy();
    let mut payload = Vec::with_capacity(2 + dest_s.len() + 1);
    payload.extend_from_slice(&(dest_s.len() as u16).to_le_bytes());
    payload.extend_from_slice(dest_s.as_bytes());
    // Compute include-empty-dirs semantics: --mir implies include empties
    let include_empty = if args.mirror || args.delete { true } else if args.subdirs || args.no_empty_dirs { false } else if args.empty_dirs { true } else { true };
    let mut flags: u8 = if args.mirror || args.delete { 0b0000_0001 } else { 0 };
    if include_empty { flags |= 0b0000_0100; }
    payload.push(flags);
    {
        let mut s = stream.lock().unwrap();
        write_frame(&mut s, FrameType::Start, &payload)?;
        let (typ, resp) = read_frame(&mut s)?;
        if typ != FrameType::Ok as u8 {
            anyhow::bail!("daemon error: {}", String::from_utf8_lossy(&resp));
        }
    }
    println!("ok");

    // Enumerate local files under src_root
    let filter = crate::fs_enum::FileFilter {
        exclude_files: args.exclude_files.clone(),
        exclude_dirs: args.exclude_dirs.clone(),
        min_size: None,
        max_size: None,
        include_empty_dirs: true,
    };
    // Link policy: daemon client defaults to dereference unless preserving links with --sl/--sj
    #[cfg(windows)]
    let preserve_links = args.sl || args.sj;
    #[cfg(not(windows))]
    let preserve_links = args.sl;

    let entries = if preserve_links {
        crate::fs_enum::enumerate_directory_filtered(src_root, &filter)?
    } else {
        crate::fs_enum::enumerate_directory_deref_filtered(src_root, &filter)?
    };
    let files: Vec<_> = entries.into_iter().filter(|e| !e.is_directory).collect();

    // Collect symlinks only if preserving; else we dereference and do not send symlink frames
    let mut symlinks: Vec<(std::path::PathBuf, std::path::PathBuf)> = Vec::new();
    if preserve_links {
        let exclude_all_links = args.xj;
        let exclude_dir_links = args.xjd;
        let exclude_file_links = args.xjf;
        for ent in walkdir::WalkDir::new(src_root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if ent.file_type().is_symlink() {
                if exclude_all_links {
                    continue;
                }
                let path = ent.path().to_path_buf();
                let target = match std::fs::read_link(&path) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                // Determine if this symlink targets a directory or a file (best-effort)
                let target_is_dir = std::fs::metadata(&path).map(|m| m.is_dir()).unwrap_or(false);
                if (target_is_dir && exclude_dir_links) || (!target_is_dir && exclude_file_links) {
                    continue;
                }
                symlinks.push((path, target));
            }
        }
    }
    let total_files = files.len() as u64;

    // Partition into small, medium, and large files
    let mut small_files = Vec::new();
    let mut medium_files = Vec::new();
    let mut large_files = Vec::new();
    for fe in files {
        if fe.size < 1_048_576 {
            // 1MB
            small_files.push(fe);
        } else if fe.size <= 104_857_600 {
            // 100MB
            medium_files.push(fe);
        } else {
            large_files.push(fe);
        }
    }

    // Respect --no-tar for daemon client: handle small files individually (via parallel path)
    if args.no_tar {
        medium_files.extend(small_files.into_iter());
        small_files = Vec::new();
    }

    let total_bytes: u64 = small_files.iter().map(|e| e.size).sum::<u64>()
        + medium_files.iter().map(|e| e.size).sum::<u64>()
        + large_files.iter().map(|e| e.size).sum::<u64>();

    println!(
        "Sending {} files ({:.2} GB) to {}",
        total_files,
        total_bytes as f64 / 1_073_741_824.0,
        dest.display()
    );
    let spinner = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let mut spin_idx = 0usize;
    let mut last_tick = Instant::now();
    let tick = Duration::from_millis(250);
    let sent_files = Arc::new(Mutex::new(0u64));
    let sent_bytes = Arc::new(Mutex::new(0u64));

    // Manifest handshake: send inventory (files + symlinks + directories), receive need list
    // Build and send manifest
    {
        let mut s = stream.lock().unwrap();
        write_frame(&mut s, FrameType::ManifestStart, &[])?;
        use std::time::UNIX_EPOCH;
        // Files
        for fe in small_files.iter().chain(medium_files.iter()).chain(large_files.iter()) {
            let rel = fe.path.strip_prefix(src_root).unwrap_or(&fe.path);
            let rels = rel.to_string_lossy();
            let md = std::fs::metadata(&fe.path)?;
            let mtime = md.modified()?
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let mut pl = Vec::with_capacity(1 + 2 + rels.len() + 8 + 8);
            pl.push(0u8); // kind=file
            pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
            pl.extend_from_slice(rels.as_bytes());
            pl.extend_from_slice(&fe.size.to_le_bytes());
            pl.extend_from_slice(&mtime.to_le_bytes());
            write_frame(&mut s, FrameType::ManifestEntry, &pl)?;
        }
        // Symlinks
        for (spath, target) in &symlinks {
            let rel = spath.strip_prefix(src_root).unwrap_or(spath);
            let rels = rel.to_string_lossy();
            let targ = target.to_string_lossy();
            let mut pl = Vec::with_capacity(1 + 2 + rels.len() + 2 + targ.len());
            pl.push(1u8); // kind=symlink
            pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
            pl.extend_from_slice(rels.as_bytes());
            pl.extend_from_slice(&(targ.len() as u16).to_le_bytes());
            pl.extend_from_slice(targ.as_bytes());
            write_frame(&mut s, FrameType::ManifestEntry, &pl)?;
        }
        // Directories (to ensure empty directories are created on the server)
        if include_empty {
            for ent in walkdir::WalkDir::new(src_root).follow_links(false).into_iter().filter_map(|e| e.ok()) {
                if ent.file_type().is_dir() {
                    let rel = ent.path().strip_prefix(src_root).unwrap_or(ent.path());
                    if rel.as_os_str().is_empty() { continue; } // skip root itself
                    let rels = rel.to_string_lossy();
                    let mut pl = Vec::with_capacity(1 + 2 + rels.len());
                    pl.push(2u8); // kind=directory
                    pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                    pl.extend_from_slice(rels.as_bytes());
                    write_frame(&mut s, FrameType::ManifestEntry, &pl)?;
                }
            }
        }
        write_frame(&mut s, FrameType::ManifestEnd, &[])?;
        // Read need list
        let (tneed, plneed) = read_frame(&mut s)?;
        if tneed != FrameType::NeedList as u8 { anyhow::bail!("server did not reply NeedList"); }
        let mut need = std::collections::HashSet::new();
        if plneed.len() >= 4 {
            let mut off = 0usize;
            let _cnt = u32::from_le_bytes(plneed[off..off+4].try_into().unwrap()) as usize; off+=4;
            while off + 2 <= plneed.len() {
                let nlen = u16::from_le_bytes(plneed[off..off+2].try_into().unwrap()) as usize; off+=2;
                if off + nlen > plneed.len() { break; }
                let s = std::str::from_utf8(&plneed[off..off+nlen]).unwrap_or("").to_string(); off+=nlen;
                need.insert(s);
            }
        }
        drop(s);
        // Filter small/medium/large sets by need
        let mut filter_vec = |v: &mut Vec<crate::fs_enum::FileEntry>| {
            v.retain(|fe: &crate::fs_enum::FileEntry| {
                let rel = fe.path.strip_prefix(src_root).unwrap_or(&fe.path);
                need.contains(&rel.to_string_lossy().to_string())
            });
        };
        filter_vec(&mut small_files);
        filter_vec(&mut medium_files);
        filter_vec(&mut large_files);
        // Also filter symlinks
        symlinks.retain(|(p, _)| {
            let rel = p.strip_prefix(src_root).unwrap_or(p);
            need.contains(&rel.to_string_lossy().to_string())
        });
    }

    // If any small files, stream them via tar frames first (unless --no-tar)
    if !args.no_tar && !small_files.is_empty() {
        let mut s = stream.lock().unwrap();
        write_frame(&mut s, FrameType::TarStart, &[])?;
        struct FrameWriter<'a> {
            stream: &'a mut TcpStream,
            buf: Vec<u8>,
        }
        impl<'a> FrameWriter<'a> {
            fn new(stream: &'a mut TcpStream) -> Self {
                Self {
                    stream,
                    buf: Vec::with_capacity(1024 * 1024),
                }
            }
            fn flush(&mut self) -> Result<()> {
                if !self.buf.is_empty() {
                    write_frame(self.stream, FrameType::TarData, &self.buf)?;
                    self.buf.clear();
                }
                Ok(())
            }
        }
        impl<'a> std::io::Write for FrameWriter<'a> {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                let mut rem = b;
                while !rem.is_empty() {
                    let space = 1024 * 1024 - self.buf.len();
                    let take = space.min(rem.len());
                    self.buf.extend_from_slice(&rem[..take]);
                    rem = &rem[take..];
                    if self.buf.len() == 1024 * 1024 {
                        write_frame(self.stream, FrameType::TarData, &self.buf)
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
                        self.buf.clear();
                    }
                }
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                FrameWriter::flush(self)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
            }
        }
        let mut fw = FrameWriter::new(&mut s);
        {
            let mut builder = tar::Builder::new(&mut fw);
            for fe in &small_files {
                let rel = fe.path.strip_prefix(src_root).unwrap_or(&fe.path);
                builder.append_path_with_name(&fe.path, rel)?;
            }
            builder.finish()?;
        }
        fw.flush()?;
        write_frame(&mut s, FrameType::TarEnd, &[])?;
        let (t_ok, _) = read_frame(&mut s)?;
        if t_ok != FrameType::Ok as u8 {
            anyhow::bail!("server TAR error");
        }
        // Update counters to include tar-streamed files/bytes
        {
            let mut sf = sent_files.lock().unwrap();
            *sf += small_files.len() as u64;
        }
        {
            let bytes: u64 = small_files.iter().map(|e| e.size).sum();
            let mut sb = sent_bytes.lock().unwrap();
            *sb += bytes;
        }
    }

    // Send symlinks first (individually)
    for (spath, target) in &symlinks {
        let rel = spath.strip_prefix(src_root).unwrap_or(spath);
        let rels = rel.to_string_lossy();
        let targ = target.to_string_lossy();
        let mut pl = Vec::with_capacity(2 + rels.len() + 2 + targ.len());
        pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
        pl.extend_from_slice(rels.as_bytes());
        pl.extend_from_slice(&(targ.len() as u16).to_le_bytes());
        pl.extend_from_slice(targ.as_bytes());
        let mut s = stream.lock().unwrap();
        write_frame(&mut s, FrameType::Symlink, &pl)?;
        drop(s);
        let mut sf = sent_files.lock().unwrap();
        *sf += 1;
        if args.progress { println!("{} → (symlink) {}", spath.display(), rels); }
    }

    // Send medium files in parallel
    let medium_files_arc = Arc::new(Mutex::new(medium_files));
    let mut handles = vec![];
    for i in 0..4 {
        let medium_files = Arc::clone(&medium_files_arc);
        let stream = Arc::clone(&stream);
        let src_root = src_root.to_path_buf();
        let sent_files = Arc::clone(&sent_files);
        let sent_bytes = Arc::clone(&sent_bytes);
        let progress = args.progress;

        let handle = thread::spawn(move || -> Result<()> {
            let mut last_bytes = 0;
            let mut last_tick = Instant::now();
            let mut spin_idx = 0;
            let spinner = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
            loop {
                let fe = {
                    let mut files = medium_files.lock().unwrap();
                    if files.is_empty() {
                        break;
                    }
                    files.remove(0)
                };

                let rel = fe.path.strip_prefix(&src_root).unwrap_or(&fe.path);
                let dest_rel = rel.to_string_lossy();

                let mut pl = Vec::with_capacity(1 + 2 + dest_rel.len() + 8 + 8);
                pl.push(i as u8); // stream_id
                pl.extend_from_slice(&(dest_rel.len() as u16).to_le_bytes());
                pl.extend_from_slice(dest_rel.as_bytes());
                pl.extend_from_slice(&fe.size.to_le_bytes());
                let md = std::fs::metadata(&fe.path)?;
                let mtime = md.modified()?.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
                pl.extend_from_slice(&mtime.to_le_bytes());

                {
                    let mut s = stream.lock().unwrap();
                    write_frame(&mut s, FrameType::PFileStart, &pl)?;
                }

                let mut f = File::open(&fe.path)?;
                let mut buf = vec![0u8; 1024 * 1024];
                loop {
                    let n = f.read(&mut buf)?;
                    if n == 0 {
                        break;
                    }
                    let mut pl = Vec::with_capacity(1 + n);
                    pl.push(i as u8);
                    pl.extend_from_slice(&buf[..n]);
                    {
                        let mut s = stream.lock().unwrap();
                        write_frame(&mut s, FrameType::PFileData, &pl)?;
                    }
                    let mut sb = sent_bytes.lock().unwrap();
                    *sb += n as u64;
                    if !progress && last_tick.elapsed() >= Duration::from_millis(250) {
                        let sf = sent_files.lock().unwrap();
                        let current_bytes = *sb;
                        let rate = (current_bytes - last_bytes) as f64
                            / last_tick.elapsed().as_secs_f64()
                            / 1_048_576.0;
                        // Live heartbeat: spinner, files sent, total MB, and MB/s
                        print!(
                            "\r{} sent {} files, {:.2} MB ({:.2} MB/s)",
                            spinner[spin_idx],
                            *sf,
                            current_bytes as f64 / 1_048_576.0,
                            rate
                        );
                        let _ = stdout().flush();
                        spin_idx = (spin_idx + 1) % spinner.len();
                        last_tick = Instant::now();
                        last_bytes = current_bytes;
                    }
                }

                let mut pl = Vec::with_capacity(1);
                pl.push(i as u8);
                {
                    let mut s = stream.lock().unwrap();
                    write_frame(&mut s, FrameType::PFileEnd, &pl)?;
                }

                let mut sf = sent_files.lock().unwrap();
                *sf += 1;
                if progress {
                    println!("{} → {} ({} bytes)", fe.path.display(), dest_rel, fe.size);
                }
            }
            Ok(())
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap()?;
    }

    // Send large files sequentially
    let mut buf = vec![0u8; 1024 * 1024];
    let mut last_bytes = *sent_bytes.lock().unwrap();
    for fe in &large_files {
        let rel = fe.path.strip_prefix(src_root).unwrap_or(&fe.path);
        let dest_rel = rel.to_string_lossy();
        let mut pl = Vec::with_capacity(2 + dest_rel.len() + 8 + 8);
        pl.extend_from_slice(&(dest_rel.len() as u16).to_le_bytes());
        pl.extend_from_slice(dest_rel.as_bytes());
        pl.extend_from_slice(&fe.size.to_le_bytes());
        let md = std::fs::metadata(&fe.path)?;
        let mtime = md.modified()?.duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
        pl.extend_from_slice(&mtime.to_le_bytes());

        let (t, _r) = {
            let mut s = stream.lock().unwrap();
            write_frame(&mut s, FrameType::FileStart, &pl)?;
            read_frame(&mut s)?
        };
        if t != FrameType::Ok as u8 {
            anyhow::bail!("server rejected FILE_START");
        }

        let mut f = File::open(&fe.path)?;
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            {
                let mut s = stream.lock().unwrap();
                write_frame(&mut s, FrameType::FileData, &buf[..n])?;
            }
            let mut sb = sent_bytes.lock().unwrap();
            *sb += n as u64;
            if !args.progress && last_tick.elapsed() >= tick {
                let sf = sent_files.lock().unwrap();
                let current_bytes = *sb;
                let rate = (current_bytes - last_bytes) as f64
                    / last_tick.elapsed().as_secs_f64()
                    / 1_048_576.0;
                print!(
                    "\r{} sent {} files, {:.2} MB ({:.2} MB/s)",
                    spinner[spin_idx],
                    *sf,
                    current_bytes as f64 / 1_048_576.0,
                    rate
                );
                let _ = stdout().flush();
                spin_idx = (spin_idx + 1) % spinner.len();
                last_tick = Instant::now();
                last_bytes = current_bytes;
            }
        }

        let (t2, _r2) = {
            let mut s = stream.lock().unwrap();
            write_frame(&mut s, FrameType::FileEnd, &[])?;
            read_frame(&mut s)?
        };
        if t2 != FrameType::Ok as u8 {
            anyhow::bail!("server FILE_END error");
        }

        let mut sf = sent_files.lock().unwrap();
        *sf += 1;
        if args.progress {
            println!("{} → {} ({} bytes)", fe.path.display(), dest_rel, fe.size);
        }
    }

    // DONE
    {
        let mut s = stream.lock().unwrap();
        write_frame(&mut s, FrameType::Done, &[])?;
        let (t3, _r3) = read_frame(&mut s)?;
        if t3 != FrameType::Ok as u8 {
            anyhow::bail!("server DONE error");
        }
    }
    if !args.progress {
        // Clear the carriage-returned spinner/status line and any trailing characters
        print!("\r\x1b[K");
        let _ = stdout().flush();
        let sf = sent_files.lock().unwrap();
        let sb = sent_bytes.lock().unwrap();
        println!("✓ sent {} files, {:.2} MB", *sf, *sb as f64 / 1_048_576.0);
    }
    Ok(())
}

pub fn client_pull(host: &str, port: u16, src: &Path, dest_root: &Path, args: &crate::Args) -> Result<()> {
    let addr = format!("{}:{}", host, port);
    print!("Connecting {}... ", addr);
    let _ = stdout().flush();
    let stream = TcpStream::connect(&addr).with_context(|| format!("connect {}", addr))?;
    let stream = Arc::new(Mutex::new(stream));

    // START payload: path on server (src) + flags (mirror + pull + include_empty_dirs)
    let src_s = src.to_string_lossy();
    let mut payload = Vec::with_capacity(2 + src_s.len() + 1);
    payload.extend_from_slice(&(src_s.len() as u16).to_le_bytes());
    payload.extend_from_slice(src_s.as_bytes());
    let mut flags: u8 = 0;
    if args.mirror || args.delete { flags |= 0b0000_0001; }
    flags |= 0b0000_0010; // pull
    let include_empty = if args.mirror || args.delete { true } else if args.subdirs || args.no_empty_dirs { false } else if args.empty_dirs { true } else { true };
    if include_empty { flags |= 0b0000_0100; }
    payload.push(flags);
    {
        let mut s = stream.lock().unwrap();
        write_frame(&mut s, FrameType::Start, &payload)?;
        let (typ, resp) = read_frame(&mut s)?;
        if typ != FrameType::Ok as u8 { anyhow::bail!("daemon error: {}", String::from_utf8_lossy(&resp)); }
        // Send manifest of local destination to allow delta
        write_frame(&mut s, FrameType::ManifestStart, &[])?;
        use std::time::UNIX_EPOCH;
        let filter = crate::fs_enum::FileFilter { exclude_files: args.exclude_files.clone(), exclude_dirs: args.exclude_dirs.clone(), min_size: None, max_size: None, include_empty_dirs: true };
        let entries = crate::fs_enum::enumerate_directory_filtered(dest_root, &filter)?;
        for fe in entries.into_iter().filter(|e| !e.is_directory) {
            let rel = fe.path.strip_prefix(dest_root).unwrap_or(&fe.path);
            let rels = rel.to_string_lossy();
            let md = std::fs::metadata(&fe.path)?;
            let mtime = md.modified()?.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs() as i64;
            let mut pl = Vec::with_capacity(1 + 2 + rels.len() + 8 + 8);
            pl.push(0u8);
            pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
            pl.extend_from_slice(rels.as_bytes());
            pl.extend_from_slice(&fe.size.to_le_bytes());
            pl.extend_from_slice(&mtime.to_le_bytes());
            write_frame(&mut s, FrameType::ManifestEntry, &pl)?;
        }
        for ent in walkdir::WalkDir::new(dest_root).follow_links(false).into_iter().filter_map(|e| e.ok()) {
            if ent.file_type().is_symlink() {
                if let Ok(t) = std::fs::read_link(ent.path()) {
                    let rel = ent.path().strip_prefix(dest_root).unwrap_or(ent.path());
                    let rels = rel.to_string_lossy();
                    let targ = t.to_string_lossy();
                    let mut pl = Vec::with_capacity(1 + 2 + rels.len() + 2 + targ.len());
                    pl.push(1u8);
                    pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                    pl.extend_from_slice(rels.as_bytes());
                    pl.extend_from_slice(&(targ.len() as u16).to_le_bytes());
                    pl.extend_from_slice(targ.as_bytes());
                    write_frame(&mut s, FrameType::ManifestEntry, &pl)?;
                }
            }
        }
        write_frame(&mut s, FrameType::ManifestEnd, &[])?;
        let (_tneed, _plneed) = read_frame(&mut s)?;
    }
    println!("ok");

    // Minimal heartbeat/progress
    let spinner = ['⠋','⠙','⠹','⠸','⠼','⠴','⠦','⠧','⠇','⠏'];
    let mut spin_idx = 0usize;
    let mut last_tick = std::time::Instant::now();
    let tick = std::time::Duration::from_millis(250);
    let mut files_recv: u64 = 0;
    let mut bytes_recv: u64 = 0;
    let mut transferred_any = false;

    use std::collections::HashSet as HSet;
    let mut expected: HSet<PathBuf> = HSet::new();
    loop {
        let (t, pl) = { let mut s = stream.lock().unwrap(); read_frame(&mut s)? };
        match t {
            x if x == FrameType::MkDir as u8 => {
                if pl.len() < 2 { anyhow::bail!("bad MKDIR"); }
                let nlen = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                if pl.len() < 2 + nlen { anyhow::bail!("bad MKDIR payload"); }
                let rel = std::str::from_utf8(&pl[2..2+nlen]).context("utf8 dir path")?;
                let dir_path = dest_root.join(rel);
                std::fs::create_dir_all(&dir_path).ok();
                expected.insert(dir_path);
            }
            x if x == FrameType::Symlink as u8 => {
                if pl.len() < 2 { anyhow::bail!("bad SYMLINK"); }
                let nlen = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                if pl.len() < 2 + nlen + 2 { anyhow::bail!("bad SYMLINK payload"); }
                let rel = std::str::from_utf8(&pl[2..2 + nlen]).context("utf8 sym path")?;
                let off = 2 + nlen;
                let tlen = u16::from_le_bytes([pl[off], pl[off + 1]]) as usize;
                if pl.len() < off + 2 + tlen { anyhow::bail!("bad SYMLINK target len"); }
                let target = std::str::from_utf8(&pl[off + 2..off + 2 + tlen]).context("utf8 sym target")?;
                let dst_path = dest_root.join(rel);
                if let Some(parent) = dst_path.parent() { std::fs::create_dir_all(parent).ok(); }
                #[cfg(unix)]
                {
                    let _ = std::fs::remove_file(&dst_path);
                    std::os::unix::fs::symlink(target, &dst_path)
                        .with_context(|| format!("symlink {} -> {}", dst_path.display(), target))?;
                }
                files_recv += 1;
                transferred_any = true;
                expected.insert(dst_path);
            }
            x if x == FrameType::FileStart as u8 => {
                if pl.len() < 2 + 8 + 8 { anyhow::bail!("bad FILE_START"); }
                let nlen = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                if pl.len() < 2 + nlen + 8 + 8 { anyhow::bail!("bad FILE_START len"); }
                let rel = std::str::from_utf8(&pl[2..2 + nlen]).context("utf8 path")?;
                let mut off = 2 + nlen;
                let size = u64::from_le_bytes(pl[off..off + 8].try_into().unwrap()); off += 8;
                let mtime = i64::from_le_bytes(pl[off..off + 8].try_into().unwrap());
                let dst_path = dest_root.join(rel);
                if let Some(parent) = dst_path.parent() { std::fs::create_dir_all(parent).ok(); }
                let mut f = File::create(&dst_path).with_context(|| format!("create {}", dst_path.display()))?;
                f.set_len(size).ok();
                MTIME_STORE.with(|mt| { let mut m = mt.borrow_mut(); m.insert(dst_path.to_string_lossy().to_string(), mtime); });
                let mut written = 0u64;
                loop {
                    let (t2, pl2) = { let mut s = stream.lock().unwrap(); read_frame(&mut s)? };
                    if t2 == FrameType::FileData as u8 { f.write_all(&pl2)?; written += pl2.len() as u64; }
                    else if t2 == FrameType::FileEnd as u8 { break; }
                    else { anyhow::bail!("unexpected frame during file data: {}", t2); }
                    bytes_recv += pl2.len() as u64;
                    if !args.progress && last_tick.elapsed() >= tick {
                        print!("\r{} received {} files, {:.2} MB", spinner[spin_idx], files_recv, bytes_recv as f64 / 1_048_576.0);
                        let _ = stdout().flush();
                        spin_idx = (spin_idx + 1) % spinner.len();
                        last_tick = std::time::Instant::now();
                    }
                }
                if written != size { eprintln!("short download: {} {}/{}", dst_path.display(), written, size); }
                apply_preserved_mtime(&dst_path)?;
                files_recv += 1;
                transferred_any = true;
                expected.insert(dst_path);
            }
            x if x == FrameType::Done as u8 => {
                let mut s = stream.lock().unwrap();
                write_frame(&mut s, FrameType::Ok, b"OK")?;
                break;
            }
            _ => anyhow::bail!("unexpected frame in pull: {}", t),
        }
    }
    if args.mirror {
        // Delete local extras not present in expected set (files); then clean empty dirs
        let mut all_dirs: Vec<PathBuf> = Vec::new();
        for e in walkdir::WalkDir::new(dest_root).into_iter().filter_map(|e| e.ok()) {
            let p = e.path().to_path_buf();
            if e.file_type().is_dir() { all_dirs.push(p); continue; }
            if e.file_type().is_file() || e.file_type().is_symlink() {
                if !expected.contains(&p) { let _ = std::fs::remove_file(&p); }
            }
        }
        all_dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
        for d in all_dirs {
            if d == *dest_root { continue; }
            if expected.contains(&d) { continue; }
            let _ = std::fs::remove_dir(&d);
        }
    }
    if !args.progress {
        print!("\r\x1b[K");
        let _ = stdout().flush();
        if transferred_any {
            println!("✓ received {} files, {:.2} MB", files_recv, bytes_recv as f64 / 1_048_576.0);
        } else {
            println!("✓ up to date");
        }
    }
    Ok(())
}
