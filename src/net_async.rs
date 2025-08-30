//! Experimental async (Tokio) transport scaffolding for Blit daemon/client.
//!
//! This module is not yet wired into the CLI. It provides minimal, compiling
//! stubs and a basic async server accept loop to start iterating toward the
//! TODO.md P0 goal of refactoring network I/O to Tokio.


#[cfg(feature = "server")]
pub mod server {
    use anyhow::{Context, Result};
    use crate::protocol::frame;
    use crate::protocol::timeouts::{read_deadline_ms, FRAME_HEADER_MS};
    use crate::protocol_core;
    use std::path::{Path, PathBuf};
    use std::time::Instant;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::time::{timeout, Duration};

    #[inline]
    async fn read_exact_timed<S>(stream: &mut S, buf: &mut [u8], ms: u64) -> Result<()>
    where
        S: tokio::io::AsyncRead + Unpin,
    {
        match timeout(Duration::from_millis(ms), async { stream.read_exact(buf).await }).await {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(e.into()),
            Err(_) => anyhow::bail!("read timeout ({} ms)", ms),
        }
    }

    async fn read_frame<S>(stream: &mut S) -> Result<(u8, Vec<u8>)>
    where
        S: tokio::io::AsyncRead + Unpin,
    {
        let mut hdr = [0u8; 11];
        match timeout(Duration::from_millis(FRAME_HEADER_MS), async { stream.read_exact(&mut hdr).await }).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => anyhow::bail!("frame header timeout ({} ms)", FRAME_HEADER_MS),
        }
        let (typ, len_u32) = protocol_core::parse_frame_header(&hdr)?;
        let len = len_u32 as usize;
        protocol_core::validate_frame_size(len)?;
        let mut payload = vec![0u8; len];
        if len > 0 {
            let ms = read_deadline_ms(len);
            read_exact_timed(stream, &mut payload, ms).await?;
        }
        Ok((typ, payload))
    }

    async fn write_frame<S>(stream: &mut S, t: u8, payload: &[u8]) -> Result<()>
    where
        S: tokio::io::AsyncWrite + Unpin,
    {
        let hdr = protocol_core::build_frame_header(t, payload.len() as u32);
        stream.write_all(&hdr).await?;
        if !payload.is_empty() {
            stream.write_all(payload).await?;
        }
        Ok(())
    }

    // Use protocol_core::normalize_under_root directly when needed

    pub async fn serve(bind: &str, root: &Path) -> Result<()> {
        let listener = TcpListener::bind(bind).await?;
        eprintln!("blit async daemon listening on {} (plaintext mode)", bind);
        loop {
            let (mut stream, peer) = listener.accept().await?;
            let _ = stream.set_nodelay(true);
            eprintln!("async conn from {}", peer);
            let root = root.to_path_buf();
            tokio::spawn(async move {
                if let Err(e) = handle_session(&mut stream, &root).await { eprintln!("async connection error: {}", e); }
            });
        }
    }

    pub async fn serve_with_tls(bind: &str, root: &Path, tls_config: rustls::ServerConfig) -> Result<()> {
        use std::sync::Arc;
        use tokio_rustls::TlsAcceptor;
        let listener = TcpListener::bind(bind).await?;
        let acceptor = TlsAcceptor::from(Arc::new(tls_config));
        eprintln!("blit async daemon (TLS) listening on {} root={}", bind, root.display());
        loop {
            let (tcp_stream, peer) = listener.accept().await?;
            let _ = tcp_stream.set_nodelay(true);
            eprintln!("async TLS conn from {}", peer);
            let root = root.to_path_buf();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let res = async move {
                    let mut stream = acceptor.accept(tcp_stream).await?;
                    handle_session(&mut stream, &root).await
                }.await;
                if let Err(e) = res { eprintln!("async TLS connection error: {}", e); }
            });
        }
    }

    async fn handle_session<S>(stream: &mut S, root: &Path) -> Result<()>
    where S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin
    {
        let started = Instant::now();
        // First frame: LIST_REQ or START
        let (typ, pl) = read_frame(stream).await?;
        if typ == frame::LIST_REQ {
            if pl.len() < 2 { anyhow::bail!("bad LIST_REQ payload"); }
            let nlen = u16::from_le_bytes([pl[0], pl[1]]) as usize;
            if pl.len() < 2 + nlen { anyhow::bail!("bad LIST_REQ path len"); }
            let pbytes = &pl[2..2+nlen];
            let preq_raw = std::str::from_utf8(pbytes).unwrap_or("");
            let mut rel = PathBuf::new();
            for comp in Path::new(preq_raw).components() { use std::path::Component::*; match comp { RootDir|CurDir|ParentDir|Prefix(_)=>{}, Normal(s)=>rel.push(s) } }
            let list_base = if rel.as_os_str().is_empty() { root.to_path_buf() } else { root.join(rel) };
            let mut items: Vec<(u8, String)> = vec![(1u8, "..".into())];
            if let Ok(rd) = std::fs::read_dir(&list_base) {
                for e in rd.flatten() {
                    let name = e.file_name().to_string_lossy().to_string();
                    let kind = if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {1} else {0};
                    items.push((kind, name));
                    if items.len() >= crate::protocol::MAX_LIST_ENTRIES { break; }
                }
            }
            items.sort_by(|a,b| match (a.0,b.0){ (1,0)=>std::cmp::Ordering::Less,(0,1)=>std::cmp::Ordering::Greater,_=>a.1.cmp(&b.1)});
            let mut out = Vec::new(); out.extend_from_slice(&(items.len() as u32).to_le_bytes());
            for (k,n) in items { out.push(k); out.extend_from_slice(&(n.len() as u16).to_le_bytes()); out.extend_from_slice(n.as_bytes()); }
            write_frame(stream, frame::LIST_RESP, &out).await?;
            return Ok(());
        }
        if typ != frame::START { anyhow::bail!("expected START frame"); }
        let (dest_rel, flags) = if pl.len() >= 3 {
            let n = u16::from_le_bytes([pl[0], pl[1]]) as usize;
            if pl.len() >= 3+n { (std::str::from_utf8(&pl[2..2+n]).unwrap_or("").to_string(), pl[2+n]) } else { ("".into(), 0) }
        } else { ("".into(), 0) };
        let mut rel = PathBuf::new();
        for comp in Path::new(&dest_rel).components() { use std::path::Component::*; match comp { RootDir|CurDir|ParentDir|Prefix(_)=>{}, Normal(s)=>rel.push(s) } }
        let base_dir = root.join(rel);
        std::fs::create_dir_all(&base_dir).ok();
        let pull = (flags & 0b0000_0010) != 0;
        write_frame(stream, frame::OK, b"OK").await?;

        // Session loop
        let mut verify_batch: Vec<String> = Vec::new();
        loop {
            let (t, payload) = read_frame(stream).await?;
            use crate::protocol::frame as fids;
            match t {
                fids::MANIFEST_START => { verify_batch.clear(); }
                fids::MANIFEST_ENTRY => {
                    if payload.len() < 3 { anyhow::bail!("bad MANIFEST_ENTRY"); }
                    let kind = payload[0];
                    let nlen = u16::from_le_bytes([payload[1], payload[2]]) as usize;
                    if payload.len() < 3+nlen { anyhow::bail!("bad MANIFEST_ENTRY name len"); }
                    let name = std::str::from_utf8(&payload[3..3+nlen]).unwrap_or("").to_string();
                    if kind == 0 || kind == 1 { verify_batch.push(name); }
                }
                fids::MANIFEST_END => {
                    if pull {
                        // Align client state then stream files
                        write_frame(stream, frame::NEED_LIST, &0u32.to_le_bytes()).await?;
                        use walkdir::WalkDir; use std::time::UNIX_EPOCH;
                        for ent in WalkDir::new(&base_dir).into_iter().filter_map(|e| e.ok()) {
                            if ent.file_type().is_file() {
                                let rel = ent.path().strip_prefix(&base_dir).unwrap_or(ent.path());
                                let rels = rel.to_string_lossy();
                                let md = std::fs::metadata(ent.path()).ok();
                                let size = md.as_ref().map(|m| m.len()).unwrap_or(0);
                                let mtime = md.and_then(|m| m.modified().ok()).and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs() as i64).unwrap_or(0);
                                let mut pls = Vec::with_capacity(2 + rels.len() + 8 + 8);
                                pls.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                                pls.extend_from_slice(rels.as_bytes());
                                pls.extend_from_slice(&size.to_le_bytes());
                                pls.extend_from_slice(&mtime.to_le_bytes());
                                write_frame(stream, frame::FILE_START, &pls).await?;
                                let mut f = std::fs::File::open(ent.path())?;
                                let mut buf = vec![0u8; 1024*1024];
                                loop { use std::io::Read as _; let n = f.read(&mut buf)?; if n==0 { break; } write_frame(stream, frame::FILE_DATA, &buf[..n]).await?; }
                                write_frame(stream, frame::FILE_END, &[]).await?;
                            }
                        }
                        write_frame(stream, frame::DONE, &[]).await?;
                    } else {
                        let mut resp = Vec::new();
                        resp.extend_from_slice(&(verify_batch.len() as u32).to_le_bytes());
                        for name in verify_batch.iter() { let nb = name.as_bytes(); resp.extend_from_slice(&(nb.len() as u16).to_le_bytes()); resp.extend_from_slice(nb); }
                        write_frame(stream, frame::NEED_LIST, &resp).await?;
                    }
                }
                fids::TAR_START => {
                    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(4);
                    let unpack_root = base_dir.clone();
                    let unpacker = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                        struct ChanReader { rx: tokio::sync::mpsc::Receiver<Vec<u8>>, buf: Vec<u8>, pos: usize, done: bool }
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
        let avail = self.buf.len().saturating_sub(self.pos);
        let n = out.len().min(avail);
        if n > 0 {
            out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
        }
        Ok(n)
    }
}
                        let mut ar = tar::Archive::new(ChanReader{ rx, buf: Vec::new(), pos: 0, done: false });
                        ar.set_overwrite(true);
                        ar.unpack(&unpack_root)?; Ok(()) });
                    loop { let (ti, pl2) = read_frame(stream).await?; if ti == fids::TAR_DATA { tx.send(pl2).await.ok(); } else if ti == fids::TAR_END { break; } else { anyhow::bail!("unexpected frame during tar: {}", ti); } }
                    drop(tx); unpacker.await??; write_frame(stream, frame::OK, b"TAR_OK").await?;
                }
                // Prepare/resize file and set mtime (idempotent). Payload: nlen u16 | name | size u64 | mtime i64
                fids::SET_ATTR => {
                    use std::io::Write as _;
                    if payload.len() < 2 + 8 + 8 { anyhow::bail!("bad SET_ATTR"); }
                    let nlen = u16::from_le_bytes([payload[0], payload[1]]) as usize;
                    if payload.len() < 2 + nlen + 8 + 8 { anyhow::bail!("bad SET_ATTR len"); }
                    let name = std::str::from_utf8(&payload[2..2+nlen]).unwrap_or("");
                    let mut off = 2 + nlen;
                    let size = u64::from_le_bytes(payload[off..off+8].try_into().unwrap());
                    off += 8;
                    let mtime = i64::from_le_bytes(payload[off..off+8].try_into().unwrap());
                    let dst = base_dir.join(name);
                    if let Some(parent) = dst.parent() { std::fs::create_dir_all(parent).ok(); }
                    let f = std::fs::OpenOptions::new().create(true).write(true).open(&dst)
                        .with_context(|| format!("open {}", dst.display()))?;
                    f.set_len(size).context("set file length")?;
                    let ft = filetime::FileTime::from_unix_time(mtime, 0);
                    let _ = filetime::set_file_mtime(&dst, ft);
                    write_frame(stream, frame::OK, b"OK").await?;
                }
                // Parallel range write. Payload: nlen u16 | name | off u64 | len u32 | raw bytes follow
                fids::PFILE_START => {
                    if payload.len() < 2 + 8 + 4 { anyhow::bail!("bad PFILE_START"); }
                    let nlen = u16::from_le_bytes([payload[0], payload[1]]) as usize;
                    if payload.len() < 2 + nlen + 8 + 4 { anyhow::bail!("bad PFILE_START len"); }
                    let name = std::str::from_utf8(&payload[2..2+nlen]).unwrap_or("");
                    let mut offp = 2 + nlen;
                    let off = u64::from_le_bytes(payload[offp..offp+8].try_into().unwrap());
                    offp += 8;
                    let mut remaining = u32::from_le_bytes(payload[offp..offp+4].try_into().unwrap()) as u64;
                    let dst = base_dir.join(name);
                    // Open for write
                    let f = std::fs::OpenOptions::new().write(true).open(&dst)
                        .with_context(|| format!("open {}", dst.display()))?;
                    // Read raw body and write at offset
                    use tokio::io::AsyncReadExt as _;
                    #[cfg(unix)]
                    use std::os::unix::fs::FileExt;
                    #[cfg(windows)]
                    use std::os::windows::fs::FileExt as WinFileExt;
                    let mut buf = vec![0u8; 4 * 1024 * 1024];
                    let mut cursor = off;
                    while remaining > 0 {
                        let to = remaining.min(buf.len() as u64) as usize;
                        let n = stream.read(&mut buf[..to]).await?;
                        if n == 0 { anyhow::bail!("eof during pfile range"); }
                        #[cfg(unix)]
                        {
                            f.write_at(&buf[..n], cursor).context("write_at")?;
                        }
                        #[cfg(windows)]
                        {
                            let _ = f.seek_write(&buf[..n], cursor).map_err(|e| anyhow::anyhow!(e))?;
                        }
                        cursor += n as u64;
                        remaining -= n as u64;
                    }
                    write_frame(stream, frame::OK, b"OK").await?;
                }
                fids::FILE_RAW_START => {
                    if payload.len() < 2 + 8 + 8 { anyhow::bail!("bad FILE_RAW_START"); }
                    let nlen = u16::from_le_bytes([payload[0], payload[1]]) as usize;
                    if payload.len() < 2 + nlen + 8 + 8 { anyhow::bail!("bad FILE_RAW_START len"); }
                    let rels = std::str::from_utf8(&payload[2..2+nlen]).unwrap_or("");
                    let mut off = 2 + nlen; let size = u64::from_le_bytes(payload[off..off+8].try_into().unwrap()); off+=8; let mtime = i64::from_le_bytes(payload[off..off+8].try_into().unwrap());
                    let dst = base_dir.join(rels);
                    if let Some(parent)=dst.parent(){ std::fs::create_dir_all(parent).ok(); }
                    use std::io::Write as _;
                    let mut f = std::fs::File::create(&dst).with_context(|| format!("create {}", dst.display()))?;
                    let mut remaining=size; let mut buf=vec![0u8; 4*1024*1024];
                    use tokio::io::AsyncReadExt as _;
                    while remaining>0 { let to=remaining.min(buf.len() as u64) as usize; let n=stream.read(&mut buf[..to]).await?; if n==0{ anyhow::bail!("eof during raw"); } f.write_all(&buf[..n]).context("write raw")?; remaining-=n as u64; }
                    let ft = filetime::FileTime::from_unix_time(mtime, 0); let _=filetime::set_file_mtime(&dst, ft);
                    write_frame(stream, frame::OK, b"OK").await?;
                }
                fids::DONE => { write_frame(stream, frame::OK, b"OK").await?; break; }
                fids::OK => { break; }
                _ => {}
            }
        }
        // Send a clean shutdown to emit TLS close_notify when applicable
        {
            use tokio::io::AsyncWriteExt as _;
            let _ = stream.shutdown().await;
        }
        let _ = started; // suppress unused if logs disabled
        Ok(())
    }
}
pub mod client {
    use crate::protocol::frame;
    use crate::url;
    use anyhow::{Context, Result};
    use filetime::{set_file_mtime, FileTime};
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;
    use tokio::sync::Mutex;
    use tokio::time::{timeout, Duration};
    use tokio_rustls::{client::TlsStream as ClientTlsStream, TlsConnector};

