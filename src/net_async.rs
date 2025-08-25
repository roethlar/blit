//! Experimental async (Tokio) transport scaffolding for RoboSync daemon/client.
//!
//! This module is not yet wired into the CLI. It provides minimal, compiling
//! stubs and a basic async server accept loop to start iterating toward the
//! TODO.md P0 goal of refactoring network I/O to Tokio.

use anyhow::{Context, Result};
use std::path::Path;

#[allow(dead_code)]
pub mod server {
    use super::*;
    use filetime::{set_file_mtime, FileTime};
    use std::collections::{HashMap, HashSet};
    use std::fs::File;
    use std::io::Write as _;
    use std::path::{Path, PathBuf};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    const MAGIC: &[u8; 4] = b"RSNC";
    const VERSION: u16 = 1;

    async fn write_frame(stream: &mut TcpStream, t: u8, payload: &[u8]) -> Result<()> {
        let mut hdr = Vec::with_capacity(4 + 2 + 1 + 4);
        hdr.extend_from_slice(MAGIC);
        hdr.extend_from_slice(&VERSION.to_le_bytes());
        hdr.push(t);
        hdr.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        stream.write_all(&hdr).await?;
        stream.write_all(payload).await?;
        Ok(())
    }

    async fn read_frame(stream: &mut TcpStream) -> Result<(u8, Vec<u8>)> {
        let mut hdr = [0u8; 11];
        stream.read_exact(&mut hdr).await?;
        if &hdr[0..4] != MAGIC {
            anyhow::bail!("bad magic");
        }
        let _ver = u16::from_le_bytes([hdr[4], hdr[5]]);
        let typ = hdr[6];
        let len = u32::from_le_bytes([hdr[7], hdr[8], hdr[9], hdr[10]]) as usize;
        let mut payload = vec![0u8; len];
        if len > 0 {
            stream.read_exact(&mut payload).await?;
        }
        Ok((typ, payload))
    }

