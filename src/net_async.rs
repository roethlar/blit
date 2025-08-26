//! Experimental async (Tokio) transport scaffolding for RoboSync daemon/client.
//!
//! This module is not yet wired into the CLI. It provides minimal, compiling
//! stubs and a basic async server accept loop to start iterating toward the
//! TODO.md P0 goal of refactoring network I/O to Tokio.

use anyhow::{Context, Result};
use std::time::Instant;

#[allow(dead_code)]
pub mod server {
    use super::*;
    use robosync::protocol::{MAGIC, VERSION};
    use robosync::protocol::frame;
    use filetime::{set_file_mtime, FileTime};
    use std::collections::{HashMap, HashSet};
    use std::fs::File;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::time::{timeout, Duration};

    // MAGIC and VERSION imported from crate::protocol

    // Use centralized timeout constants and functions from protocol module
    use crate::protocol::timeouts::{write_deadline_ms, read_deadline_ms, FRAME_HEADER_MS};

    #[inline]
    async fn read_exact_timed(stream: &mut TcpStream, buf: &mut [u8], ms: u64) -> Result<()> {
        use tokio::io::AsyncReadExt;
        match timeout(Duration::from_millis(ms), async { stream.read_exact(buf).await }).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(e.into()),
            Err(_) => anyhow::bail!("read timeout ({} ms)", ms),
        }
    }

    #[inline]
    async fn write_all_timed(stream: &mut TcpStream, buf: &[u8], ms: u64) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        match timeout(Duration::from_millis(ms), async { stream.write_all(buf).await }).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(e.into()),
            Err(_) => anyhow::bail!("write timeout ({} ms)", ms),
        }
    }

    pub(crate) async fn write_frame_timed(
        stream: &mut TcpStream,
        t: u8,
        payload: &[u8],
        ms: u64,
    ) -> Result<()> {
        match timeout(Duration::from_millis(ms), async {
            let mut hdr = Vec::with_capacity(4 + 2 + 1 + 4);
            hdr.extend_from_slice(MAGIC);
            hdr.extend_from_slice(&VERSION.to_le_bytes());
            hdr.push(t);
            hdr.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            stream.write_all(&hdr).await?;
            if !payload.is_empty() {
                stream.write_all(payload).await?;
            }
            Ok(())
        })
        .await
        {
            Ok(result) => result,
            Err(_) => anyhow::bail!("frame write timeout ({} ms)", ms),
        }
    }

    pub(crate) async fn write_frame(stream: &mut TcpStream, t: u8, payload: &[u8]) -> Result<()> {
        let ms = write_deadline_ms(payload.len());
        write_frame_timed(stream, t, payload, ms).await
    }

    pub async fn read_frame(stream: &mut TcpStream) -> Result<(u8, Vec<u8>)> {
        // Read header with base timeout
        let mut hdr = [0u8; 11];
        match timeout(Duration::from_millis(FRAME_HEADER_MS), async {
            stream.read_exact(&mut hdr).await
        })
        .await
        {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => anyhow::bail!("frame header timeout ({} ms)", FRAME_HEADER_MS),
        }
        if &hdr[0..4] != MAGIC {
            anyhow::bail!("bad magic");
        }
        let ver = u16::from_le_bytes([hdr[4], hdr[5]]);
        if ver != VERSION {
            anyhow::bail!("protocol version mismatch: got {}, need {}", ver, VERSION);
        }
        let typ = hdr[6];
        let len = u32::from_le_bytes([hdr[7], hdr[8], hdr[9], hdr[10]]) as usize;
        if len > crate::protocol::MAX_FRAME_SIZE {
            anyhow::bail!("frame too large: {} bytes (max: {} bytes)", len, crate::protocol::MAX_FRAME_SIZE);
        }
        let mut payload = vec![0u8; len];
        if len > 0 {
            let ms = read_deadline_ms(len);
            read_exact_timed(stream, &mut payload, ms).await?;
        }
        Ok((typ, payload))
    }

    pub(crate) async fn read_frame_timed(stream: &mut TcpStream, ms: u64) -> Result<(u8, Vec<u8>)> {
        match timeout(Duration::from_millis(ms), read_frame(stream)).await {
            Ok(res) => res,
            Err(_) => anyhow::bail!("frame IO timeout ({} ms)", ms),
        }
    }

    fn normalize_under_root(root: &Path, p: &Path) -> Result<PathBuf> {
        // Security: Ensure the destination stays under root (no traversal)
        use std::path::Component::{CurDir, Normal, ParentDir, Prefix, RootDir};
        
        // Reject any parent directory components for security
        for comp in p.components() {
            if matches!(comp, ParentDir) {
                anyhow::bail!("destination contains parent component");
            }
        }
        
        // Build the joined path, stripping any absolute/prefix components
        let mut joined = root.to_path_buf();
        for comp in p.components() {
            match comp {
                Normal(s) => {
                    // Additional Windows ADS defense: reject ':' in path components
                    #[cfg(windows)]
                    if s.to_string_lossy().contains(':') {
                        anyhow::bail!("path component contains colon (potential ADS attack)");
                    }
                    joined.push(s)
                },
                ParentDir => unreachable!(), // Already rejected above
                CurDir => {}
                Prefix(_) | RootDir => {} // Ignore absolute path components
            }
        }
        
        // Canonicalize root for comparison
        let canon_root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        
        // Try to canonicalize the full path (for existing files/dirs)
        if let Ok(canon) = std::fs::canonicalize(&joined) {
            if !canon.starts_with(&canon_root) {
                anyhow::bail!("destination escapes root via symlinks");
            }
            return Ok(canon);
        }
        
        // For new files, canonicalize parent and verify it's under root
        if let Some(parent) = joined.parent() {
            if let Ok(canon_parent) = std::fs::canonicalize(parent) {
                if !canon_parent.starts_with(&canon_root) {
                    anyhow::bail!("destination parent escapes root via symlinks");
                }
                // Return canonical parent + final component
                if let Some(file_name) = joined.file_name() {
                    return Ok(canon_parent.join(file_name));
                }
            }
        }
        
        // If we can't canonicalize, just ensure no escape via components
        Ok(joined)
    }

    pub async fn serve(bind: &str, root: &Path) -> Result<()> {
        let listener = TcpListener::bind(bind)
            .await
            .with_context(|| format!("bind {}", bind))?;
        eprintln!(
            "robosync async daemon listening on {} root={}",
            bind,
            root.display()
        );
        loop {
            let (mut stream, peer) = listener.accept().await?;
            let _ = stream.set_nodelay(true);
            eprintln!("async conn from {}", peer);
            // Spawn per-connection task.
            let root = root.to_path_buf();
            tokio::spawn(async move {
                if let Err(e) = async move {
                    let started = Instant::now();
                    // Expect START or LIST_REQ, reply accordingly
                        let (typ, pl) = read_frame_timed(&mut stream, 500).await?;
                        if typ == frame::LIST_REQ { // ListReq
                        // payload: path_len u16 | path bytes (relative to root)
                        if pl.len() < 2 { anyhow::bail!("bad LIST_REQ payload"); }
                        let nlen = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                        if pl.len() < 2 + nlen { anyhow::bail!("bad LIST_REQ path len"); }
                        let pstr = std::str::from_utf8(&pl[2..2 + nlen]).unwrap_or("");
                        let (parent, base) = if let Some(pos) = pstr.rfind('/') { (&pstr[..pos], &pstr[pos + 1..]) } else { ("", pstr) };
                        let parent_path = PathBuf::from(parent);
                        let base_dir = normalize_under_root(&root, &parent_path)?;
                        let mut out: Vec<u8> = Vec::new();
                        let mut items: Vec<(u8, String)> = Vec::new();
                        
                        // TUI pagination: Cap at 1000 entries to keep UI responsive
                        const MAX_LIST_ENTRIES: usize = 1000;
                        let mut entry_count = 0;
                        
                        if let Ok(rd) = std::fs::read_dir(&base_dir) {
                            for e in rd.flatten() {
                                if entry_count >= MAX_LIST_ENTRIES {
                                    // Add a special marker entry to indicate truncation
                                    items.push((2u8, format!("... ({} entries max)", MAX_LIST_ENTRIES)));
                                    break;
                                }
                                let name = e.file_name().to_string_lossy().to_string();
                                if !name.starts_with(base) { continue; }
                                let kind = match e.file_type() { Ok(ft) if ft.is_dir() => 1u8, _ => 0u8 };
                                items.push((kind, name));
                                entry_count += 1;
                            }
                        }
                        
                        // Sort entries: directories first, then files, alphabetically within each
                        items.sort_by(|a, b| {
                            match (a.0, b.0) {
                                (1, 0) => std::cmp::Ordering::Less,    // dir before file
                                (0, 1) => std::cmp::Ordering::Greater, // file after dir
                                _ => a.1.cmp(&b.1),                    // alphabetical within type
                            }
                        });
                        
                        out.extend_from_slice(&(items.len() as u32).to_le_bytes());
                        for (k, n) in items.into_iter() {
                            out.push(k);
                            out.extend_from_slice(&(n.len() as u16).to_le_bytes());
                            out.extend_from_slice(n.as_bytes());
                        }
                        // 41 == ListResp
                        write_frame(&mut stream, frame::LIST_RESP, &out).await?;
                        return Ok::<(), anyhow::Error>(());
                    }
                    // 1 == FrameType::Start in sync path
                    if typ != frame::START { anyhow::bail!("expected START frame"); }
                    // Parse destination and flags: dest_len u16 | dest_bytes | flags u8
                    let (base, flags) = if pl.len() >= 3 {
                        let n = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                        if pl.len() >= 3 + n {
                            let d = std::str::from_utf8(&pl[2..2 + n]).unwrap_or("");
                            let p = normalize_under_root(&root, Path::new(d))?;
                            let flags = pl[2 + n];
                            (p, flags)
                        } else {
                            (root.clone(), 0)
                        }
                    } else {
                        (root.clone(), 0)
                    };
                    let mirror = (flags & 0b0000_0001) != 0;
                    let pull = (flags & 0b0000_0010) != 0;
                    let include_empty_dirs = (flags & 0b0000_0100) != 0;
                    let speed = (flags & 0b0000_1000) != 0;

                    // 2 == FrameType::Ok
                    write_frame(&mut stream, frame::OK, b"OK").await?;

                    // Connection-scoped state with counters
                    struct DeltaState {
                        dst_path: PathBuf,
                        file_size: u64,
                        mtime: i64,
                        granule: u64,
                        sample: usize,
                        need_ranges: Vec<(u64,u64)>,
                    }
                    struct Connection {
                        expected_paths: HashSet<PathBuf>,
                        needed: HashSet<String>,
                        client_present: HashSet<String>,
                        p_files: HashMap<u8, (PathBuf, File, u64, u64, i64)>,
                        bytes_sent: u64,
                        bytes_received: u64,
                        files_sent: u64,
                        files_received: u64,
                        verify_batch: Vec<String>, // Collect paths for batch verification
                    }
                    let mut conn = Connection {
                        expected_paths: HashSet::new(),
                        needed: HashSet::new(),
                        client_present: HashSet::new(),
                        p_files: HashMap::new(),
                        bytes_sent: 0,
                        bytes_received: 0,
                        files_sent: 0,
                        files_received: 0,
                        verify_batch: Vec::new(),
                    };

                    // Handle frames until Done
                    let mut delta_state: Option<DeltaState> = None;
                    loop {
                        let (t, payload) = read_frame(&mut stream).await?;
                        match t {
                            // 14 == ManifestStart, 15 == ManifestEntry, 16 == ManifestEnd
                            frame::MANIFEST_START => {
                                // Compute need list from manifest
                                conn.needed.clear();
                                loop {
                                    let (ti, pli) = read_frame_timed(&mut stream, 750).await?;
                                    if ti == frame::MANIFEST_ENTRY {
                                        if pli.len() < 3 {
                                            anyhow::bail!("bad MANIFEST_ENTRY");
                                        }
                                        let kind = pli[0];
                                        let nlen = u16::from_le_bytes([pli[1], pli[2]]) as usize;
                                        if pli.len() < 3 + nlen {
                                            anyhow::bail!("bad MANIFEST path len");
                                        }
                                        let rels =
                                            std::str::from_utf8(&pli[3..3 + nlen]).unwrap_or("");
                                        let relp = Path::new(rels);
                                        let dst = normalize_under_root(&base, relp)?;
                                        // Track what client reports as present
                                        conn.client_present.insert(rels.to_string());
                                        match kind {
                                            0 => {
                                                if pli.len() < 3 + nlen + 8 + 8 {
                                                    anyhow::bail!("bad file entry");
                                                }
                                                let off = 3 + nlen;
                                                let size = u64::from_le_bytes(
                                                    pli[off..off + 8].try_into()
                                                        .context("Invalid size bytes in FILE_START")?,
                                                );
                                                let mtime = i64::from_le_bytes(
                                                    pli[off + 8..off + 16].try_into()
                                                        .context("Invalid mtime bytes in FILE_START")?,
                                                );
                                                let mut need = true;
                                                if let Ok(md) = std::fs::metadata(&dst) {
                                                    if md.is_file() {
                                                        let dsize = md.len();
                                                        let dmtime = md
                                                            .modified()
                                                            .ok()
                                                            .and_then(|t| {
                                                                t.duration_since(
                                                                    std::time::UNIX_EPOCH,
                                                                )
                                                                .ok()
                                                            })
                                                            .map(|d| d.as_secs() as i64)
                                                            .unwrap_or(0);
                                                        let dt = (dmtime - mtime).abs();
                                                        need = !(dsize == size && dt <= 2);
                                                    }
                                                }
                                                if need {
                                                    conn.needed.insert(rels.to_string());
                                                }
                                                conn.expected_paths.insert(dst);
                                            }
                                            1 => {
                                                // symlink
                                                if pli.len() < 3 + nlen + 2 {
                                                    anyhow::bail!("bad symlink entry");
                                                }
                                                let off = 3 + nlen;
                                                let tlen =
                                                    u16::from_le_bytes([pli[off], pli[off + 1]])
                                                        as usize;
                                                if pli.len() < off + 2 + tlen {
                                                    anyhow::bail!("bad symlink target len");
                                                }
                                                let target = std::str::from_utf8(
                                                    &pli[off + 2..off + 2 + tlen],
                                                )
                                                .unwrap_or("");
                                                let mut need = true;
                                                if let Ok(smd) = std::fs::symlink_metadata(&dst) {
                                                    if smd.file_type().is_symlink() {
                                                        if let Ok(cur) = std::fs::read_link(&dst) {
                                                            if cur.as_os_str()
                                                                == std::ffi::OsStr::new(target)
                                                            {
                                                                need = false;
                                                            }
                                                        }
                                                    }
                                                }
                                                if need {
                                                    conn.needed.insert(rels.to_string());
                                                }
                                                conn.expected_paths.insert(dst);
                                            }
                                            2 => {
                                                std::fs::create_dir_all(&dst).ok();
                                                conn.expected_paths.insert(dst);
                                            }
                                            _ => {}
                                        }
                                        continue;
                                    } else if ti == frame::MANIFEST_END {
                                        // Send NeedList with computed entries
                                        let mut resp = Vec::with_capacity(4 + conn.needed.len() * 8);
                                        resp.extend_from_slice(
                                            &(conn.needed.len() as u32).to_le_bytes(),
                                        );
                                        for p in &conn.needed {
                                            let b = p.as_bytes();
                                            resp.extend_from_slice(&(b.len() as u16).to_le_bytes());
                                            resp.extend_from_slice(b);
                                        }
                                        {
    let payload_len = resp.len() as u64;
    write_frame(&mut stream, frame::NEED_LIST, &resp).await?;
    conn.bytes_sent += (payload_len as u64) + 11;
}
                                                                                      // If client is pulling, send files now
                                        if pull {
                                            // Directories first
                                            if include_empty_dirs {
                                                for ent in walkdir::WalkDir::new(&base)
                                                    .follow_links(false)
                                                    .into_iter()
                                                    .filter_map(|e| e.ok())
                                                {
                                                    if ent.file_type().is_dir() {
                                                        let rel = ent
                                                            .path()
                                                            .strip_prefix(&base)
                                                            .unwrap_or(ent.path());
                                                        if rel.as_os_str().is_empty() {
                                                            continue;
                                                        }
                                                        let rels = rel.to_string_lossy();
                                                        let mut plm =
                                                            Vec::with_capacity(2 + rels.len());
                                                        plm.extend_from_slice(
                                                            &(rels.len() as u16).to_le_bytes(),
                                                        );
                                                        plm.extend_from_slice(rels.as_bytes());
                                            write_frame(&mut stream, frame::MKDIR, &plm)
                                                            .await?; // MkDir
                                                    }
                                                }
                                            }
                                            // Send needed or missing entries
                                            // 1) Partition into small-file TAR bundle and per-file sends
                                            let small_threshold: u64 = 1_000_000; // ~1MB
                                            let mut small_files: Vec<(std::path::PathBuf, String, u64, u32)> = Vec::new();
                                            let mut large_files: Vec<(std::path::PathBuf, String, u64, i64, u32)> = Vec::new();
                                            for ent in walkdir::WalkDir::new(&base)
                                                .follow_links(false)
                                                .into_iter()
                                                .filter_map(|e| e.ok())
                                            {
                                                let rel = ent
                                                    .path()
                                                    .strip_prefix(&base)
                                                    .unwrap_or(ent.path());
                                                if rel.as_os_str().is_empty() {
                                                    continue;
                                                }
                                                let rels = rel.to_string_lossy().to_string();
                                                let send_this = conn.needed.contains(&rels)
                                                    || !conn.client_present.contains(&rels);
                                                if !send_this {
                                                    continue;
                                                }
                                                if ent.file_type().is_symlink() {
                                                    if let Ok(t) = std::fs::read_link(ent.path()) {
                                                        let targ = t.to_string_lossy();
                                                        let mut pls = Vec::with_capacity(
                                                            2 + rels.len() + 2 + targ.len(),
                                                        );
                                                        pls.extend_from_slice(
                                                            &(rels.len() as u16).to_le_bytes(),
                                                        );
                                                        pls.extend_from_slice(rels.as_bytes());
                                                        pls.extend_from_slice(
                                                            &(targ.len() as u16).to_le_bytes(),
                                                        );
                                                        pls.extend_from_slice(targ.as_bytes());
                                            write_frame(&mut stream, frame::SYMLINK, &pls)
                                                            .await?; // Symlink
                                                        // Count symlink as a sent item
                                                        conn.files_sent = conn.files_sent.saturating_add(1);
                                                    }
                                                    continue;
                                                }
                                                if ent.file_type().is_file() {
                                                    match ent.metadata() {
                                                        Ok(md) => {
                                                            let size = md.len();
                                                            let mtime = md
                                                                .modified()
                                                                .ok()
                                                                .and_then(|t| t
                                                                    .duration_since(std::time::UNIX_EPOCH)
                                                                    .ok())
                                                                .map(|d| d.as_secs() as i64)
                                                                .unwrap_or(0);
                                                            #[cfg(unix)]
                                                            use std::os::unix::fs::PermissionsExt;
                                                            let mode: u32 = {
                                                                #[cfg(unix)]
                                                                { md.permissions().mode() }
                                                                #[cfg(not(unix))]
                                                                { 0 }
                                                            };
                                                            if size <= small_threshold {
                                                                small_files.push((ent.path().to_path_buf(), rels, size, mode));
                                                            } else {
                                                                large_files.push((ent.path().to_path_buf(), rels, size, mtime, mode));
                                                            }
                                                        }
                                                        Err(e) => {
                                                            eprintln!(
                                                                "async pull: metadata failed {}: {}",
                                                                ent.path().display(),
                                                                e
                                                            );
                                                            continue;
                                                        }
                                                    }
                                                }
                                            }

                                            // 2) Send TAR for small files if any
                                            if !small_files.is_empty() {
                                                // Start TAR sequence
                                                write_frame(&mut stream, frame::TAR_START, &[]).await?;
                                                let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
                                                // Writer that batches into chunks and sends via blocking_send from blocking context
                                                struct TarChanWriter {
                                                    tx: tokio::sync::mpsc::Sender<Vec<u8>>,
                                                    buf: Vec<u8>,
                                                    cap: usize,
                                                }
                                                impl std::io::Write for TarChanWriter {
                                                    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                                                        let mut rem = data;
                                                        while !rem.is_empty() {
                                                            let space = self.cap - self.buf.len();
                                                            let take = rem.len().min(space);
                                                            self.buf.extend_from_slice(&rem[..take]);
                                                            rem = &rem[take..];
                                                            if self.buf.len() >= self.cap {
                                                                let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(self.cap));
                                                                // blocking_send is fine in blocking context
                                                                self.tx.blocking_send(chunk).map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))?;
                                                            }
                                                        }
                                                        Ok(data.len())
                                                    }
                                                    fn flush(&mut self) -> std::io::Result<()> {
                                                        if !self.buf.is_empty() {
                                                            let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(self.cap));
                                                            self.tx.blocking_send(chunk).map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))?;
                                                        }
                                                        Ok(())
                                                    }
                                                }
                                                let files_for_tar = small_files.clone();
                                                let chunk_size = if speed { 2 * 1024 * 1024 } else { 1024 * 1024 };
                                                let tar_task = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                                                    let mut w = TarChanWriter { tx, buf: Vec::with_capacity(chunk_size), cap: chunk_size };
                                                    {
                                                        let mut builder = tar::Builder::new(&mut w);
                                                        // Preserve original mtimes and modes from filesystem metadata
                                                        for (src, rels, _size, _mode) in files_for_tar.into_iter() {
                                                            let relp = std::path::Path::new(&rels);
                                                            builder.append_path_with_name(&src, relp).context("append file to tar")?;
                                                        }
                                                        builder.finish().context("finish tar")?;
                                                    }
                                                    let _ = std::io::Write::flush(&mut w);
                                                    Ok(())
                                                });
                                                // Forward chunks as TAR_DATA frames
                                                while let Some(chunk) = rx.recv().await {
                                                    let frame = chunk; // already owned
                                                    write_frame(&mut stream, frame::TAR_DATA, &frame).await?;
                                                    // Do not count tar frame bytes toward logical bytes_sent to match classic metrics
                                                }
                                                // Await tar task
                                                match tar_task.await {
                                                    Ok(Ok(())) => {}
                                                    Ok(Err(e)) => anyhow::bail!("tar pack error: {}", e),
                                                    Err(e) => anyhow::bail!("tar task join error: {}", e),
                                                }
                                                // End of TAR
                                                write_frame(&mut stream, frame::TAR_END, &[]).await?;
                                                // Update counters to align with classic: sum file sizes and count files
                                                let total_small_bytes: u64 = small_files
                                                    .iter()
                                                    .map(|(_src, _rels, size, _mode)| *size)
                                                    .sum();
                                                conn.bytes_sent = conn.bytes_sent.saturating_add(total_small_bytes);
                                                conn.files_sent = conn.files_sent.saturating_add(small_files.len() as u64);
                                                // Optionally send POSIX modes explicitly for parity
                                                #[cfg(unix)]
                                                {
                                                    for (_src, rels, _size, mode) in small_files.iter() {
                                                        let mut pla = Vec::with_capacity(2 + rels.len() + 1 + 4);
                                                        pla.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                                                        pla.extend_from_slice(rels.as_bytes());
                                                        pla.push(0u8);
                                                        pla.extend_from_slice(&mode.to_le_bytes());
                                                write_frame(&mut stream, frame::SET_ATTR, &pla).await?;
                                                    }
                                                }
                                            }

                                            // 3) Send large files individually
                                            for (src, rels, size, mtime, mode) in large_files.into_iter() {
                                                let mut plf = Vec::with_capacity(2 + rels.len() + 8 + 8);
                                                plf.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                                                plf.extend_from_slice(rels.as_bytes());
                                                plf.extend_from_slice(&size.to_le_bytes());
                                                plf.extend_from_slice(&mtime.to_le_bytes());
                                            write_frame(&mut stream, frame::FILE_START, &plf).await?;
                                                // Stream data
                                                let mut f = match tokio::fs::File::open(&src).await {
                                                    Ok(f) => f,
                                                    Err(e) => { eprintln!("async pull: open failed {}: {}", src.display(), e); continue; }
                                                };
                                                let mut buf = vec![0u8; if speed { 4 * 1024 * 1024 } else { 2 * 1024 * 1024 }];
                                                loop {
                                                    use tokio::io::AsyncReadExt;
                                                    let n = match f.read(&mut buf).await {
                                                        Ok(n) => n,
                                                        Err(e) => { eprintln!("async pull: read failed {}: {}", src.display(), e); break; }
                                                    };
                                                    if n == 0 { break; }
                                                if let Err(e) = write_frame(&mut stream, frame::FILE_DATA, &buf[..n]).await {
                                                        eprintln!("async pull: send chunk failed {}: {}", src.display(), e);
                                                        return Err(e);
                                                    }
                                                    conn.bytes_sent = conn.bytes_sent.saturating_add(n as u64);
                                                }
                                            write_frame(&mut stream, frame::FILE_END, &[]).await?;
                                                conn.files_sent = conn.files_sent.saturating_add(1);
                                                // POSIX mode via SetAttr
                                                #[cfg(unix)]
                                                {
                                                    let mut pla = Vec::with_capacity(2 + rels.len() + 1 + 4);
                                                    pla.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                                                    pla.extend_from_slice(rels.as_bytes());
                                                    pla.push(0u8);
                                                    pla.extend_from_slice(&mode.to_le_bytes());
                                                    write_frame(&mut stream, frame::SET_ATTR, &pla).await?;
                                                }
                                            }
                                            // Done and wait for client OK
                                            write_frame(&mut stream, frame::DONE, &[]).await?;
                                let (tt, _plok) = read_frame_timed(&mut stream, 500).await?;
                                            if tt != frame::OK {
                                                anyhow::bail!("client did not ack DONE");
                                            }
                                            return Ok::<(), anyhow::Error>(());
                                        }
                                        break;
                                    } else {
                                        anyhow::bail!("unexpected frame during manifest: {}", ti);
                                    }
                                }
                            }
                            // 8 == TarStart, 9 == TarData, 10 == TarEnd
                            frame::TAR_START => {
                                // Streaming tar unpack without temp files.
                                // Use a bounded channel to feed a blocking unpacker.
                                let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
                                let (res_tx, res_rx) = tokio::sync::oneshot::channel::<(u64,u64)>();
                                struct ChanReader {
                                    rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
                                    buf: Vec<u8>,
                                    pos: usize,
                                    done: bool,
                                }
                                impl std::io::Read for ChanReader {
                                    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
                                        if self.done { return Ok(0); }
                                        if self.pos >= self.buf.len() {
                                            match self.rx.blocking_recv() {
                                                Some(chunk) => { self.buf = chunk; self.pos = 0; }
                                                None => { self.done = true; return Ok(0); }
                                            }
                                        }
                                        let n = out.len().min(self.buf.len() - self.pos);
                                        if n > 0 { out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]); self.pos += n; }
                                        Ok(n)
                                    }
                                }
                                let base_dir = base.clone();
                                let unpacker = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                                    let reader = ChanReader { rx, buf: Vec::new(), pos: 0, done: false };
                                    let mut ar = tar::Archive::new(reader);
                                    ar.set_overwrite(true);
                                    let mut files: u64 = 0;
                                    let mut bytes: u64 = 0;
                                    let entries = ar.entries().context("tar entries")?;
                                    for e in entries {
                                        let mut entry = e.context("tar entry")?;
                                        let et = entry.header().entry_type();
                                        if et.is_file() {
                                            if let Ok(sz) = entry.header().size() { bytes = bytes.saturating_add(sz); files = files.saturating_add(1); }
                                        }
                                        entry.unpack_in(&base_dir).context("unpack entry")?;
                                    }
                                    let _ = res_tx.send((files, bytes));
                                    Ok(())
                                });
                                // Feed frames to unpacker
                                loop {
                                    let (ti, pli) = read_frame_timed(&mut stream, 750).await?;
                                    if ti == frame::TAR_DATA {
                                        let sz = pli.len() as u64;
                                        if tx.send(pli).await.is_err() { anyhow::bail!("tar unpacker closed"); }
                                        let _ = sz; // transport bytes ignored; use logical bytes from unpacker
                                        continue;
                                    }
                                    if ti == frame::TAR_END { break; }
                                    anyhow::bail!("unexpected frame during tar: {}", ti);
                                }
                                drop(tx);
                                // Await unpack completion
                                match unpacker.await {
                                    Ok(Ok(())) => {}
                                    Ok(Err(e)) => { anyhow::bail!("tar unpack error: {}", e); }
                                    Err(join_err) => { anyhow::bail!("tar unpack task error: {}", join_err); }
                                }
                                if let Ok((files, bytes)) = res_rx.await { conn.files_received = conn.files_received.saturating_add(files); conn.bytes_received = conn.bytes_received.saturating_add(bytes); }
                                write_frame(&mut stream, frame::OK, b"TAR_OK").await?;
                            }
                            // 11 == PFileStart, 12 == PFileData, 13 == PFileEnd
                            frame::PFILE_START => {
                                if payload.len() < 1 + 2 {
                                    anyhow::bail!("bad PFILE_START");
                                }
                                let sid = payload[0];
                                let nlen = u16::from_le_bytes([payload[1], payload[2]]) as usize;
                                if payload.len() < 1 + 2 + nlen + 8 + 8 {
                                    anyhow::bail!("bad PFILE_START len");
                                }
                                let rel = std::str::from_utf8(&payload[3..3 + nlen]).unwrap_or("");
                                let mut off = 3 + nlen;
                                let size =
                                    u64::from_le_bytes(payload[off..off + 8].try_into()
                                        .context("Invalid size bytes in FILE_START")?);
                                off += 8;
                                let mtime =
                                    i64::from_le_bytes(payload[off..off + 8].try_into()
                                        .context("Invalid mtime bytes in FILE_START")?);
                                let dst = normalize_under_root(&base, Path::new(rel))?;
                                if let Some(parent) = dst.parent() {
                                    std::fs::create_dir_all(parent).ok();
                                }
                                let f = File::create(&dst)
                                    .with_context(|| format!("create {}", dst.display()))?;
                                let _ = f.set_len(size);
                                conn.expected_paths.insert(dst.clone());
                                conn.p_files.insert(sid, (dst, f, size, 0, mtime));
                            }
                            frame::PFILE_DATA => {
                                if payload.len() < 1 {
                                    anyhow::bail!("bad PFILE_DATA");
                                }
                                let sid = payload[0];
                                if let Some((_p, f, _sz, ref mut written, _mt)) =
                                    conn.p_files.get_mut(&sid)
                                {
                                    let data = &payload[1..];
                                    let is_zero = data.iter().all(|&b| b == 0);
                                    if is_zero && data.len() >= 128 * 1024 {
                                        use std::io::{Seek, SeekFrom};
                                        let _ = f.seek(SeekFrom::Current(data.len() as i64));
                                    } else {
                                        f.write_all(data).context("write PFILE_DATA")?;
                                    }
                                    *written += data.len() as u64;
                                    conn.bytes_received = conn.bytes_received.saturating_add(data.len() as u64);
                                } else {
                                    anyhow::bail!("PFILE_DATA unknown stream");
                                }
                            }
                            frame::PFILE_END => {
                                if payload.len() < 1 {
                                    anyhow::bail!("bad PFILE_END");
                                }
                                let sid = payload[0];
                                if let Some((p, _f, sz, written, mt)) = conn.p_files.remove(&sid) {
                                    if written != sz {
                                        eprintln!(
                                            "async short write: {} {}/{}",
                                            p.display(),
                                            written,
                                            sz
                                        );
                                    }
                                    let ft = FileTime::from_unix_time(mt, 0);
                                    let _ = set_file_mtime(&p, ft);
                                    conn.files_received = conn.files_received.saturating_add(1);
                                }
                            }
                            // 19 == MkDir
                            19u8 => {
                                if payload.len() < 2 {
                                    anyhow::bail!("bad MKDIR");
                                }
                                let nlen = u16::from_le_bytes([payload[0], payload[1]]) as usize;
                                if payload.len() < 2 + nlen {
                                    anyhow::bail!("bad MKDIR payload");
                                }
                                let rel = std::str::from_utf8(&payload[2..2 + nlen]).unwrap_or("");
                                let dir_path = normalize_under_root(&base, Path::new(rel))?;
                                let _ = std::fs::create_dir_all(&dir_path);
                                conn.expected_paths.insert(dir_path);
                            }
                            // 18 == Symlink
                            frame::SYMLINK => {
                                if payload.len() < 2 {
                                    anyhow::bail!("bad SYMLINK");
                                }
                                let nlen = u16::from_le_bytes([payload[0], payload[1]]) as usize;
                                if payload.len() < 2 + nlen + 2 {
                                    anyhow::bail!("bad SYMLINK payload");
                                }
                                let rel = std::str::from_utf8(&payload[2..2 + nlen]).unwrap_or("");
                                let off = 2 + nlen;
                                let tlen =
                                    u16::from_le_bytes([payload[off], payload[off + 1]]) as usize;
                                if payload.len() < off + 2 + tlen {
                                    anyhow::bail!("bad SYMLINK target len");
                                }
                                let target = std::str::from_utf8(&payload[off + 2..off + 2 + tlen])
                                    .unwrap_or("");
                                let dst_path = normalize_under_root(&base, Path::new(rel))?;
                                if let Some(parent) = dst_path.parent() {
                                    std::fs::create_dir_all(parent).ok();
                                }
                                #[cfg(unix)]
                                {
                                    let _ = std::fs::remove_file(&dst_path);
                                    std::os::unix::fs::symlink(target, &dst_path).ok();
                                }
                                #[cfg(windows)]
                                {
                                    let _ = std::fs::remove_file(&dst_path);
                                    let _ = std::fs::remove_dir(&dst_path);
                                    let _ = robosync::win_fs::create_symlink(Path::new(target), &dst_path);
                                }
                                conn.expected_paths.insert(dst_path);
                                conn.files_received = conn.files_received.saturating_add(1);
                            }
                            // 30 == SetAttr (flags + optional POSIX mode)
                            frame::SET_ATTR => {
                                if payload.len() < 2 + 1 {
                                    anyhow::bail!("bad SET_ATTR");
                                }
                                let nlen = u16::from_le_bytes([payload[0], payload[1]]) as usize;
                                if payload.len() < 2 + nlen + 1 {
                                    anyhow::bail!("bad SET_ATTR payload");
                                }
                                let rel = std::str::from_utf8(&payload[2..2 + nlen]).unwrap_or("");
                                let flags = payload[2 + nlen];
                                let dst = normalize_under_root(&base, Path::new(rel))?;
                                #[cfg(windows)]
                                {
                                    use std::os::windows::ffi::OsStrExt;
                                    use windows::core::PCWSTR;
                                    use windows::Win32::Storage::FileSystem::{
                                        GetFileAttributesW, SetFileAttributesW,
                                        FILE_ATTRIBUTE_READONLY, FILE_FLAGS_AND_ATTRIBUTES,
                                    };
                                    let wide: Vec<u16> = dst
                                        .as_os_str()
                                        .encode_wide()
                                        .chain(std::iter::once(0))
                                        .collect();
                                    unsafe {
                                        let mut attrs = GetFileAttributesW(PCWSTR(wide.as_ptr()));
                                        if attrs == u32::MAX {
                                            attrs = 0;
                                        }
                                        if (flags & 0b0000_0001) != 0 {
                                            attrs |= FILE_ATTRIBUTE_READONLY.0;
                                        } else {
                                            attrs &= !FILE_ATTRIBUTE_READONLY.0;
                                        }
                                        let _ = SetFileAttributesW(
                                            PCWSTR(wide.as_ptr()),
                                            FILE_FLAGS_AND_ATTRIBUTES(attrs),
                                        );
                                    }
                                }
                                #[cfg(unix)]
                                {
                                    use std::os::unix::fs::PermissionsExt;
                                    // Optional POSIX mode trailing (u32 LE)
                                    if payload.len() >= 2 + nlen + 1 + 4 {
                                        let off = 2 + nlen + 1;
                                        let mode = u32::from_le_bytes([
                                            payload[off],
                                            payload[off + 1],
                                            payload[off + 2],
                                            payload[off + 3],
                                        ]);
                                        let _ = std::fs::set_permissions(
                                            &dst,
                                            std::fs::Permissions::from_mode(mode),
                                        );
                                    }
                                }
                                let _ = flags;
                            }
                            // 31 == VerifyReq - collect for batch processing
                            frame::VERIFY_REQ => {
                                if payload.len() < 2 {
                                    anyhow::bail!("bad VERIFY_REQ");
                                }
                                let nlen = u16::from_le_bytes([payload[0], payload[1]]) as usize;
                                if payload.len() < 2 + nlen {
                                    anyhow::bail!("bad VERIFY_REQ len");
                                }
                                let rel = std::str::from_utf8(&payload[2..2 + nlen]).unwrap_or("");
                                
                                // Add to batch for processing when VERIFY_DONE is received
                                conn.verify_batch.push(rel.to_string());
                                
                                // Note: We don't send a response yet - wait for VERIFY_DONE
                            }
                            
                            // 33 == VerifyDone - process all batched verify requests
                            frame::VERIFY_DONE => {
                                // Process all batched verify requests
                                for rel in &conn.verify_batch {
                                    let path = normalize_under_root(&base, Path::new(rel))?;
                                    
                                    // Build response: [status:1][pathlen:2][path:n][hash:32]
                                    // Status: 0=OK, 1=NOT_FOUND, 2=ERROR
                                    let mut response = Vec::new();
                                    
                                    match std::fs::File::open(&path) {
                                        Ok(mut f) => {
                                            // File exists and is readable
                                            response.push(0u8); // Status: OK
                                            
                                            // Echo back the path
                                            let path_bytes = rel.as_bytes();
                                            response.extend_from_slice(&(path_bytes.len() as u16).to_le_bytes());
                                            response.extend_from_slice(path_bytes);
                                            
                                            // Compute and append hash
                                            let mut hasher = blake3::Hasher::new();
                                            let mut buf = vec![0u8; 4 * 1024 * 1024];
                                            loop {
                                                use std::io::Read as _;
                                                match f.read(&mut buf) {
                                                    Ok(0) => break,
                                                    Ok(n) => { hasher.update(&buf[..n]); },
                                                    Err(_) => {
                                                        // Read error during hashing
                                                        response[0] = 2; // Change status to ERROR
                                                        response.extend_from_slice(&[0u8; 32]); // Zero hash
                                                        break;
                                                    }
                                                }
                                            }
                                            
                                            if response[0] == 0 {
                                                let hash = hasher.finalize();
                                                response.extend_from_slice(hash.as_bytes());
                                            }
                                        }
                                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                            // File doesn't exist
                                            response.push(1u8); // Status: NOT_FOUND
                                            
                                            // Echo back the path
                                            let path_bytes = rel.as_bytes();
                                            response.extend_from_slice(&(path_bytes.len() as u16).to_le_bytes());
                                            response.extend_from_slice(path_bytes);
                                            
                                            // No hash for missing files
                                            response.extend_from_slice(&[0u8; 32]);
                                        }
                                        Err(_) => {
                                            // Other error (permissions, etc.)
                                            response.push(2u8); // Status: ERROR
                                            
                                            // Echo back the path
                                            let path_bytes = rel.as_bytes();
                                            response.extend_from_slice(&(path_bytes.len() as u16).to_le_bytes());
                                            response.extend_from_slice(path_bytes);
                                            
                                            // No hash on error
                                            response.extend_from_slice(&[0u8; 32]);
                                        }
                                    }
                                    
                                    // Send each response
                                    write_frame(&mut stream, frame::VERIFY_HASH, &response).await?;
                                }
                                
                                // Clear the batch
                                conn.verify_batch.clear();
                                
                                // Send DONE to signal batch complete
                                write_frame(&mut stream, frame::DONE, &[]).await?;
                            }
                            // 29 == FileRawStart (followed by raw body bytes, not framed)
                            frame::FILE_RAW_START => {
                                if payload.len() < 2 + 8 + 8 {
                                    anyhow::bail!("bad FILE_RAW_START");
                                }
                                let nlen = u16::from_le_bytes([payload[0], payload[1]]) as usize;
                                if payload.len() < 2 + nlen + 8 + 8 {
                                    anyhow::bail!("bad FILE_RAW_START len");
                                }
                                let rel = std::str::from_utf8(&payload[2..2 + nlen]).unwrap_or("");
                                let mut off = 2 + nlen;
                                let size =
                                    u64::from_le_bytes(payload[off..off + 8].try_into()
                                        .context("Invalid size bytes in FILE_START")?);
                                off += 8;
                                let mtime =
                                    i64::from_le_bytes(payload[off..off + 8].try_into()
                                        .context("Invalid mtime bytes in FILE_START")?);
                                let dst = normalize_under_root(&base, Path::new(rel))?;
                                if let Some(parent) = dst.parent() {
                                    std::fs::create_dir_all(parent).ok();
                                }
                                let mut f = File::create(&dst)
                                    .with_context(|| format!("create {}", dst.display()))?;
                                let _ = f.set_len(size);
                                // Read raw body bytes directly from stream (sparse-friendly) with deadlines
                                let mut remaining = size as usize;
                                let mut buf = vec![0u8; 4 * 1024 * 1024];
                                use std::io::{Seek, SeekFrom};
                                while remaining > 0 {
                                    let to_read = remaining.min(buf.len());
                                    let ms = crate::protocol::timeouts::READ_BASE_MS + ((to_read as u64 + 1_048_575) / 1_048_576) * crate::protocol::timeouts::PER_MB_MS;
                                    let n = match timeout(Duration::from_millis(ms), async { stream.read(&mut buf[..to_read]).await }).await {
                                        Ok(Ok(n)) => n,
                                        Ok(Err(e)) => return Err(e.into()),
                                        Err(_) => anyhow::bail!("raw body read timeout"),
                                    };
                                    if n == 0 {
                                        anyhow::bail!("unexpected EOF during raw file body");
                                    }
                                    let is_zero = buf[..n].iter().all(|&b| b == 0);
                                    if is_zero && n >= 128 * 1024 {
                                        let _ = f.seek(SeekFrom::Current(n as i64));
                                    } else {
                                        f.write_all(&buf[..n]).context("write raw file body")?;
                                    }
                                    remaining -= n;
                                    conn.bytes_received = conn.bytes_received.saturating_add(n as u64);
                                }
                                let ft = FileTime::from_unix_time(mtime, 0);
                                let _ = set_file_mtime(&dst, ft);
                                conn.expected_paths.insert(dst);
                                conn.files_received = conn.files_received.saturating_add(1);
                            }
                            // 21 == DeltaStart, 22 == DeltaSample, 23 == DeltaEnd
                            frame::DELTA_START => {
                                // payload: nlen u16 | rel bytes | size u64 | mtime i64 | granule u32 | sample u32
                                if payload.len() < 2 + 8 + 8 + 4 + 4 { anyhow::bail!("bad DELTA_START"); }
                                let nlen = u16::from_le_bytes([payload[0], payload[1]]) as usize;
                                if payload.len() < 2 + nlen + 8 + 8 + 4 + 4 { anyhow::bail!("bad DELTA_START len"); }
                                let rels = std::str::from_utf8(&payload[2..2+nlen]).unwrap_or("");
                                let mut off = 2 + nlen;
                                let fsize = u64::from_le_bytes(payload[off..off+8].try_into()
                                    .context("Invalid size bytes in NEED")?); off+=8;
                                let fmtime = i64::from_le_bytes(payload[off..off+8].try_into()
                                    .context("Invalid mtime bytes in NEED")?); off+=8;
                                let granule = u32::from_le_bytes(payload[off..off+4].try_into()
                                    .context("Invalid granule bytes in NEED")?) as u64; off+=4;
                                let sample = u32::from_le_bytes(payload[off..off+4].try_into()
                                    .context("Invalid sample bytes in NEED")?) as usize;
                                let dst = normalize_under_root(&base, Path::new(rels))?;
                                if let Some(parent) = dst.parent() { std::fs::create_dir_all(parent).ok(); }
                                let mut f = File::options().create(true).read(true).write(true).open(&dst)?;
                                let _ = f.set_len(fsize);
                                drop(f);
                                delta_state = Some(DeltaState{ dst_path: dst, file_size: fsize, mtime: fmtime, granule, sample, need_ranges: Vec::new() });
                            }
                            frame::DELTA_SAMPLE => {
                                // payload: offset u64 | hash_len u16 | hash bytes
                                if payload.len() < 8 + 2 { anyhow::bail!("bad DELTA_SAMPLE"); }
                                let off = u64::from_le_bytes(payload[0..8].try_into()
                                    .context("Invalid offset bytes in DELTA_SAMPLE")?);
                                let hlen = u16::from_le_bytes(payload[8..10].try_into()
                                    .context("Invalid hash length bytes in DELTA_SAMPLE")?) as usize;
                                if payload.len() < 10 + hlen { anyhow::bail!("bad DELTA_SAMPLE hash len"); }
                                let hashc = &payload[10..10+hlen];
                                if let Some(ds) = delta_state.as_mut() {
                                    if let Ok(mut f) = File::open(&ds.dst_path) {
                                        use std::io::{Read, Seek, SeekFrom};
                                        let mut buf = vec![0u8; ds.sample];
                                        let _ = f.seek(SeekFrom::Start(off));
                                        let n = f.read(&mut buf).unwrap_or(0);
                                        let n = std::cmp::min(n, ds.sample);
                                        let h = blake3::hash(&buf[..n]);
                                        if h.as_bytes() != hashc {
                                            let start = (off / ds.granule) * ds.granule;
                                            let end = (start + ds.granule).min(ds.file_size);
                                            ds.need_ranges.push((start, end - start));
                                        }
                                    } else {
                                        ds.need_ranges.clear();
                                        ds.need_ranges.push((0, ds.file_size));
                                    }
                                }
                            }
                            frame::DELTA_END => {
                                // Coalesce and send NeedRanges
                                if let Some(ds) = delta_state.as_mut() {
                                    let mut v = std::mem::take(&mut ds.need_ranges);
                                    v.sort_by_key(|r| r.0);
                                    let mut coalesced: Vec<(u64,u64)> = Vec::new();
                                    for (mut s_off, mut s_len) in v.into_iter() {
                                        if let Some(last) = coalesced.last_mut() {
                                            let last_end = last.0 + last.1;
                                            if s_off <= last_end { let new_end = (s_off + s_len).max(last_end); last.1 = new_end - last.0; continue; }
                                        }
                                        coalesced.push((s_off, s_len));
                                    }
                                    let mut hdr = Vec::with_capacity(4);
                                    hdr.extend_from_slice(&(coalesced.len() as u32).to_le_bytes());
                                    write_frame(&mut stream, frame::NEED_RANGES_START, &hdr).await?;
                                    for (off, len) in &coalesced {
                                        let mut pl = Vec::with_capacity(16);
                                        pl.extend_from_slice(&off.to_le_bytes());
                                        pl.extend_from_slice(&len.to_le_bytes());
                                        write_frame(&mut stream, frame::NEED_RANGE, &pl).await?;
                                    }
                                    write_frame(&mut stream, frame::NEED_RANGES_END, &[]).await?;
                                    ds.need_ranges = coalesced;
                                } else {
                                    write_frame(&mut stream, frame::NEED_RANGES_START, &0u32.to_le_bytes()).await?;
                                    write_frame(&mut stream, frame::NEED_RANGES_END, &[]).await?;
                                }
                            }
                            frame::DELTA_DATA => {
                                // payload: offset u64 | data bytes
                                if payload.len() < 8 { anyhow::bail!("bad DELTA_DATA"); }
                                let off = u64::from_le_bytes(payload[0..8].try_into()
                                    .context("Invalid offset bytes in DELTA_DATA")?);
                                let data = &payload[8..];
                                if let Some(ds) = delta_state.as_ref() {
                                    use std::io::{Seek, SeekFrom, Write};
                                    let mut f = File::options().read(true).write(true).open(&ds.dst_path)?;
                                    let _ = f.seek(SeekFrom::Start(off));
                                    if off.saturating_add(data.len() as u64) > ds.file_size { anyhow::bail!("DELTA_DATA out of range"); }
                                    f.write_all(data)?;
                                    conn.bytes_received = conn.bytes_received.saturating_add(data.len() as u64);
                                }
                            }
                            frame::DELTA_DONE => {
                                if let Some(ds) = delta_state.take() {
                                    let ft = FileTime::from_unix_time(ds.mtime, 0);
                                    let _ = set_file_mtime(&ds.dst_path, ft);
                                    conn.expected_paths.insert(ds.dst_path.clone());
                                    conn.files_received = conn.files_received.saturating_add(1);
                                }
                                write_frame(&mut stream, frame::OK, b"OK").await?;
                            }
                            // 7 == Done
                            frame::DONE => {
                                // Mirror delete if requested
                                if mirror {
                                    let mut all_dirs: Vec<PathBuf> = Vec::new();
                                    #[cfg(windows)]
                                    fn keyify(p: &Path) -> String { p.to_string_lossy().to_ascii_lowercase() }
                                    #[cfg(not(windows))]
                                    fn keyify(p: &Path) -> String { p.to_string_lossy().to_string() }
                                    #[cfg(windows)]
                                    let expected_set: std::collections::HashSet<String> = conn
                                        .expected_paths
                                        .iter()
                                        .map(|p| keyify(p))
                                        .collect();
                                    #[cfg(not(windows))]
                                    let expected_set: std::collections::HashSet<String> = conn
                                        .expected_paths
                                        .iter()
                                        .map(|p| keyify(p))
                                        .collect();
                                    for e in walkdir::WalkDir::new(&base)
                                        .into_iter()
                                        .filter_map(|e| e.ok())
                                    {
                                        let p = e.path().to_path_buf();
                                        if e.file_type().is_dir() {
                                            all_dirs.push(p);
                                            continue;
                                        }
                                        if e.file_type().is_file() || e.file_type().is_symlink() {
                                            if !expected_set.contains(&keyify(&p)) {
                                                let _ = std::fs::remove_file(&p);
                                            }
                                        }
                                    }
                                    all_dirs
                                        .sort_by_key(|p| std::cmp::Reverse(p.components().count()));
                                    for d in all_dirs {
                                        if !expected_set.contains(&keyify(&d)) {
                                            let _ = std::fs::remove_dir(&d);
                                        }
                                    }
                                }
                                write_frame(&mut stream, frame::OK, b"OK").await?;
                                // Log connection counters and elapsed time
                                let elapsed_ms = started.elapsed().as_millis();
                                eprintln!(
                                    "Connection summary: files_sent={}, bytes_sent={}, files_received={}, bytes_received={}, elapsed_ms={}",
                                    conn.files_sent, conn.bytes_sent, conn.files_received, conn.bytes_received, elapsed_ms
                                );
                                break;
                            }
                            // Ignore range frames until implemented
                            frame::NEED_RANGES_START | frame::NEED_RANGE | frame::NEED_RANGES_END => {
                                let _ = payload;
                                // For now, do nothing; future work will implement.
                                continue;
                            }
                            // Remove tree request (for move when src is remote)
                            frame::REMOVE_TREE_REQ => {
                                if payload.len() < 2 { anyhow::bail!("bad REMOVE_TREE_REQ"); }
                                let nlen = u16::from_le_bytes([payload[0], payload[1]]) as usize;
                                if payload.len() < 2 + nlen { anyhow::bail!("bad REMOVE_TREE_REQ len"); }
                                let rel = std::str::from_utf8(&payload[2..2+nlen]).unwrap_or("");
                                let base_path = normalize_under_root(&base, Path::new(rel))?;
                                // Delete files then dirs (deepest-first)
                                let mut err: Option<String> = None;
                                if base_path.exists() {
                                    let mut dirs: Vec<PathBuf> = Vec::new();
                                    for e in walkdir::WalkDir::new(&base_path).into_iter().filter_map(|e| e.ok()) {
                                        let p = e.path().to_path_buf();
                                        if e.file_type().is_dir() { dirs.push(p); continue; }
                                        if e.file_type().is_file() || e.file_type().is_symlink() {
                                            if let Err(e) = std::fs::remove_file(&p) { err = Some(format!("{}", e)); break; }
                                        }
                                    }
                                    dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
                                    if err.is_none() {
                                        for d in dirs { let _ = std::fs::remove_dir(&d); }
                                        let _ = std::fs::remove_dir(&base_path);
                                    }
                                }
                                let mut resp = Vec::with_capacity(1 + 128);
                                if let Some(e) = err { resp.push(1u8); resp.extend_from_slice(e.as_bytes()); } else { resp.push(0u8); }
                                write_frame(&mut stream, frame::REMOVE_TREE_RESP, &resp).await?;
                            }
                            _ => {
                                anyhow::bail!("unexpected frame: {}", t);
                            }
                        }
                    }
                    Ok::<(), anyhow::Error>(())
                }
                .await
                {
                    eprintln!("async connection error: {}", e);
                }
            });
        }
    }
}