    #[inline]
    async fn write_all_timed(stream: &mut TcpStream, buf: &[u8], ms: u64) -> Result<()> {
        match timeout(Duration::from_millis(ms), async {
            stream.write_all(buf).await
        })
        .await
        {
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

    enum StreamAny {
        Plain(TcpStream),
        Tls(Box<ClientTlsStream<TcpStream>>),
    }

    impl StreamAny {
        async fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
            use tokio::io::AsyncWriteExt;
            match self {
                StreamAny::Plain(s) => s.write_all(buf).await,
                StreamAny::Tls(s) => s.write_all(buf).await,
            }
        }
        async fn read_exact(&mut self, buf: &mut [u8]) -> std::io::Result<()> {
            use tokio::io::AsyncReadExt;
            match self {
                StreamAny::Plain(s) => {
                    let _ = s.read_exact(buf).await?;
                    Ok(())
                }
                StreamAny::Tls(s) => {
                    let _ = s.read_exact(buf).await?;
                    Ok(())
                }
            }
        }
    
        async fn shutdown(&mut self) {
            use tokio::io::AsyncWriteExt;
            match self {
                StreamAny::Plain(s) => { let _ = s.shutdown().await; }
                StreamAny::Tls(s) => { let _ = s.shutdown().await; }
            }
        }
}

    // List a remote directory (non-recursive). Returns (name, is_dir).
    pub async fn list_dir(
        host: &str,
        port: u16,
        path: &std::path::Path,
        secure: bool,
    ) -> Result<Vec<(String, bool)>> {
        let mut stream = connect_secure(host, port, secure).await?;
        let path_str = path.to_string_lossy();
        let mut payload = Vec::with_capacity(2 + path_str.len());
        payload.extend_from_slice(&(path_str.len() as u16).to_le_bytes());
        payload.extend_from_slice(path_str.as_bytes());
        write_frame_any(&mut stream, frame::LIST_REQ, &payload).await?;
        let (t, pl) = read_frame_any(&mut stream).await?;
        if t != frame::LIST_RESP {
            anyhow::bail!("unexpected frame: {}", t);
        }
        let mut out = Vec::new();
        if pl.len() < 4 {
            return Ok(out);
        }
        let count = u32::from_le_bytes([pl[0], pl[1], pl[2], pl[3]]) as usize;
        let mut off = 4;
        for _ in 0..count {
            if off + 3 > pl.len() {
                break;
            }
            let kind = pl[off];
            off += 1;
            let nlen = u16::from_le_bytes([pl[off], pl[off + 1]]) as usize;
            off += 2;
            if off + nlen > pl.len() {
                break;
            }
            let name = String::from_utf8_lossy(&pl[off..off + nlen]).to_string();
            off += nlen;
            // Filter special marker entries if present
            if name.starts_with("[More entries") || name.starts_with("...") {
                continue;
            }
            out.push((name, kind == 1));
        }
        Ok(out)
    }

    // Recursively enumerate all files under remote base, returning relative paths (files only).
    pub async fn list_files_recursive(
        host: &str,
        port: u16,
        base: &std::path::Path,
        secure: bool,
    ) -> Result<Vec<std::path::PathBuf>> {
        let mut files = Vec::new();
        let mut stack: Vec<std::path::PathBuf> = vec![std::path::PathBuf::from(base)];
        while let Some(dir) = stack.pop() {
            let entries = list_dir(host, port, &dir, secure).await.unwrap_or_default();
            for (name, is_dir) in entries {
                if name == ".." {
                    continue;
                }
                let child = dir.join(&name);
                if is_dir {
                    stack.push(child);
                } else {
                    // Compute relative to base
                    let rel = child.strip_prefix(base).unwrap_or(&child).to_path_buf();
                    files.push(rel);
                }
            }
        }
        Ok(files)
    }

    // Request hashes for a batch of relative file paths under base. Returns map path->hash (32 bytes) for found files.
    pub async fn remote_hashes(
        host: &str,
        port: u16,
        base: &std::path::Path,
        rels: &[std::path::PathBuf],
        secure: bool,
    ) -> Result<std::collections::HashMap<String, [u8; 32]>> {
        let mut s = connect_secure(host, port, secure).await?;
        // Start session with base path
        let dest_s = base.to_string_lossy();
        let mut pl = Vec::with_capacity(2 + dest_s.len() + 1);
        pl.extend_from_slice(&(dest_s.len() as u16).to_le_bytes());
        pl.extend_from_slice(dest_s.as_bytes());
        pl.push(0); // flags
        write_frame_any(&mut s, frame::START, &pl).await?;
        let (typ, _ok) = read_frame_any(&mut s).await?;
        if typ != frame::OK {
            anyhow::bail!("server did not OK START");
        }

        for r in rels {
            let rstr = r.to_string_lossy();
            let mut plv = Vec::with_capacity(2 + rstr.len());
            plv.extend_from_slice(&(rstr.len() as u16).to_le_bytes());
            plv.extend_from_slice(rstr.as_bytes());
            write_frame_any(&mut s, frame::VERIFY_REQ, &plv).await?;
        }
        write_frame_any(&mut s, frame::VERIFY_DONE, &[]).await?;

        let mut out: std::collections::HashMap<String, [u8; 32]> = std::collections::HashMap::new();
        loop {
            let (t, pl) = read_frame_any(&mut s).await?;
            if t == frame::DONE {
                break;
            }
            if t != frame::VERIFY_HASH {
                anyhow::bail!("unexpected frame {} during verify", t);
            }
            if pl.len() < 1 + 2 {
                continue;
            }
            let status = pl[0];
            let nlen = u16::from_le_bytes([pl[1], pl[2]]) as usize;
            if pl.len() < 3 + nlen + 32 {
                continue;
            }
            let name = String::from_utf8_lossy(&pl[3..3 + nlen]).to_string();
            if status == 0 {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&pl[3 + nlen..3 + nlen + 32]);
                out.insert(name, arr);
            }
        }
        Ok(out)
    }