    fn normalize_under_root(root: &Path, p: &Path) -> PathBuf {
        use std::path::Component::{CurDir, Normal, ParentDir, Prefix, RootDir};
        let mut joined = root.to_path_buf();
        for comp in p.components() {
            match comp {
                ParentDir => {}
                Normal(s) => joined.push(s),
                CurDir => {}
                Prefix(_) | RootDir => {}
            }
        }
        joined
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
            eprintln!("async conn from {}", peer);
            // Spawn per-connection task.
            let root = root.to_path_buf();
            tokio::spawn(async move {
                if let Err(e) = async move {
                    // Expect START, reply OK
                    let (typ, pl) = read_frame(&mut stream).await?;
                    // 1 == FrameType::Start in sync path; avoid dependency to keep loose coupling
                    if typ != 1u8 {
                        anyhow::bail!("expected START frame");
                    }
                    // Parse destination and flags: dest_len u16 | dest_bytes | flags u8
                    let (base, flags) = if pl.len() >= 3 {
                        let n = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                        if pl.len() >= 3 + n {
                            let d = std::str::from_utf8(&pl[2..2 + n]).unwrap_or("");
                            let p = normalize_under_root(&root, Path::new(d));
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

                    // 2 == FrameType::Ok
                    write_frame(&mut stream, 2u8, b"OK").await?;

                    // Connection-scoped state
                    let mut expected_paths: HashSet<PathBuf> = HashSet::new();
                    let mut needed: HashSet<String> = HashSet::new();
                    let mut client_present: HashSet<String> = HashSet::new();
                    // stream_id -> (path, file, size, written, mtime)
                    let mut p_files: HashMap<u8, (PathBuf, File, u64, u64, i64)> = HashMap::new();

                    // Handle frames until Done
                    loop {
                        let (t, payload) = read_frame(&mut stream).await?;
                        match t {
                            // 14 == ManifestStart, 15 == ManifestEntry, 16 == ManifestEnd
                            14u8 => {
                                // Compute need list from manifest
                                needed.clear();
                                loop {
                                    let (ti, pli) = read_frame(&mut stream).await?;
                                    if ti == 15u8 {
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
                                        let dst = normalize_under_root(&base, relp);
                                        // Track what client reports as present
                                        client_present.insert(rels.to_string());
                                        match kind {
                                            0 => {
                                                if pli.len() < 3 + nlen + 8 + 8 {
                                                    anyhow::bail!("bad file entry");
                                                }
                                                let off = 3 + nlen;
                                                let size = u64::from_le_bytes(
                                                    pli[off..off + 8].try_into().unwrap(),
                                                );
                                                let mtime = i64::from_le_bytes(
                                                    pli[off + 8..off + 16].try_into().unwrap(),
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
                                                    needed.insert(rels.to_string());
                                                }
                                                expected_paths.insert(dst);
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
                                                    needed.insert(rels.to_string());
                                                }
                                                expected_paths.insert(dst);
                                            }
                                            2 => {
                                                std::fs::create_dir_all(&dst).ok();
                                                expected_paths.insert(dst);
                                            }
                                            _ => {}
                                        }
                                        continue;
                                    } else if ti == 16u8 {
                                        // Send NeedList with computed entries
                                        let mut resp = Vec::with_capacity(4 + needed.len() * 8);
                                        resp.extend_from_slice(
                                            &(needed.len() as u32).to_le_bytes(),
                                        );
                                        for p in &needed {
                                            let b = p.as_bytes();
                                            resp.extend_from_slice(&(b.len() as u16).to_le_bytes());
                                            resp.extend_from_slice(b);
                                        }
                                        write_frame(&mut stream, 17u8, &resp).await?; // 17 == NeedList
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
                                                        write_frame(&mut stream, 19u8, &plm)
                                                            .await?; // MkDir
                                                    }
                                                }
                                            }
                                            // Send needed or missing entries
                                            // 1) Partition into small-file TAR bundle and per-file sends
                                            let small_threshold: u64 = 1_000_000; // ~1MB
                                            let mut small_files: Vec<(std::path::PathBuf, String, u32)> = Vec::new();
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
                                                let send_this = needed.contains(&rels)
                                                    || !client_present.contains(&rels);
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
                                                        write_frame(&mut stream, 18u8, &pls)
                                                            .await?; // Symlink
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
                                                                small_files.push((ent.path().to_path_buf(), rels, mode));
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
                                                write_frame(&mut stream, 8u8, &[]).await?; // TarStart
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
                                                let chunk_size = 1024 * 1024; // 1MB
                                                let tar_task = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                                                    let mut w = TarChanWriter { tx, buf: Vec::with_capacity(chunk_size), cap: chunk_size };
                                                    {
                                                        let mut builder = tar::Builder::new(&mut w);
                                                        builder.mode(tar::HeaderMode::Deterministic);
                                                        for (src, rels, _mode) in files_for_tar.into_iter() {
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
                                                    write_frame(&mut stream, 9u8, &frame).await?;
                                                }
                                                // Await tar task
                                                match tar_task.await {
                                                    Ok(Ok(())) => {}
                                                    Ok(Err(e)) => anyhow::bail!("tar pack error: {}", e),
                                                    Err(e) => anyhow::bail!("tar task join error: {}", e),
                                                }
                                                // End of TAR
                                                write_frame(&mut stream, 10u8, &[]).await?; // TarEnd
                                                // Optionally send POSIX modes explicitly for parity
                                                #[cfg(unix)]
                                                {
                                                    for (_src, rels, mode) in small_files.iter() {
                                                        let mut pla = Vec::with_capacity(2 + rels.len() + 1 + 4);
                                                        pla.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                                                        pla.extend_from_slice(rels.as_bytes());
                                                        pla.push(0u8);
                                                        pla.extend_from_slice(&mode.to_le_bytes());
                                                        write_frame(&mut stream, 30u8, &pla).await?;
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
                                                write_frame(&mut stream, 4u8, &plf).await?; // FileStart
                                                // Stream data
                                                let mut f = match tokio::fs::File::open(&src).await {
                                                    Ok(f) => f,
                                                    Err(e) => { eprintln!("async pull: open failed {}: {}", src.display(), e); continue; }
                                                };
                                                let mut buf = vec![0u8; 1024 * 1024];
                                                loop {
                                                    use tokio::io::AsyncReadExt;
                                                    let n = match f.read(&mut buf).await {
                                                        Ok(n) => n,
                                                        Err(e) => { eprintln!("async pull: read failed {}: {}", src.display(), e); break; }
                                                    };
                                                    if n == 0 { break; }
                                                    if let Err(e) = write_frame(&mut stream, 5u8, &buf[..n]).await {
                                                        eprintln!("async pull: send chunk failed {}: {}", src.display(), e);
                                                        return Err(e);
                                                    }
                                                }
                                                write_frame(&mut stream, 6u8, &[]).await?; // FileEnd
                                                // POSIX mode via SetAttr
                                                #[cfg(unix)]
                                                {
                                                    let mut pla = Vec::with_capacity(2 + rels.len() + 1 + 4);
                                                    pla.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                                                    pla.extend_from_slice(rels.as_bytes());
                                                    pla.push(0u8);
                                                    pla.extend_from_slice(&mode.to_le_bytes());
                                                    write_frame(&mut stream, 30u8, &pla).await?;
                                                }
                                            }
                                            // Done and wait for client OK
                                            write_frame(&mut stream, 7u8, &[]).await?;
                                            let (tt, _plok) = read_frame(&mut stream).await?;
                                            if tt != 2u8 {
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
                            8u8 => {
                                // Streaming tar unpack without temp files.
                                // Use a bounded channel to feed a blocking unpacker.
                                let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
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
                                    ar.unpack(&base_dir).context("unpack tar")?;
                                    Ok(())
                                });
                                // Feed frames to unpacker
                                loop {
                                    let (ti, pli) = read_frame(&mut stream).await?;
                                    if ti == 9u8 {
                                        if tx.send(pli).await.is_err() { anyhow::bail!("tar unpacker closed"); }
                                        continue;
                                    }
                                    if ti == 10u8 { break; }
                                    anyhow::bail!("unexpected frame during tar: {}", ti);
                                }
                                drop(tx);
                                // Await unpack completion
                                match unpacker.await {
                                    Ok(Ok(())) => {}
                                    Ok(Err(e)) => { anyhow::bail!("tar unpack error: {}", e); }
                                    Err(join_err) => { anyhow::bail!("tar unpack task error: {}", join_err); }
                                }
                                write_frame(&mut stream, 2u8, b"TAR_OK").await?;
                            }
                            // 11 == PFileStart, 12 == PFileData, 13 == PFileEnd
                            11u8 => {
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
                                    u64::from_le_bytes(payload[off..off + 8].try_into().unwrap());
                                off += 8;
                                let mtime =
                                    i64::from_le_bytes(payload[off..off + 8].try_into().unwrap());
                                let dst = normalize_under_root(&base, Path::new(rel));
                                if let Some(parent) = dst.parent() {
                                    std::fs::create_dir_all(parent).ok();
                                }
                                let f = File::create(&dst)
                                    .with_context(|| format!("create {}", dst.display()))?;
                                let _ = f.set_len(size);
                                expected_paths.insert(dst.clone());
                                p_files.insert(sid, (dst, f, size, 0, mtime));
                            }
                            12u8 => {
                                if payload.len() < 1 {
                                    anyhow::bail!("bad PFILE_DATA");
                                }
                                let sid = payload[0];
                                if let Some((_p, f, _sz, ref mut written, _mt)) =
                                    p_files.get_mut(&sid)
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
                                } else {
                                    anyhow::bail!("PFILE_DATA unknown stream");
                                }
                            }
                            13u8 => {
                                if payload.len() < 1 {
                                    anyhow::bail!("bad PFILE_END");
                                }
                                let sid = payload[0];
                                if let Some((p, _f, sz, written, mt)) = p_files.remove(&sid) {
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
                                let dir_path = normalize_under_root(&base, Path::new(rel));
                                let _ = std::fs::create_dir_all(&dir_path);
                                expected_paths.insert(dir_path);
                            }
                            // 18 == Symlink
                            18u8 => {
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
                                let dst_path = normalize_under_root(&base, Path::new(rel));
                                if let Some(parent) = dst_path.parent() {
                                    std::fs::create_dir_all(parent).ok();
                                }
                                #[cfg(unix)]
                                {
                                    let _ = std::fs::remove_file(&dst_path);
                                    std::os::unix::fs::symlink(target, &dst_path).ok();
                                }
                                expected_paths.insert(dst_path);
                            }
                            // 30 == SetAttr (flags + optional POSIX mode)
                            30u8 => {
                                if payload.len() < 2 + 1 {
                                    anyhow::bail!("bad SET_ATTR");
                                }
                                let nlen = u16::from_le_bytes([payload[0], payload[1]]) as usize;
                                if payload.len() < 2 + nlen + 1 {
                                    anyhow::bail!("bad SET_ATTR payload");
                                }
                                let rel = std::str::from_utf8(&payload[2..2 + nlen]).unwrap_or("");
                                let flags = payload[2 + nlen];
                                let dst = normalize_under_root(&base, Path::new(rel));
                                #[cfg(windows)]
                                {
                                    use std::os::windows::ffi::OsStrExt;
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
                                        let mut attrs = GetFileAttributesW(wide.as_ptr()).0;
                                        if attrs == u32::MAX {
                                            attrs = 0;
                                        }
                                        if (flags & 0b0000_0001) != 0 {
                                            attrs |= FILE_ATTRIBUTE_READONLY.0;
                                        } else {
                                            attrs &= !FILE_ATTRIBUTE_READONLY.0;
                                        }
                                        let _ = SetFileAttributesW(
                                            wide.as_ptr(),
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
                            // 31 == VerifyReq, respond with blake3 hash (32 bytes) in VerifyHash(32)
                            31u8 => {
                                if payload.len() < 2 {
                                    anyhow::bail!("bad VERIFY_REQ");
                                }
                                let nlen = u16::from_le_bytes([payload[0], payload[1]]) as usize;
                                if payload.len() < 2 + nlen {
                                    anyhow::bail!("bad VERIFY_REQ len");
                                }
                                let rel = std::str::from_utf8(&payload[2..2 + nlen]).unwrap_or("");
                                let path = normalize_under_root(&base, Path::new(rel));
                                // Compute hash
                                let mut f = std::fs::File::open(&path).with_context(|| {
                                    format!("open for verify {}", path.display())
                                })?;
                                let mut hasher = blake3::Hasher::new();
                                let mut buf = vec![0u8; 4 * 1024 * 1024];
                                loop {
                                    use std::io::Read as _;
                                    let n = f.read(&mut buf).context("read for verify")?;
                                    if n == 0 {
                                        break;
                                    }
                                    hasher.update(&buf[..n]);
                                }
                                let out = hasher.finalize();
                                write_frame(&mut stream, 32u8, out.as_bytes()).await?;
                            }
                            // 29 == FileRawStart (followed by raw body bytes, not framed)
                            29u8 => {
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
                                    u64::from_le_bytes(payload[off..off + 8].try_into().unwrap());
                                off += 8;
                                let mtime =
                                    i64::from_le_bytes(payload[off..off + 8].try_into().unwrap());
                                let dst = normalize_under_root(&base, Path::new(rel));
                                if let Some(parent) = dst.parent() {
                                    std::fs::create_dir_all(parent).ok();
                                }
                                let mut f = File::create(&dst)
                                    .with_context(|| format!("create {}", dst.display()))?;
                                let _ = f.set_len(size);
                                // Read raw body bytes directly from stream (sparse-friendly)
                                let mut remaining = size as usize;
                                let mut buf = vec![0u8; 4 * 1024 * 1024];
                                use std::io::{Seek, SeekFrom};
                                while remaining > 0 {
                                    let to_read = remaining.min(buf.len());
                                    let n = stream.read(&mut buf[..to_read]).await?;
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
                                }
                                let ft = FileTime::from_unix_time(mtime, 0);
                                let _ = set_file_mtime(&dst, ft);
                                expected_paths.insert(dst);
                            }
                            // 21 == DeltaStart, 22 == DeltaSample, 23 == DeltaEnd
                            21u8 => {
                                let _ = payload;
                            }
                            22u8 => {
                                let _ = payload;
                            }
                            23u8 => {
                                // Reply with empty need ranges (no ranged writes requested)
                                let mut zero = Vec::with_capacity(4);
                                zero.extend_from_slice(&0u32.to_le_bytes());
                                write_frame(&mut stream, 24u8, &zero).await?; // NeedRangesStart
                                write_frame(&mut stream, 26u8, &[]).await?; // NeedRangesEnd
                            }
                            // 27 == DeltaData, 28 == DeltaDone
                            27u8 => {
                                let _ = payload;
                            }
                            28u8 => {
                                let _ = payload;
                            }
                            // 7 == Done
                            7u8 => {
                                // Mirror delete if requested
                                if mirror {
                                    let mut all_dirs: Vec<PathBuf> = Vec::new();
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
                                            if !expected_paths.contains(&p) {
                                                let _ = std::fs::remove_file(&p);
                                            }
                                        }
                                    }
                                    all_dirs
                                        .sort_by_key(|p| std::cmp::Reverse(p.components().count()));
                                    for d in all_dirs {
                                        if !expected_paths.contains(&d) {
                                            let _ = std::fs::remove_dir(&d);
                                        }
                                    }
                                }
                                write_frame(&mut stream, 2u8, b"OK").await?;
                                break;
                            }
                            // Ignore range frames until implemented
                            24u8 | 25u8 | 26u8 => {
                                let _ = payload;
                                // For now, do nothing; future work will implement.
                                continue;
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

    pub async fn push(_host: &str, _port: u16, _dest: &Path, _src_root: &Path) -> Result<()> {
        // Placeholder for async client push implementation
        Ok(())
    }

    pub async fn pull(_host: &str, _port: u16, _src: &Path, _dest_root: &Path) -> Result<()> {
        // Placeholder for async client pull implementation
        Ok(())
    }
}