#[allow(dead_code)]
pub mod client {
    use super::*;
    use crate::url;
    use anyhow::{Context, Result};
    use robosync::protocol::{MAGIC, VERSION};
    use robosync::protocol::frame;
    use filetime::{set_file_mtime, FileTime};
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;
    use tokio::sync::Mutex;
    use tokio::time::{timeout, Duration};

    // Use centralized constants from protocol module
    use crate::protocol::timeouts::{WRITE_BASE_MS, PER_MB_MS};

    #[inline]
    async fn write_all_timed(stream: &mut TcpStream, buf: &[u8], ms: u64) -> Result<()> {
        match timeout(Duration::from_millis(ms), async { stream.write_all(buf).await }).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(e.into()),
            Err(_) => anyhow::bail!("raw body write timeout ({} ms)", ms),
        }
    }

    pub async fn connect(host: &str, port: u16) -> Result<TcpStream> {
        let addr = format!("{}:{}", host, port);
        let stream = TcpStream::connect(&addr)
            .await
            .with_context(|| format!("connect {}", addr))?;
        let _ = stream.set_nodelay(true);
        Ok(stream)
    }

    struct TarChanWriter {
        tx: tokio::sync::mpsc::Sender<Vec<u8>>,
        buf: Vec<u8>,
        cap: usize,
    }
    impl std::io::Write for TarChanWriter {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            let mut rem = data;
            while !rem.is_empty() {
                let space = self.cap - self.buf.len();
                let take = rem.len().min(space);
                self.buf.extend_from_slice(&rem[..take]);
                rem = &rem[take..];
                if self.buf.len() >= self.cap {
                    let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(self.cap));
                    self.tx
                        .blocking_send(chunk)
                        .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))?;
                }
            }
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            if !self.buf.is_empty() {
                let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(self.cap));
                self.tx
                    .blocking_send(chunk)
                    .map_err(|e| std::io::Error::new(std::io::ErrorKind::BrokenPipe, e))?;
            }
            Ok(())
        }
    }

    pub async fn complete_remote(comp_str: &str) -> Result<()> {
        let remote = if let Some(r) = url::parse_remote_url(&PathBuf::from(comp_str)) {
            r
        } else {
            return Ok(());
        };

        let mut stream = match timeout(
            Duration::from_millis(crate::protocol::timeouts::CONNECT_MS),
            connect(&remote.host, remote.port),
        )
        .await
        {
            Ok(Ok(s)) => s,
            _ => return Ok(()),
        };

        let path_bytes = remote.path.to_string_lossy();
        let mut payload = Vec::with_capacity(2 + path_bytes.len());
        payload.extend_from_slice(&(path_bytes.len() as u16).to_le_bytes());
        payload.extend_from_slice(path_bytes.as_bytes());

        let mut hdr = Vec::with_capacity(4 + 2 + 1 + 4);
        hdr.extend_from_slice(MAGIC);
        hdr.extend_from_slice(&VERSION.to_le_bytes());
        hdr.push(frame::LIST_REQ); // ListReq
        hdr.extend_from_slice(&(payload.len() as u32).to_le_bytes());

        match timeout(Duration::from_millis(crate::protocol::timeouts::CONNECT_MS), async {
            stream.write_all(&hdr).await?;
            stream.write_all(&payload).await
        })
        .await
        {
            Ok(Ok(_)) => {}
            _ => return Ok(()),
        }

        let (typ, resp_payload) = server::read_frame(&mut stream).await?;

        if typ != frame::LIST_RESP {
            // ListResp
            return Ok(());
        }

        let mut off = 0;
        if resp_payload.len() < 4 {
            return Ok(());
        }
        let count = u32::from_le_bytes(resp_payload[off..off + 4].try_into()
            .context("Invalid count bytes in NEED response")?);
        off += 4;

        for _ in 0..count {
            if resp_payload.len() < off + 1 {
                break;
            }
            let kind = resp_payload[off];
            off += 1;
            if resp_payload.len() < off + 2 {
                break;
            }
            let nlen = u16::from_le_bytes(resp_payload[off..off + 2].try_into()
                .context("Invalid name length bytes in NEED response")?) as usize;
            off += 2;
            if resp_payload.len() < off + nlen {
                break;
            }
            let name = std::str::from_utf8(&resp_payload[off..off + nlen]).unwrap_or("");
            off += nlen;

            let mut remote_path_prefix = remote.path.to_string_lossy().to_string();
            if !remote_path_prefix.ends_with('/') {
                remote_path_prefix.push('/');
            }

            let suggestion_path = format!("{}{}", remote_path_prefix, name);
            let suggestion = format!(
                "robosync://{}:{}{}",
                remote.host, remote.port, suggestion_path
            );

            if kind == 1 {
                // Directory
                println!("{}/", suggestion);
            } else {
                // File
                println!("{}", suggestion);
            }
        }

        Ok(())
    }

    pub async fn remove_tree(host: &str, port: u16, path: &std::path::Path) -> Result<()> {
        let mut stream = connect(host, port).await?;
        // START with root "/" and no flags
        let root = "/";
        let mut payload = Vec::with_capacity(2 + root.len() + 1);
        payload.extend_from_slice(&(root.len() as u16).to_le_bytes());
        payload.extend_from_slice(root.as_bytes());
        payload.push(0);
        server::write_frame(&mut stream, frame::START, &payload).await?;
        let (typ, _resp) = server::read_frame(&mut stream).await?;
        if typ != frame::OK { anyhow::bail!("daemon error starting remove"); }

        // Send RemoveTreeReq
        let rel = path.to_string_lossy();
        let mut pl = Vec::with_capacity(2 + rel.len());
        pl.extend_from_slice(&(rel.len() as u16).to_le_bytes());
        pl.extend_from_slice(rel.as_bytes());
        server::write_frame(&mut stream, frame::REMOVE_TREE_REQ, &pl).await?;
        let (t, resp) = server::read_frame(&mut stream).await?;
        if t != frame::REMOVE_TREE_RESP { anyhow::bail!("bad response to remove"); }
        if resp.is_empty() || resp[0] != 0 {
            anyhow::bail!("remove failed: {}", String::from_utf8_lossy(&resp[1..]));
        }
        Ok(())
    }

    pub async fn push(
        host: &str,
        port: u16,
        dest: &Path,
        src_root: &Path,
        args: &crate::Args,
    ) -> Result<()> {
        let mut stream = connect(host, port).await?;

        // START payload: dest_len u16 | dest_bytes | flags u8
        let dest_s = dest.to_string_lossy();
        let mut payload = Vec::with_capacity(2 + dest_s.len() + 1);
        payload.extend_from_slice(&(dest_s.len() as u16).to_le_bytes());
        payload.extend_from_slice(dest_s.as_bytes());
        let mut flags: u8 = if args.mirror || args.delete {
            0b0000_0001
        } else {
            0
        };
        if args.empty_dirs {
            flags |= 0b0000_0100;
        }
        if args.ludicrous_speed {
            flags |= 0b0000_1000;
        }
        payload.push(flags);

        server::write_frame(&mut stream, frame::START, &payload).await?;
        let (typ, resp) = server::read_frame(&mut stream).await?;
        if typ != frame::OK {
            // OK
            anyhow::bail!("daemon error: {}", String::from_utf8_lossy(&resp));
        }

        // Send manifest by walking with symlink awareness
        use walkdir::WalkDir;
        server::write_frame(&mut stream, frame::MANIFEST_START, &[]).await?; // ManifestStart
        use std::time::UNIX_EPOCH;
        for ent in WalkDir::new(src_root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = ent.path();
            let rel = path.strip_prefix(src_root).unwrap_or(path);
            let rels = rel.to_string_lossy();
            if rels.is_empty() {
                continue;
            }
            let ft = ent.file_type();
            if ft.is_dir() {
                let mut pl = Vec::with_capacity(1 + 2 + rels.len());
                pl.push(2u8);
                pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                pl.extend_from_slice(rels.as_bytes());
                server::write_frame(&mut stream, frame::MANIFEST_ENTRY, &pl).await?;
                continue;
            }
            if ft.is_symlink() {
                if let Ok(target) = std::fs::read_link(path) {
                    let t = target.to_string_lossy();
                    let mut pl = Vec::with_capacity(1 + 2 + rels.len() + 2 + t.len());
                    pl.push(1u8);
                    pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                    pl.extend_from_slice(rels.as_bytes());
                    pl.extend_from_slice(&(t.len() as u16).to_le_bytes());
                    pl.extend_from_slice(t.as_bytes());
                    server::write_frame(&mut stream, frame::MANIFEST_ENTRY, &pl).await?;
                }
                continue;
            }
            if ft.is_file() {
                if let Ok(md) = std::fs::metadata(path) {
                    let size = md.len();
                    let mtime = md
                        .modified()
                        .ok()
                        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    let mut pl = Vec::with_capacity(1 + 2 + rels.len() + 8 + 8);
                    pl.push(0u8);
                    pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                    pl.extend_from_slice(rels.as_bytes());
                    pl.extend_from_slice(&size.to_le_bytes());
                    pl.extend_from_slice(&mtime.to_le_bytes());
                    server::write_frame(&mut stream, frame::MANIFEST_ENTRY, &pl).await?;
                }
            }
        }
        server::write_frame(&mut stream, frame::MANIFEST_END, &[]).await?; // ManifestEnd

        // Read need list
        let (tneed, plneed) = server::read_frame(&mut stream).await?;
        if tneed != frame::NEED_LIST {
            // NeedList
            anyhow::bail!("server did not reply with NeedList");
        }

        let mut needed = std::collections::HashSet::new();
        let mut off = 0usize;
        if plneed.len() >= 4 {
            let count = u32::from_le_bytes(plneed[off..off + 4].try_into()
                .context("Invalid count bytes in NEED response")?) as usize;
            // Sanity check: limit to 1 million entries to prevent DoS
            const MAX_NEED_ENTRIES: usize = 1_000_000;
            if count > MAX_NEED_ENTRIES {
                anyhow::bail!("NEED_LIST count exceeds maximum allowed ({}): {}", MAX_NEED_ENTRIES, count);
            }
            off += 4;
            for _ in 0..count {
                if off + 2 > plneed.len() {
                    break;
                }
                let nlen = u16::from_le_bytes(plneed[off..off + 2].try_into()
                    .context("Invalid name length bytes in NEED response")?) as usize;
                off += 2;
                if off + nlen > plneed.len() {
                    break;
                }
                let s = std::str::from_utf8(&plneed[off..off + nlen])
                    .unwrap_or("")
                    .to_string();
                off += nlen;
                needed.insert(s);
            }
        }

        // Build file list from filesystem and filter by needed
        let filter = crate::fs_enum::FileFilter {
            exclude_files: args.exclude_files.clone(),
            exclude_dirs: args.exclude_dirs.clone(),
            min_size: None,
            max_size: None,
            include_empty_dirs: true,
        };
        let all_files = crate::fs_enum::enumerate_directory_filtered(src_root, &filter)?;
        let files_needed: Vec<_> = all_files
            .into_iter()
            .filter(|fe| {
                let rel = fe.path.strip_prefix(src_root).unwrap_or(&fe.path);
                needed.contains(&rel.to_string_lossy().to_string())
            })
            .collect();

        let (small_files, large_files): (Vec<_>, Vec<_>) =
            files_needed.into_iter().partition(|e| e.size < 1_000_000);

        if !small_files.is_empty() {
            server::write_frame(&mut stream, frame::TAR_START, &[]).await?; // TarStart
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
            let tar_task_src_root = src_root.to_path_buf();
            let tar_task = tokio::task::spawn_blocking(move || -> Result<()> {
                let mut w = crate::net_async::client::TarChanWriter {
                    tx,
                    buf: Vec::with_capacity(1024 * 1024),
                    cap: 1024 * 1024,
                };
                {
                    let mut builder = tar::Builder::new(&mut w);
                    for fe in small_files {
                        let rel = fe.path.strip_prefix(&tar_task_src_root).unwrap_or(&fe.path);
                        builder.append_path_with_name(&fe.path, rel)?;
                    }
                    builder.finish()?;
                }
                let _ = std::io::Write::flush(&mut w);
                Ok(())
            });

            while let Some(chunk) = rx.recv().await {
                server::write_frame(&mut stream, frame::TAR_DATA, &chunk).await?; // TarData
            }

            tar_task.await??;
            server::write_frame(&mut stream, frame::TAR_END, &[]).await?; // TarEnd
            let (t_ok, _) = server::read_frame(&mut stream).await?;
            if t_ok != frame::OK {
                anyhow::bail!("server TAR error");
            }
        }

        // Auto-tune workers/chunk if user hasn't overridden and based on simple heuristics
        let overridden_workers = std::env::args().any(|a| a == "--net-workers" || a.starts_with("--net-workers="));
        let overridden_chunk = std::env::args().any(|a| a == "--net-chunk-mb" || a.starts_with("--net-chunk-mb="));
        let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let mut eff_workers = args.net_workers;
        let mut eff_chunk_mb = args.net_chunk_mb;
        if !overridden_workers {
            let large_count = large_files.len().max(1);
            // Heuristic: scale workers with CPUs and available large files (bounded)
            eff_workers = std::cmp::min(32, std::cmp::max(2, std::cmp::min(large_count, std::cmp::max(4, cpus / 2))));
        }
        if !overridden_chunk {
            eff_chunk_mb = if args.ludicrous_speed { 8 } else { 4 };
        }

        let work = Arc::new(Mutex::new(large_files));
        let mut handles = vec![];
        let worker_count = eff_workers.max(1).min(32);
        let chunk_bytes: usize = (eff_chunk_mb.max(1).min(32)) * 1024 * 1024;
        for _ in 0..worker_count {
            let work_clone = Arc::clone(&work);
            let host = host.to_string();
            let port = port;
            let dest = dest.to_path_buf();
            let src_root = src_root.to_path_buf();
            let chunk_bytes = chunk_bytes;

            let handle = tokio::spawn(async move {
                let mut s = connect(&host, port).await?;
                // Start worker connection
                let dest_s = dest.to_string_lossy();
                let mut pl = Vec::with_capacity(2 + dest_s.len() + 1);
                pl.extend_from_slice(&(dest_s.len() as u16).to_le_bytes());
                pl.extend_from_slice(dest_s.as_bytes());
                pl.push(0); // Flags (inherit speed profile server-side)
                server::write_frame(&mut s, frame::START, &pl).await?;
                let (typ, resp) = server::read_frame(&mut s).await?;
                if typ != frame::OK {
                    anyhow::bail!("worker daemon error: {}", String::from_utf8_lossy(&resp));
                }

                loop {
                    let job = {
                        let mut q = work_clone.lock().await;
                        q.pop()
                    };
                    if let Some(fe) = job {
                        let rel = fe.path.strip_prefix(&src_root).unwrap_or(&fe.path);
                        let rels = rel.to_string_lossy();
                        let md = std::fs::metadata(&fe.path)?;
                        let mtime = md
                            .modified()?
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64;
                        // Try delta for large files (>= 100MB); fallback to raw if ranges are empty
                        let mut used_delta = false;
                        if fe.size >= 104_857_600 {
                            let granule: u32 = 8 * 1024 * 1024;
                            let sample: u32 = 64 * 1024;
                            let mut pl0 = Vec::with_capacity(2 + rels.len() + 8 + 8 + 4 + 4);
                            pl0.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                            pl0.extend_from_slice(rels.as_bytes());
                            pl0.extend_from_slice(&fe.size.to_le_bytes());
                            pl0.extend_from_slice(&mtime.to_le_bytes());
                            pl0.extend_from_slice(&granule.to_le_bytes());
                            pl0.extend_from_slice(&sample.to_le_bytes());
                            server::write_frame(&mut s, frame::DELTA_START, &pl0).await?;
                            // send samples for each granule: start, mid, end
                            let granule64 = granule as u64;
                            let mut bf = std::fs::File::open(&fe.path)?;
                            let mut buf0 = vec![0u8; sample as usize];
                            let mut off0: u64 = 0;
                            use std::io::{Read as _, Seek, SeekFrom};
                            while off0 < fe.size {
                                bf.seek(SeekFrom::Start(off0))?;
                                let n0 = bf.read(&mut buf0)?;
                                let h0 = blake3::hash(&buf0[..n0]);
                                let mut pls = Vec::with_capacity(8 + 2 + h0.as_bytes().len());
                                pls.extend_from_slice(&off0.to_le_bytes());
                                pls.extend_from_slice(&(h0.as_bytes().len() as u16).to_le_bytes());
                                pls.extend_from_slice(h0.as_bytes());
                                server::write_frame(&mut s, frame::DELTA_SAMPLE, &pls).await?;
                                let mid = off0.saturating_add((granule64 / 2).min(fe.size - off0));
                                bf.seek(SeekFrom::Start(mid))?;
                                let n1 = bf.read(&mut buf0)?;
                                let h1 = blake3::hash(&buf0[..n1]);
                                let mut pls1 = Vec::with_capacity(8 + 2 + h1.as_bytes().len());
                                pls1.extend_from_slice(&mid.to_le_bytes());
                                pls1.extend_from_slice(&(h1.as_bytes().len() as u16).to_le_bytes());
                                pls1.extend_from_slice(h1.as_bytes());
                                server::write_frame(&mut s, frame::DELTA_SAMPLE, &pls1).await?;
                                let end_off = if fe.size > off0 { (off0 + granule64).min(fe.size).saturating_sub(sample as u64) } else { off0 };
                                bf.seek(SeekFrom::Start(end_off))?;
                                let n2 = bf.read(&mut buf0)?;
                                let h2 = blake3::hash(&buf0[..n2]);
                                let mut pls2 = Vec::with_capacity(8 + 2 + h2.as_bytes().len());
                                pls2.extend_from_slice(&end_off.to_le_bytes());
                                pls2.extend_from_slice(&(h2.as_bytes().len() as u16).to_le_bytes());
                                pls2.extend_from_slice(h2.as_bytes());
                                server::write_frame(&mut s, frame::DELTA_SAMPLE, &pls2).await?;
                                off0 = off0.saturating_add(granule64);
                            }
                            server::write_frame(&mut s, frame::DELTA_END, &[]).await?;
                            // read need ranges
                            let (t_need, pl_need) = server::read_frame(&mut s).await?;
                            let mut need_ranges: Vec<(u64, u64)> = Vec::new();
                            if t_need == frame::NEED_RANGES_START {
                                if pl_need.len() >= 4 {
                                    let _cnt = u32::from_le_bytes(pl_need[0..4].try_into()
                                        .context("Invalid count bytes in NEEDRANGES")?) as usize;
                                    loop {
                                        let (ti, pli) = server::read_frame(&mut s).await?;
                                        if ti == frame::NEED_RANGE {
                                            if pli.len() >= 16 {
                                                let off = u64::from_le_bytes(pli[0..8].try_into()
                                                    .context("Invalid offset bytes in NEEDRANGES")?);
                                                let len = u64::from_le_bytes(pli[8..16].try_into()
                                                    .context("Invalid length bytes in NEEDRANGES")?);
                                                need_ranges.push((off, len));
                                            }
                                        } else if ti == frame::NEED_RANGES_END { break; } else { anyhow::bail!("unexpected frame in need list"); }
                                    }
                                }
                            }
                            if !need_ranges.is_empty() && (need_ranges.len() as u64) * granule64 < fe.size {
                                let mut f2 = tokio::fs::File::open(&fe.path).await?;
                                use tokio::io::{AsyncReadExt, AsyncSeekExt};
                                for (mut off, mut left) in need_ranges.into_iter() {
                                    let mut b = vec![0u8; 4 * 1024 * 1024];
                                    while left > 0 {
                                        f2.seek(std::io::SeekFrom::Start(off)).await?;
                                        let want = (left as usize).min(b.len());
                                        let n = f2.read(&mut b[..want]).await?;
                                        if n == 0 { break; }
                                        let mut p = Vec::with_capacity(8 + n);
                                        p.extend_from_slice(&off.to_le_bytes());
                                        p.extend_from_slice(&b[..n]);
                                        server::write_frame(&mut s, frame::DELTA_DATA, &p).await?;
                                        off += n as u64;
                                        left -= n as u64;
                                    }
                                }
                                server::write_frame(&mut s, frame::DELTA_DONE, &[]).await?;
                                let (_tok, _plk) = server::read_frame(&mut s).await?;
                                used_delta = true;
                            }
                        }
                        if !used_delta {
                            // Fallback: raw full file
                            let mut pl_raw = Vec::with_capacity(2 + rels.len() + 8 + 8);
                            pl_raw.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                            pl_raw.extend_from_slice(rels.as_bytes());
                            pl_raw.extend_from_slice(&fe.size.to_le_bytes());
                            pl_raw.extend_from_slice(&mtime.to_le_bytes());
                            server::write_frame(&mut s, frame::FILE_RAW_START, &pl_raw).await?;
                            let mut f = tokio::fs::File::open(&fe.path).await?;
                            use tokio::io::AsyncReadExt;
                            let mut buf = vec![0u8; chunk_bytes];
                            let mut remaining = fe.size;
                            while remaining > 0 {
                                let to_read = (remaining as usize).min(buf.len());
                                let n = f.read(&mut buf[..to_read]).await?;
                                if n == 0 { break; }
                                let ms = crate::protocol::timeouts::WRITE_BASE_MS + ((n as u64 + 1_048_575)/1_048_576) * crate::protocol::timeouts::PER_MB_MS;
                                write_all_timed(&mut s, &buf[..n], ms).await?;
                                remaining -= n as u64;
                            }
                        }
                    } else {
                        break;
                    }
                }
                server::write_frame(&mut s, frame::DONE, &[]).await?; // Done
                let (t_ok, _) = server::read_frame(&mut s).await?;
                if t_ok != frame::OK {
                    anyhow::bail!("worker DONE error");
                }
                Ok::<(), anyhow::Error>(())
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.await??;
        }

        server::write_frame(&mut stream, frame::DONE, &[]).await?; // Final Done
        let (t_ok, _) = server::read_frame(&mut stream).await?;
        if t_ok != frame::OK {
            anyhow::bail!("server did not ack final DONE");
        }

        Ok(())
    }

    // (TarChanWriter defined above)

    pub async fn pull(
        host: &str,
        port: u16,
        src: &Path,
        dest_root: &Path,
        args: &crate::Args,
    ) -> Result<()> {
        let mut stream = connect(host, port).await?;

        // START payload: path on server (src) + flags (mirror + pull + include_empty_dirs)
        let src_s = src.to_string_lossy();
        let mut payload = Vec::with_capacity(2 + src_s.len() + 1);
        payload.extend_from_slice(&(src_s.len() as u16).to_le_bytes());
        payload.extend_from_slice(src_s.as_bytes());
        let mut flags: u8 = 0b0000_0010; // pull
        if args.mirror || args.delete {
            flags |= 0b0000_0001;
        }
        if args.empty_dirs {
            flags |= 0b0000_0100;
        }
        payload.push(flags);

        server::write_frame(&mut stream, 1, &payload).await?;
        let (typ, resp) = server::read_frame(&mut stream).await?;
        if typ != 2u8 {
            anyhow::bail!("daemon error: {}", String::from_utf8_lossy(&resp));
        }

        // Send manifest of local destination to allow delta
        server::write_frame(&mut stream, frame::MANIFEST_START, &[]).await?; // ManifestStart
        let filter = crate::fs_enum::FileFilter {
            exclude_files: args.exclude_files.clone(),
            exclude_dirs: args.exclude_dirs.clone(),
            min_size: None,
            max_size: None,
            include_empty_dirs: true,
        };
        let entries = crate::fs_enum::enumerate_directory_filtered(dest_root, &filter)?;
        use std::time::UNIX_EPOCH;
        for fe in entries.iter().filter(|e| !e.is_directory) {
            let rel = fe.path.strip_prefix(dest_root).unwrap_or(&fe.path);
            let rels = rel.to_string_lossy();
            let md = std::fs::metadata(&fe.path)?;
            let mtime = md
                .modified()?
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as i64;
            let mut pl = Vec::with_capacity(1 + 2 + rels.len() + 8 + 8);
            pl.push(0u8);
            pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
            pl.extend_from_slice(rels.as_bytes());
            pl.extend_from_slice(&fe.size.to_le_bytes());
            pl.extend_from_slice(&mtime.to_le_bytes());
                    server::write_frame(&mut stream, frame::MANIFEST_ENTRY, &pl).await?; // ManifestEntry
        }
        server::write_frame(&mut stream, frame::MANIFEST_END, &[]).await?; // ManifestEnd

        let (_tneed, _plneed) = server::read_frame(&mut stream).await?;

        let mut expected_paths = HashSet::new();
        let mut current_file: Option<(tokio::fs::File, std::path::PathBuf, u64, i64)> = None;

        loop {
            let (t, pl) = server::read_frame(&mut stream).await?;
            match t {
                8u8 => {
                    // TarStart
                    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
                    let unpack_dest = dest_root.to_path_buf();
                    let unpacker = tokio::task::spawn_blocking(move || -> Result<()> {
                        let mut reader = ChanReader {
                            rx,
                            buf: Vec::new(),
                            pos: 0,
                            done: false,
                        };
                        let mut ar = tar::Archive::new(reader);
                        ar.set_overwrite(true);
                        ar.unpack(&unpack_dest)?;
                        Ok(())
                    });

                    loop {
                        let (ti, pli) = server::read_frame(&mut stream).await?;
                        if ti == 9u8 {
                            // TarData
                            if tx.send(pli).await.is_err() {
                                anyhow::bail!("tar unpacker closed");
                            }
                        } else if ti == 10u8 {
                            // TarEnd
                            break;
                        } else {
                            anyhow::bail!("unexpected frame during tar: {}", ti);
                        }
                    }
                    drop(tx);
                    unpacker.await??;
                    server::write_frame(&mut stream, 2, b"OK").await?;
                }
                4u8 => {
                    // FileStart
                    if pl.len() < 2 + 8 + 8 {
                        anyhow::bail!("bad FILE_START");
                    }
                    let nlen = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                    if pl.len() < 2 + nlen + 8 + 8 {
                        anyhow::bail!("bad FILE_START len");
                    }
                    let rel = std::str::from_utf8(&pl[2..2 + nlen])?;
                    let mut off = 2 + nlen;
                    let size = u64::from_le_bytes(pl[off..off + 8].try_into()
                        .context("Invalid size bytes in FILE_START")?);
                    off += 8;
                    let mtime = i64::from_le_bytes(pl[off..off + 8].try_into()
                        .context("Invalid mtime bytes in FILE_START")?);
                    let dst_path = dest_root.join(rel);
                    if let Some(parent) = dst_path.parent() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                    let f = tokio::fs::File::create(&dst_path).await?;
                    f.set_len(size).await?;
                    expected_paths.insert(dst_path.clone());
                    current_file = Some((f, dst_path, size, mtime));
                }
                5u8 => {
                    // FileData
                    if let Some((f, _, _, _)) = &mut current_file {
                        f.write_all(&pl).await?;
                    }
                }
                6u8 => {
                    // FileEnd
                    if let Some((_, path, _, mtime)) = current_file.take() {
                        let ft = FileTime::from_unix_time(mtime, 0);
                        set_file_mtime(&path, ft)?;
                    }
                }
                frame::MKDIR => {
                    // MkDir
                    if pl.len() < 2 {
                        anyhow::bail!("bad MKDIR");
                    }
                    let nlen = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                    if pl.len() < 2 + nlen {
                        anyhow::bail!("bad MKDIR payload");
                    }
                    let rel = std::str::from_utf8(&pl[2..2 + nlen])?;
                    let dir_path = dest_root.join(rel);
                    tokio::fs::create_dir_all(&dir_path).await?;
                    expected_paths.insert(dir_path);
                }
                frame::SYMLINK => {
                    // Symlink
                    if pl.len() < 4 {
                        anyhow::bail!("bad SYMLINK");
                    }
                    let nlen = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                    let tlen = u16::from_le_bytes([pl[2], pl[3]]) as usize;
                    if pl.len() < 4 + nlen + tlen {
                        anyhow::bail!("bad SYMLINK len");
                    }
                    let rel = std::str::from_utf8(&pl[4..4 + nlen])?;
                    let target = std::str::from_utf8(&pl[4 + nlen..])?;
                    let dst_path = dest_root.join(rel);
                    if let Some(parent) = dst_path.parent() {
                        tokio::fs::create_dir_all(parent).await?;
                    }
                    #[cfg(unix)]
                    tokio::fs::symlink(target, &dst_path).await?;
                    expected_paths.insert(dst_path);
                }
                frame::DONE => {
                    // Done
                    server::write_frame(&mut stream, frame::OK, b"OK").await?;
                    break;
                }
                _ => {}
            }
        }

        if args.mirror {
            let mut all_dirs: Vec<PathBuf> = Vec::new();
            for entry in walkdir::WalkDir::new(dest_root)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let p = entry.path().to_path_buf();
                if entry.file_type().is_dir() {
                    all_dirs.push(p);
                    continue;
                }
                if entry.file_type().is_file() || entry.file_type().is_symlink() {
                    if !expected_paths.contains(&p) {
                        tokio::fs::remove_file(&p).await.ok();
                    }
                }
            }
            all_dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
            for d in all_dirs {
                if d != dest_root && !expected_paths.contains(&d) {
                    tokio::fs::remove_dir(&d).await.ok();
                }
            }
        }

        Ok(())
    }

    struct ChanReader {
        rx: tokio::sync::mpsc::Receiver<Vec<u8>>,
        buf: Vec<u8>,
        pos: usize,
        done: bool,
    }

    impl std::io::Read for ChanReader {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.done {
                return Ok(0);
            }
            if self.pos >= self.buf.len() {
                match self.rx.blocking_recv() {
                    Some(chunk) => {
                        self.buf = chunk;
                        self.pos = 0;
                    }
                    None => {
                        self.done = true;
                        return Ok(0);
                    }
                }
            }
            let n = out.len().min(self.buf.len() - self.pos);
            if n > 0 {
                out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
                self.pos += n;
            }
            Ok(n)
        }
    }
}