    async fn connect_secure(host: &str, port: u16, secure: bool) -> Result<StreamAny> {
        let addr = format!("{}:{}", host, port);
        let tcp = TcpStream::connect(&addr)
            .await
            .with_context(|| format!("connect {}", addr))?;
        let _ = tcp.set_nodelay(true);
        eprintln!("[client] connect_secure to {} secure={} (scheme)", addr, secure);
        if !secure {
            eprintln!("[client] using PLAINTEXT to {}", addr);
            return Ok(StreamAny::Plain(tcp));
        }
        eprintln!("[client] using TLS to {}", addr);
        let cfg = crate::tls::build_client_config_tofu(host, port);
        let cx = TlsConnector::from(std::sync::Arc::new(cfg));
        let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
            .map_err(|_| anyhow::anyhow!("Invalid server name for TLS: {}", host))?;
        let tls = cx.connect(server_name, tcp).await.map_err(|e| {
            anyhow::anyhow!(
                "TLS handshake failed (server may be running in unsafe mode): {}",
                e
            )
        })?;
        Ok(StreamAny::Tls(Box::new(tls)))
    }

    async fn write_frame_any(stream: &mut StreamAny, t: u8, payload: &[u8]) -> Result<()> {
        let hdr = crate::protocol_core::build_frame_header(t, payload.len() as u32);
        stream.write_all(&hdr).await?;
        if !payload.is_empty() {
            stream.write_all(payload).await?;
        }
        Ok(())
    }

    async fn read_frame_any(stream: &mut StreamAny) -> Result<(u8, Vec<u8>)> {
        use crate::protocol_core::{parse_frame_header, validate_frame_size};
        let mut hdr = [0u8; 11];
        stream.read_exact(&mut hdr).await?;
        let (typ, len_u32) = parse_frame_header(&hdr)?;
        let len = len_u32 as usize;
        validate_frame_size(len)?;
        let mut payload = vec![0u8; len];
        if len > 0 {
            stream.read_exact(&mut payload).await?;
        }
        Ok((typ, payload))
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
            connect_secure(&remote.host, remote.port, true),
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

        let hdr = crate::protocol_core::build_frame_header(frame::LIST_REQ, payload.len() as u32);

        match timeout(
            Duration::from_millis(crate::protocol::timeouts::CONNECT_MS),
            async {
                stream.write_all(&hdr).await?;
                stream.write_all(&payload).await
            },
        )
        .await
        {
            Ok(Ok(_)) => {}
            _ => return Ok(()),
        }

        let (typ, resp_payload) = read_frame_any(&mut stream).await?;

        if typ != frame::LIST_RESP {
            // ListResp
            return Ok(());
        }

        let mut off = 0;
        if resp_payload.len() < 4 {
            return Ok(());
        }
        let count = u32::from_le_bytes(
            resp_payload[off..off + 4]
                .try_into()
                .context("Invalid count bytes in NEED response")?,
        );
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
            let nlen = u16::from_le_bytes(
                resp_payload[off..off + 2]
                    .try_into()
                    .context("Invalid name length bytes in NEED response")?,
            ) as usize;
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
            let suggestion = format!("blit://{}:{}{}", remote.host, remote.port, suggestion_path);

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

    pub async fn remove_tree(host: &str, port: u16, path: &std::path::Path, secure: bool) -> Result<()> {
        let mut stream = connect_secure(host, port, secure).await?;
        // START with root "/" and no flags
        let root = "/";
        let mut payload = Vec::with_capacity(2 + root.len() + 1);
        payload.extend_from_slice(&(root.len() as u16).to_le_bytes());
        payload.extend_from_slice(root.as_bytes());
        payload.push(0);
        write_frame_any(&mut stream, frame::START, &payload).await?;
        let (typ, _resp) = read_frame_any(&mut stream).await?;
        if typ != frame::OK {
            anyhow::bail!("daemon error starting remove");
        }

        // Send RemoveTreeReq
        let rel = path.to_string_lossy();
        let mut pl = Vec::with_capacity(2 + rel.len());
        pl.extend_from_slice(&(rel.len() as u16).to_le_bytes());
        pl.extend_from_slice(rel.as_bytes());
        write_frame_any(&mut stream, frame::REMOVE_TREE_REQ, &pl).await?;
        let (t, resp) = read_frame_any(&mut stream).await?;
        if t != frame::REMOVE_TREE_RESP {
            anyhow::bail!("bad response to remove");
        }
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
        let secure = !args.never_tell_me_the_odds;
        let mut stream = connect_secure(host, port, secure).await?;

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

        write_frame_any(&mut stream, frame::START, &payload).await?;
        let (typ, resp) = read_frame_any(&mut stream).await?;
        if typ != frame::OK {
            // OK
            anyhow::bail!("daemon error: {}", String::from_utf8_lossy(&resp));
        }

        // Send manifest by walking with symlink awareness
        use walkdir::WalkDir;
        write_frame_any(&mut stream, frame::MANIFEST_START, &[]).await?; // ManifestStart
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
                write_frame_any(&mut stream, frame::MANIFEST_ENTRY, &pl).await?;
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
                    write_frame_any(&mut stream, frame::MANIFEST_ENTRY, &pl).await?;
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
                    write_frame_any(&mut stream, frame::MANIFEST_ENTRY, &pl).await?;
                }
            }
        }
        write_frame_any(&mut stream, frame::MANIFEST_END, &[]).await?; // ManifestEnd

        // Read need list
        let (tneed, plneed) = read_frame_any(&mut stream).await?;
        if tneed != frame::NEED_LIST {
            // NeedList
            anyhow::bail!("server did not reply with NeedList");
        }

        let mut needed = std::collections::HashSet::new();
        let mut off = 0usize;
        if plneed.len() >= 4 {
            let count = u32::from_le_bytes(
                plneed[off..off + 4]
                    .try_into()
                    .context("Invalid count bytes in NEED response")?,
            ) as usize;
            // Sanity check: limit to 1 million entries to prevent DoS
            const MAX_NEED_ENTRIES: usize = 1_000_000;
            if count > MAX_NEED_ENTRIES {
                anyhow::bail!(
                    "NEED_LIST count exceeds maximum allowed ({}): {}",
                    MAX_NEED_ENTRIES,
                    count
                );
            }
            off += 4;
            for _ in 0..count {
                if off + 2 > plneed.len() {
                    break;
                }
                let nlen = u16::from_le_bytes(
                    plneed[off..off + 2]
                        .try_into()
                        .context("Invalid name length bytes in NEED response")?,
                ) as usize;
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
            write_frame_any(&mut stream, frame::TAR_START, &[]).await?; // TarStart
            // Deeper buffer for better pipelining over higher latency
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
            let tar_task_src_root = src_root.to_path_buf();
            let tar_task = tokio::task::spawn_blocking(move || -> Result<()> {
                let mut w = crate::net_async::client::TarChanWriter {
                    tx,
                    buf: Vec::with_capacity(2 * 1024 * 1024),
                    cap: 2 * 1024 * 1024,
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
                write_frame_any(&mut stream, frame::TAR_DATA, &chunk).await?; // TarData
            }

            tar_task.await??;
            write_frame_any(&mut stream, frame::TAR_END, &[]).await?; // TarEnd
            let (t_ok, _) = read_frame_any(&mut stream).await?;
            if t_ok != frame::OK {
                anyhow::bail!("server TAR error");
            }
        }

        // Auto-tune workers/chunk if user hasn't overridden and based on simple heuristics
        let overridden_workers = std::env::args()
            .any(|a| a == "--net-workers" || a.starts_with("--net-workers="));
        let overridden_chunk = std::env::args()
            .any(|a| a == "--net-chunk-mb" || a.starts_with("--net-chunk-mb="));
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let mut eff_workers = args.net_workers;
        let mut eff_chunk_mb = args.net_chunk_mb;
        if !overridden_workers {
            let large_count = large_files.len().max(1);
            // Aggressive default to target 10GbE; cap by available work and 32 overall
            eff_workers = std::cmp::min(large_count, std::cmp::max(8, cpus)).clamp(2, 32);
        }
        if !overridden_chunk {
            // Bigger chunks reduce syscall/record overhead
            eff_chunk_mb = if args.ludicrous_speed { 16 } else { 8 };
        }

        let large_cap = large_files.len().max(1);
        let work = Arc::new(Mutex::new(large_files));
        let mut handles = vec![];
        // Cap workers by number of large files to avoid idle START→DONE sessions
        let worker_count = std::cmp::min(eff_workers.clamp(1, 32), large_cap);
        let chunk_bytes: usize = eff_chunk_mb.clamp(1, 32) * 1024 * 1024;
        for _ in 0..worker_count {
            let work_clone = Arc::clone(&work);
            let host = host.to_string();
            let dest = dest.to_path_buf();
            let src_root = src_root.to_path_buf();
            // Preserve the chosen security mode for worker connections
            let worker_secure = secure;

            let handle = tokio::spawn(async move {
                let secure = worker_secure;
                let mut s = connect_secure(&host, port, secure).await?;
                // Start worker connection
                let dest_s = dest.to_string_lossy();
                let mut pl = Vec::with_capacity(2 + dest_s.len() + 1);
                pl.extend_from_slice(&(dest_s.len() as u16).to_le_bytes());
                pl.extend_from_slice(dest_s.as_bytes());
                pl.push(0); // Flags (inherit speed profile server-side)
                write_frame_any(&mut s, frame::START, &pl).await?;
                let (typ, resp) = read_frame_any(&mut s).await?;
                if typ != frame::OK {
                    anyhow::bail!("worker daemon error: {}", String::from_utf8_lossy(&resp));
                }

                loop {
                    let job = {
                        let mut q = work_clone.lock().await;
                        q.pop()
                    };
                    if let Some(fe) = job {
                        // For very large files, split into parallel ranges across workers
                        let rel = fe.path.strip_prefix(&src_root).unwrap_or(&fe.path);
                        let rels = rel.to_string_lossy();
                        let md = std::fs::metadata(&fe.path)?;
                        let size = md.len();
                        let mtime = md
                            .modified()?
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs() as i64;

                        if size >= 256 * 1024 * 1024 {
                            // Pre-create file via SET_ATTR on a fresh control START
                            let mut ctrl = connect_secure(&host, port, secure).await?;
                            let mut pl = Vec::with_capacity(2 + rels.len() + 8 + 8);
                            pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                            pl.extend_from_slice(rels.as_bytes());
                            pl.extend_from_slice(&size.to_le_bytes());
                            pl.extend_from_slice(&mtime.to_le_bytes());
                            // New session for control
                            let dest_s = dest.to_string_lossy();
                            let mut sp = Vec::with_capacity(2 + dest_s.len() + 1);
                            sp.extend_from_slice(&(dest_s.len() as u16).to_le_bytes());
                            sp.extend_from_slice(dest_s.as_bytes());
                            sp.push(0);
                            write_frame_any(&mut ctrl, frame::START, &sp).await?;
                            let (_t, _r) = read_frame_any(&mut ctrl).await?;
                            write_frame_any(&mut ctrl, frame::SET_ATTR, &pl).await?;
                            let (_tok, _pl) = read_frame_any(&mut ctrl).await?;
                            write_frame_any(&mut ctrl, frame::DONE, &[]).await?;
                            let _ = read_frame_any(&mut ctrl).await?;

                            // Build ranges and send via PFILE on this worker connection
                            let mut off0 = 0u64;
                            let stride = chunk_bytes as u64;
                            let mut f = std::fs::File::open(&fe.path)?;
                            use std::io::Read as _;
                            let mut buf = vec![0u8; chunk_bytes];
                            while off0 < size {
                                let len = std::cmp::min(stride, size - off0) as usize;
                                // Read from disk
                                let mut rd = 0usize;
                                while rd < len {
                                    let n = f.read(&mut buf[rd..len])?;
                                    if n == 0 { break; }
                                    rd += n;
                                }
                                if rd == 0 { break; }
                                // Send header + raw bytes
                                let mut ph = Vec::with_capacity(2 + rels.len() + 8 + 4);
                                ph.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                                ph.extend_from_slice(rels.as_bytes());
                                ph.extend_from_slice(&off0.to_le_bytes());
                                ph.extend_from_slice(&(rd as u32).to_le_bytes());
                                write_frame_any(&mut s, frame::PFILE_START, &ph).await?;
                                match &mut s {
                                    StreamAny::Plain(raw) => { raw.write_all(&buf[..rd]).await?; }
                                    StreamAny::Tls(tls) => { use tokio::io::AsyncWriteExt; tls.write_all(&buf[..rd]).await?; }
                                }
                                let (_tok, _plk) = read_frame_any(&mut s).await?;
                                off0 += rd as u64;
                            }
                        } else {
                            // Fallback: raw single-stream file on this connection
                            let mut pl_raw = Vec::with_capacity(2 + rels.len() + 8 + 8);
                            pl_raw.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                            pl_raw.extend_from_slice(rels.as_bytes());
                            pl_raw.extend_from_slice(&size.to_le_bytes());
                            pl_raw.extend_from_slice(&mtime.to_le_bytes());
                            write_frame_any(&mut s, frame::FILE_RAW_START, &pl_raw).await?;
                            let mut f = tokio::fs::File::open(&fe.path).await?;
                            use tokio::io::AsyncReadExt;
                            let mut buf = vec![0u8; chunk_bytes];
                            let mut remaining = size;
                            while remaining > 0 {
                                let to_read = (remaining as usize).min(buf.len());
                                let n = f.read(&mut buf[..to_read]).await?;
                                if n == 0 { break; }
                                match &mut s {
                                    StreamAny::Plain(raw) => { raw.write_all(&buf[..n]).await?; }
                                    StreamAny::Tls(tls) => { use tokio::io::AsyncWriteExt; tls.write_all(&buf[..n]).await?; }
                                }
                                remaining -= n as u64;
                            }
                        }
                    } else { break; }
                }
                write_frame_any(&mut s, frame::DONE, &[]).await?; // Done
                let (t_ok, _) = read_frame_any(&mut s).await?;
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

        write_frame_any(&mut stream, frame::DONE, &[]).await?; // Final Done
        let (t_ok, _) = read_frame_any(&mut stream).await?;
        if t_ok != frame::OK {
            anyhow::bail!("server did not ack final DONE");
        }
        // Graceful close (sends TLS close_notify when applicable)
        stream.shutdown().await;
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
        let secure = !args.never_tell_me_the_odds;
        let mut stream = connect_secure(host, port, secure).await?;

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

        write_frame_any(&mut stream, 1, &payload).await?;
        let (typ, resp) = read_frame_any(&mut stream).await?;
        if typ != 2u8 {
            anyhow::bail!("daemon error: {}", String::from_utf8_lossy(&resp));
        }

        // Send manifest of local destination to allow delta
        write_frame_any(&mut stream, frame::MANIFEST_START, &[]).await?; // ManifestStart
        let filter = crate::fs_enum::FileFilter {
            exclude_files: args.exclude_files.clone(),
            exclude_dirs: args.exclude_dirs.clone(),
            min_size: None,
            max_size: None,
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
            write_frame_any(&mut stream, frame::MANIFEST_ENTRY, &pl).await?;
            // ManifestEntry
        }
        write_frame_any(&mut stream, frame::MANIFEST_END, &[]).await?; // ManifestEnd

        let (_tneed, _plneed) = read_frame_any(&mut stream).await?;

        let mut expected_paths = HashSet::new();
        let mut current_file: Option<(tokio::fs::File, std::path::PathBuf, u64, i64)> = None;

        loop {
            let (t, pl) = read_frame_any(&mut stream).await?;
            match t {
                8u8 => {
                    // TarStart
                    let (tx, rx) = tokio::sync::mpsc::channel::<Vec<u8>>(64);
                    let unpack_dest = dest_root.to_path_buf();
                    let unpacker = tokio::task::spawn_blocking(move || -> Result<()> {
                        let reader = ChanReader {
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
                        let (ti, pli) = read_frame_any(&mut stream).await?;
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
                    write_frame_any(&mut stream, frame::OK, b"OK").await?;
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
                    let size = u64::from_le_bytes(
                        pl[off..off + 8]
                            .try_into()
                            .context("Invalid size bytes in FILE_START")?,
                    );
                    off += 8;
                    let mtime = i64::from_le_bytes(
                        pl[off..off + 8]
                            .try_into()
                            .context("Invalid mtime bytes in FILE_START")?,
                    );
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
                    #[cfg(unix)]
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
                    write_frame_any(&mut stream, frame::OK, b"OK").await?;
                    stream.shutdown().await;
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
                if (entry.file_type().is_file() || entry.file_type().is_symlink())
                    && !expected_paths.contains(&p)
                {
                    tokio::fs::remove_file(&p).await.ok();
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
