use anyhow::{Context, Result};
// Use library modules from the robosync lib crate
// Unused imports removed for 3.1 cleanup
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::stdout;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

thread_local! {
    static MTIME_STORE: std::cell::RefCell<std::collections::HashMap<String, i64>> = std::cell::RefCell::new(std::collections::HashMap::new());
}

fn apply_preserved_mtime(path: &Path) -> Result<()> {
    use filetime::{set_file_mtime, FileTime};
    let rel = path.to_string_lossy().to_string();
    let mtime_opt = MTIME_STORE.with(|mt| mt.borrow_mut().remove(&rel));
    if let Some(secs) = mtime_opt {
        let ft = FileTime::from_unix_time(secs, 0);
        set_file_mtime(path, ft).ok();
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn preallocate_file_linux(file: &std::fs::File, size: u64) {
    use std::os::fd::AsRawFd;
    let fd = file.as_raw_fd();
    unsafe {
        let r = libc::posix_fallocate(fd, 0, size as libc::off_t);
        if r != 0 {
            // Ignore errors; fallback to sparse allocation
        }
    }
}
#[cfg(not(target_os = "linux"))]
fn preallocate_file_linux(_file: &std::fs::File, _size: u64) {}
const MAGIC: &[u8; 4] = b"RSNC";
const VERSION: u16 = 1;
const MAX_FRAME_SIZE: usize = 32 * 1024 * 1024;

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
    CompressedManifest = 20,
    DeltaStart = 21,
    DeltaSample = 22,
    DeltaEnd = 23,
    NeedRangesStart = 24,
    NeedRange = 25,
    NeedRangesEnd = 26,
    DeltaData = 27,
    DeltaDone = 28,
    FileRawStart = 29,
    SetAttr = 30,
    VerifyReq = 31,
    VerifyHash = 32,
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

fn build_frame(t: FrameType, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + 2 + 1 + 4 + payload.len());
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(&VERSION.to_le_bytes());
    buf.push(t as u8);
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    buf
}

#[cfg(target_os = "linux")]
fn sendfile_to_stream(file: &std::fs::File, stream: &TcpStream, mut remaining: u64) -> Result<()> {
    use std::os::fd::AsRawFd;
    let in_fd = file.as_raw_fd();
    let out_fd = stream.as_raw_fd();
    while remaining > 0 {
        let to_send = remaining.min(8 * 1024 * 1024) as usize;
        let sent = unsafe { libc::sendfile(out_fd, in_fd, std::ptr::null_mut(), to_send) };
        if sent < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e.into());
        }
        if sent == 0 {
            break;
        }
        remaining -= sent as u64;
    }
    Ok(())
}

// Fallback for Unix targets that are not Linux or macOS
#[cfg(all(not(target_os = "linux"), not(target_os = "macos"), not(windows)))]
fn sendfile_to_stream(
    file: &std::fs::File,
    stream: &mut TcpStream,
    mut remaining: u64,
) -> Result<()> {
    let mut f = file.try_clone()?;
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    while remaining > 0 {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        stream.write_all(&buf[..n])?;
        remaining -= n as u64;
    }
    Ok(())
}

#[cfg(windows)]
fn sendfile_to_stream(
    file: &std::fs::File,
    stream: &mut TcpStream,
    mut remaining: u64,
) -> anyhow::Result<()> {
    use std::os::windows::io::{AsRawHandle, AsRawSocket};
    use windows::Win32::Networking::WinSock::{TransmitFile, TF_WRITE_BEHIND};
    let sock = stream.as_raw_socket();
    let hfile = file.as_raw_handle();
    while remaining > 0 {
        let chunk = remaining.min(8 * 1024 * 1024) as u32;
        let ok =
            unsafe { TransmitFile(sock, hfile, chunk, 0, None, None, TF_WRITE_BEHIND).as_bool() };
        if !ok {
            // Fallback to buffered copy
            let mut f = file.try_clone()?;
            let mut buf = vec![0u8; 4 * 1024 * 1024];
            while remaining > 0 {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                stream.write_all(&buf[..n])?;
                remaining -= n as u64;
            }
            return Ok(());
        }
        remaining -= chunk as u64;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn sendfile_to_stream(
    file: &std::fs::File,
    stream: &mut TcpStream,
    mut remaining: u64,
) -> anyhow::Result<()> {
    use std::os::fd::AsRawFd;
    let in_fd = file.as_raw_fd();
    let out_fd = stream.as_raw_fd();
    let mut offset: libc::off_t = 0;
    while remaining > 0 {
        let mut len: libc::off_t = remaining.min(8 * 1024 * 1024) as libc::off_t;
        let r = unsafe { libc::sendfile(in_fd, out_fd, offset, &mut len, std::ptr::null_mut(), 0) };
        if r == -1 {
            let err = std::io::Error::last_os_error();
            if let Some(raw) = err.raw_os_error() {
                if raw == libc::EAGAIN || raw == libc::EINTR {
                    if len > 0 {
                        remaining -= len as u64;
                        offset += len;
                    }
                    continue;
                }
            }
            break;
        } else {
            if len > 0 {
                remaining -= len as u64;
                offset += len;
            }
        }
    }
    if remaining > 0 {
        use std::io::{Read, Seek, SeekFrom, Write};
        let mut f = file.try_clone()?;
        f.seek(SeekFrom::Start(offset as u64))?;
        let mut buf = vec![0u8; 4 * 1024 * 1024];
        while remaining > 0 {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            stream.write_all(&buf[..n])?;
            remaining -= n as u64;
        }
    }
    Ok(())
}
fn recv_raw_to_file(
    stream: &mut TcpStream,
    file: &mut std::fs::File,
    mut remaining: u64,
) -> Result<()> {
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    while remaining > 0 {
        let to_read = remaining.min(buf.len() as u64) as usize;
        let n = stream.read(&mut buf[..to_read])?;
        if n == 0 {
            anyhow::bail!("unexpected EOF during raw file body");
        }
        file.write_all(&buf[..n])?;
        remaining -= n as u64;
    }
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
    if len > MAX_FRAME_SIZE {
        anyhow::bail!("frame too large: {}", len);
    }
    let payload = read_exact(stream, len)?;
    Ok((typ, payload))
}

// Socket tuning: enlarge buffers and disable Nagle for throughput
#[allow(unused_variables)]
fn tune_socket(stream: &TcpStream) {
    let _ = stream.set_nodelay(true);
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        let fd = stream.as_raw_fd();
        unsafe {
            let sz: libc::c_int = 8 * 1024 * 1024;
            let p = &sz as *const _ as *const libc::c_void;
            let _ = libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                p,
                std::mem::size_of_val(&sz) as libc::socklen_t,
            );
            let _ = libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                p,
                std::mem::size_of_val(&sz) as libc::socklen_t,
            );
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawSocket;
        use windows::Win32::Networking::WinSock::{setsockopt, SOL_SOCKET, SO_RCVBUF, SO_SNDBUF};
        let s = stream.as_raw_socket();
        unsafe {
            let mut sz: i32 = 8 * 1024 * 1024;
            let p = &mut sz as *mut _ as *const u8;
            let _ = setsockopt(
                s,
                SOL_SOCKET as i32,
                SO_SNDBUF as i32,
                p,
                std::mem::size_of_val(&sz) as i32,
            );
            let _ = setsockopt(
                s,
                SOL_SOCKET as i32,
                SO_RCVBUF as i32,
                p,
                std::mem::size_of_val(&sz) as i32,
            );
        }
    }
}

#[inline]
fn compute_attr_flags(_path: &std::path::Path) -> u8 {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::FileSystem::{GetFileAttributesW, FILE_ATTRIBUTE_READONLY};
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let attrs = GetFileAttributesW(wide.as_ptr());
            if attrs.0 == u32::MAX {
                return 0;
            }
            let mut flags = 0u8;
            if (attrs.0 & FILE_ATTRIBUTE_READONLY.0) != 0 {
                flags |= 0b0000_0001;
            }
            return flags;
        }
    }
    #[cfg(not(windows))]
    {
        0
    }
}

#[inline]
fn apply_windows_attrs(_path: &std::path::Path, _flags: u8) {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::Storage::FileSystem::{
            GetFileAttributesW, SetFileAttributesW, FILE_ATTRIBUTE_READONLY,
            FILE_FLAGS_AND_ATTRIBUTES,
        };
        let wide: Vec<u16> = path
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
            let _ = SetFileAttributesW(wide.as_ptr(), FILE_FLAGS_AND_ATTRIBUTES(attrs));
        }
    }
}

fn hash_file_blake3(p: &std::path::Path) -> anyhow::Result<[u8; 32]> {
    let mut f = std::fs::File::open(p)?;
    let mut hasher = blake3::Hasher::new();
    let mut buf = vec![0u8; 4 * 1024 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(*hasher.finalize().as_bytes())
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
                .map_err(|e| std::io::Error::other(e))?;
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

fn handle_tar_stream(
    stream: &mut TcpStream,
    base: &Path,
    received: &mut HashSet<PathBuf>,
) -> Result<(u64, u64)> {
    // Unpack tar stream under base while tracking received file paths
    let mut reader = TarFrameReader::new(stream);
    let mut archive = tar::Archive::new(&mut reader);
    archive.set_overwrite(true);
    let entries = archive.entries()?;
    let mut file_count: u64 = 0;
    let mut total_bytes: u64 = 0;
    for res in entries {
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
                use std::path::Component::{CurDir, Normal, ParentDir, Prefix, RootDir};
                for comp in rel.components() {
                    match comp {
                        CurDir => {}
                        Normal(s) => dst.push(s),
                        RootDir | Prefix(_) => {}
                        ParentDir => anyhow::bail!("tar symlink contains parent component"),
                    }
                }
                if let Some(parent) = dst.parent() {
                    fs::create_dir_all(parent).ok();
                }
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
        if et.is_file() {
            if let Ok(sz) = entry.header().size() {
                total_bytes = total_bytes.saturating_add(sz);
                file_count = file_count.saturating_add(1);
            }
        }
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
    loop {
        let (typ, _pl) = read_frame(stream)?;
        if typ == FrameType::TarEnd as u8 {
            break;
        }
        if typ != FrameType::TarData as u8 {
            anyhow::bail!("unexpected frame while finishing tar: {}", typ);
        }
    }
    // Ack TAR sequence complete
    write_frame(stream, FrameType::Ok, b"TAR_OK")?;
    Ok((file_count, total_bytes))
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
                tune_socket(&stream);
                let peer = stream
                    .peer_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|_| "unknown".to_string());
                eprintln!("conn from {}", peer);
                if let Err(e) = handle_conn(&mut stream, root) {
                    eprintln!(
                        "connection error during handling (possible client disconnect): {}",
                        e
                    );
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
    let flags = if payload.len() > 2 + dlen {
        payload[2 + dlen]
    } else {
        0
    };
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

    // State for delta algorithm per file
    struct DeltaState {
        dst_path: PathBuf,
        file_size: u64,
        mtime: i64,
        granule: u64,
        sample: usize,
        need_ranges: Vec<(u64, u64)>,
    }
    let mut delta_state: Option<DeltaState> = None;

    // Tracks all relative paths the client reported in its manifest (files, symlinks, dirs)
    let mut client_present: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut expected_paths: HashSet<PathBuf> = HashSet::new();
    let mut pending: Option<(u8, Vec<u8>)> = None;
    // Delta-transfer state for current connection
    let mut delta_active: bool = false;
    if let Ok((t0, pl0)) = read_frame(stream) {
        if t0 == FrameType::ManifestStart as u8 {
            let mut needed: std::collections::HashSet<String> = std::collections::HashSet::new();
            loop {
                let (t2, pl2) = read_frame(stream)?;
                if t2 == FrameType::ManifestEntry as u8 {
                    if pl2.len() < 1 + 2 {
                        anyhow::bail!("bad MANIFEST_ENTRY");
                    }
                    let kind = pl2[0];
                    let nlen = u16::from_le_bytes([pl2[1], pl2[2]]) as usize;
                    if pl2.len() < 3 + nlen {
                        anyhow::bail!("bad MANIFEST_ENTRY path len");
                    }
                    let rel = std::str::from_utf8(&pl2[3..3 + nlen]).context("utf8 rel")?;
                    let relp = PathBuf::from(rel);
                    let dst = normalize_under_root(&base, &relp)?;
                    // Record that the client has this path
                    client_present.insert(rel.to_string());
                    match kind {
                        0 => {
                            // file: size u64 | mtime i64
                            if pl2.len() < 3 + nlen + 8 + 8 {
                                anyhow::bail!("bad MANIFEST_ENTRY file fields");
                            }
                            let off = 3 + nlen;
                            let size = u64::from_le_bytes(pl2[off..off + 8].try_into().unwrap());
                            let mtime =
                                i64::from_le_bytes(pl2[off + 8..off + 16].try_into().unwrap());
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
                            if need {
                                needed.insert(rel.to_string());
                            }
                            expected_paths.insert(dst.clone());
                        }
                        1 => {
                            // symlink: tlen u16 | target bytes
                            if pl2.len() < 3 + nlen + 2 {
                                anyhow::bail!("bad MANIFEST_ENTRY symlink fields");
                            }
                            let off = 3 + nlen;
                            let tlen = u16::from_le_bytes([pl2[off], pl2[off + 1]]) as usize;
                            if pl2.len() < off + 2 + tlen {
                                anyhow::bail!("bad MANIFEST_ENTRY symlink target len");
                            }
                            let target =
                                std::str::from_utf8(&pl2[off + 2..off + 2 + tlen]).unwrap_or("");
                            let mut need = true;
                            if let Ok(smd) = std::fs::symlink_metadata(&dst) {
                                if smd.file_type().is_symlink() {
                                    if let Ok(cur) = std::fs::read_link(&dst) {
                                        if cur.as_os_str() == std::ffi::OsStr::new(target) {
                                            need = false;
                                        }
                                    }
                                }
                            }
                            if need {
                                needed.insert(rel.to_string());
                            }
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
                    for ent in walkdir::WalkDir::new(&base)
                        .follow_links(false)
                        .into_iter()
                        .filter_map(|e| e.ok())
                    {
                        if ent.file_type().is_dir() {
                            let rel = ent.path().strip_prefix(&base).unwrap_or(ent.path());
                            if rel.as_os_str().is_empty() {
                                continue;
                            }
                            let rels = rel.to_string_lossy();
                            let mut pl = Vec::with_capacity(2 + rels.len());
                            pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                            pl.extend_from_slice(rels.as_bytes());
                            write_frame(stream, FrameType::MkDir, &pl)?;
                        }
                    }
                }
                // Collect symlinks, small files, and large files for pull
                let mut small_files: Vec<(PathBuf, u64, i64)> = vec![];
                let mut large_files: Vec<(PathBuf, u64, i64)> = vec![];
                let mut symlinks: Vec<(String, String)> = vec![]; // (rel, target)
                for ent in walkdir::WalkDir::new(&base)
                    .follow_links(false)
                    .into_iter()
                    .filter_map(|e| e.ok())
                {
                    let rel = ent.path().strip_prefix(&base).unwrap_or(ent.path());
                    if rel.as_os_str().is_empty() {
                        continue;
                    }
                    let rels = rel.to_string_lossy().to_string();
                    if !send_all {
                        if !(needed.contains(&rels) || !client_present.contains(&rels)) {
                            continue;
                        }
                    }
                    if ent.file_type().is_symlink() {
                        if let Ok(t) = std::fs::read_link(ent.path()) {
                            let targ = t.to_string_lossy().to_string();
                            symlinks.push((rels, targ));
                        }
                    } else if ent.file_type().is_file() {
                        let md = match ent.metadata() {
                            Ok(m) => m,
                            Err(_) => continue,
                        };
                        let size = md.len();
                        let mtime = md
                            .modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_secs() as i64)
                            .unwrap_or(0);
                        let entry = (ent.path().to_path_buf(), size, mtime);
                        if size < 1_048_576 {
                            small_files.push(entry);
                        } else {
                            large_files.push(entry);
                        }
                    }
                }

                // Send tar bundle for small files
                if !small_files.is_empty() {
                    write_frame(stream, FrameType::TarStart, &[])?;

                    struct FrameWriter<'a> (&'a mut TcpStream, Vec<u8>);
                    impl<'a> FrameWriter<'a> {
                        fn new(s: &'a mut TcpStream) -> Self { Self(s, Vec::with_capacity(4*1024*1024)) }
                        fn flush(&mut self) -> Result<()> {
                            if !self.1.is_empty() {
                                let frame = build_frame(FrameType::TarData, &self.1);
                                self.0.write_all(&frame)?;
                                self.1.clear();
                            }
                            Ok(())
                        }
                    }
                    impl<'a> Write for FrameWriter<'a> {
                        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                            let mut rem = b;
                            while !rem.is_empty() {
                                let space = 4*1024*1024 - self.1.len();
                                let take = space.min(rem.len());
                                self.1.extend_from_slice(&rem[..take]);
                                rem = &rem[take..];
                                if self.1.len() == 4*1024*1024 {
                                    let frame = build_frame(FrameType::TarData, &self.1);
                                    self.0.write_all(&frame)?;
                                    self.1.clear();
                                }
                            }
                            Ok(b.len())
                        }
                        fn flush(&mut self) -> std::io::Result<()> { self.flush().map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)) }
                    }

                    let mut fw = FrameWriter::new(stream);
                    {
                        let mut builder = tar::Builder::new(&mut fw);
                        for (path, _, _) in &small_files {
                            let rel = path.strip_prefix(&base).unwrap_or(path);
                            builder.append_path_with_name(path, rel)?;
                        }
                        builder.finish()?;
                    }
                    fw.flush()?;
                    write_frame(stream, FrameType::TarEnd, &[])?;
                    let (t_ok, _) = read_frame(stream)?;
                    if t_ok != FrameType::Ok as u8 {
                        anyhow::bail!("client TAR error");
                    }
                }

                // Send SetAttr for small files (attributes and modes)
                for (path, _, _) in &small_files {
                    let rel = path.strip_prefix(&base).unwrap_or(path).to_string_lossy();
                    let md = std::fs::metadata(path)?;
                    let mut pla = Vec::with_capacity(2 + rel.len() + 1 + 4);
                    pla.extend_from_slice(&(rel.len() as u16).to_le_bytes());
                    pla.extend_from_slice(rel.as_bytes());
                    pla.push(compute_attr_flags(path));
                    #[cfg(unix)] {
                        use std::os::unix::fs::PermissionsExt;
                        let mode = md.permissions().mode();
                        pla.extend_from_slice(&mode.to_le_bytes());
                    }
                    write_frame(stream, FrameType::SetAttr, &pla)?;
                }

                // Send symlinks
                for (rels, targ) in symlinks {
                    let mut pl = Vec::with_capacity(2 + rels.len() + 2 + targ.len());
                    pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                    pl.extend_from_slice(rels.as_bytes());
                    pl.extend_from_slice(&(targ.len() as u16).to_le_bytes());
                    pl.extend_from_slice(targ.as_bytes());
                    write_frame(stream, FrameType::Symlink, &pl)?;
                }

                // Send large files individually
                for (path, size, mtime) in large_files {
                    let rels = path.strip_prefix(&base).unwrap_or(&path).to_string_lossy();
                    let mut pl = Vec::with_capacity(2 + rels.len() + 8 + 8);
                    pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                    pl.extend_from_slice(rels.as_bytes());
                    pl.extend_from_slice(&size.to_le_bytes());
                    pl.extend_from_slice(&mtime.to_le_bytes());
                    write_frame(stream, FrameType::FileStart, &pl)?;
                    let mut f = File::open(&path)?;
                    let mut buf = vec![0u8; 1024 * 1024];
                    loop {
                        let n = f.read(&mut buf)?;
                        if n == 0 { break; }
                        write_frame(stream, FrameType::FileData, &buf[..n])?;
                    }
                    write_frame(stream, FrameType::FileEnd, &[])?;

                    // Send SetAttr for large file
                    let md = std::fs::metadata(&path)?;
                    let mut pla = Vec::with_capacity(2 + rels.len() + 1 + 4);
                    pla.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                    pla.extend_from_slice(rels.as_bytes());
                    pla.push(compute_attr_flags(&path));
                    #[cfg(unix)] {
                        use std::os::unix::fs::PermissionsExt;
                        let mode = md.permissions().mode();
                        pla.extend_from_slice(&mode.to_le_bytes());
                    }
                    write_frame(stream, FrameType::SetAttr, &pla)?;
                }

                write_frame(stream, FrameType::Done, &[])?;
                // Wait for client OK and return
                let (tt, _pl) = read_frame(stream)?;
                if tt != FrameType::Ok as u8 {
                    anyhow::bail!("client did not ack DONE");
                }
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
        let (t, pl) = if let Some((t1, pl1)) = pending.take() {
            (t1, pl1)
        } else {
            read_frame(stream)?
        };
        match t {
            x if x == FrameType::Symlink as u8 => {
                if pl.len() < 2 {
                    anyhow::bail!("bad SYMLINK");
                }
                let nlen = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                if pl.len() < 2 + nlen + 2 {
                    anyhow::bail!("bad SYMLINK payload");
                }
                let rel = std::str::from_utf8(&pl[2..2 + nlen]).context("utf8 sym path")?;
                let off = 2 + nlen;
                let tlen = u16::from_le_bytes([pl[off], pl[off + 1]]) as usize;
                if pl.len() < off + 2 + tlen {
                    anyhow::bail!("bad SYMLINK target len");
                }
                let target =
                    std::str::from_utf8(&pl[off + 2..off + 2 + tlen]).context("utf8 sym target")?;
                let relp = PathBuf::from(rel);
                let dst_path = normalize_under_root(&base, &relp)?;
                if let Some(parent) = dst_path.parent() {
                    fs::create_dir_all(parent).ok();
                }
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
            x if x == FrameType::SetAttr as u8 => {
                if pl.len() < 2 + 1 {
                    anyhow::bail!("bad SET_ATTR");
                }
                let nlen = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                if pl.len() < 2 + nlen + 1 {
                    anyhow::bail!("bad SET_ATTR len");
                }
                let rel = std::str::from_utf8(&pl[2..2 + nlen]).context("utf8 attr path")?;
                let attr = pl[2 + nlen];
                let relp = PathBuf::from(rel);
                let dst_path = normalize_under_root(&base, &relp)?;
                apply_windows_attrs(&dst_path, attr);
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
                preallocate_file_linux(&f, size);
                preallocate_file_linux(&f, size);
                cur_file = Some((dst_path, f, size, 0));
                // Store desired mtime in a side map keyed by absolute path
                // We'll apply it on FileEnd
                MTIME_STORE.with(|mt| {
                    let mut m = mt.borrow_mut();
                    if let Some((ref p_abs, _, _, _)) = cur_file {
                        m.insert(p_abs.to_string_lossy().to_string(), mtime);
                    }
                });
                if let Some((p, _, _, _)) = &cur_file {
                    received_paths.insert(p.clone());
                    expected_paths.insert(p.clone());
                }
                write_frame(stream, FrameType::Ok, b"FILE_START OK")?;
            }
            x if x == FrameType::TarStart as u8 => {
                // Receive a tar stream and unpack under base
                let (_files, _bytes) = handle_tar_stream(stream, &base, &mut received_paths)?;
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
            x if x == FrameType::FileRawStart as u8 => {
                if pl.len() < 2 + 8 + 8 {
                    anyhow::bail!("bad FILE_RAW_START");
                }
                let nlen = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                if pl.len() < 2 + nlen + 8 + 8 {
                    anyhow::bail!("bad FILE_RAW_START len");
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
                let mut f = File::create(&dst_path)
                    .with_context(|| format!("create {}", dst_path.display()))?;
                f.set_len(size).ok();
                preallocate_file_linux(&f, size);
                preallocate_file_linux(&f, size);
                recv_raw_to_file(stream, &mut f, size)?;
                MTIME_STORE.with(|mt| {
                    mt.borrow_mut()
                        .insert(dst_path.to_string_lossy().to_string(), mtime);
                });
                apply_preserved_mtime(&dst_path)?;
                received_paths.insert(dst_path.clone());
                expected_paths.insert(dst_path);
            }
            x if x == FrameType::DeltaStart as u8 => {
                // payload: nlen u16 | rel bytes | size u64 | mtime i64 | granule u32 | sample u32
                if pl.len() < 2 + 8 + 8 + 4 + 4 {
                    anyhow::bail!("bad DELTA_START");
                }
                let nlen = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                if pl.len() < 2 + nlen + 8 + 8 + 4 + 4 {
                    anyhow::bail!("bad DELTA_START len");
                }
                let rel = std::str::from_utf8(&pl[2..2 + nlen]).context("utf8 path")?;
                let mut off = 2 + nlen;
                let size = u64::from_le_bytes(pl[off..off + 8].try_into().unwrap());
                off += 8;
                let mtime = i64::from_le_bytes(pl[off..off + 8].try_into().unwrap());
                off += 8;
                let granule = u32::from_le_bytes(pl[off..off + 4].try_into().unwrap()) as u64;
                off += 4;
                let sample = u32::from_le_bytes(pl[off..off + 4].try_into().unwrap()) as usize;
                let relp = PathBuf::from(rel);
                let dst_path = normalize_under_root(&base, &relp)?;
                if let Some(parent) = dst_path.parent() {
                    fs::create_dir_all(parent).ok();
                }
                // Ensure file exists and size; we'll write sparse ranges
                let mut f = File::options()
                    .create(true)
                    .read(true)
                    .write(true)
                    .open(&dst_path)
                    .with_context(|| format!("open {}", dst_path.display()))?;
                f.set_len(size).ok();
                preallocate_file_linux(&f, size);
                drop(f);
                delta_state = Some(DeltaState {
                    dst_path,
                    file_size: size,
                    mtime,
                    granule,
                    sample,
                    need_ranges: Vec::new(),
                });
            }
            x if x == FrameType::DeltaSample as u8 => {
                // payload: offset u64 | hash_len u16 | hash bytes
                if pl.len() < 8 + 2 {
                    anyhow::bail!("bad DELTA_SAMPLE");
                }
                let off = u64::from_le_bytes(pl[0..8].try_into().unwrap());
                let hlen = u16::from_le_bytes(pl[8..10].try_into().unwrap()) as usize;
                if pl.len() < 10 + hlen {
                    anyhow::bail!("bad DELTA_SAMPLE hash len");
                }
                let hashc = &pl[10..10 + hlen];
                if let Some(ds) = delta_state.as_mut() {
                    // Read sample from existing file and compare
                    if let Ok(mut f) = File::open(&ds.dst_path) {
                        use std::io::{Read, Seek, SeekFrom};
                        let mut buf = vec![0u8; ds.sample];
                        let _ = f.seek(SeekFrom::Start(off));
                        let n = f.read(&mut buf).unwrap_or(0);
                        let n = std::cmp::min(n, ds.sample);
                        let h = blake3::hash(&buf[..n]);
                        if h.as_bytes() != hashc {
                            // Mark this granule as needed (coalesce later)
                            // We store exact granule range based on offset alignment
                            let start = (off / ds.granule) * ds.granule;
                            let end = (start + ds.granule).min(ds.file_size);
                            ds.need_ranges.push((start, end - start));
                        }
                    } else {
                        // If we cannot open, request all
                        if let Some(ds2) = delta_state.as_mut() {
                            ds2.need_ranges.clear();
                            ds2.need_ranges.push((0, ds2.file_size));
                        }
                    }
                }
            }
            x if x == FrameType::DeltaEnd as u8 => {
                // Coalesce overlapping ranges and send NeedRanges list
                if let Some(ds) = delta_state.as_mut() {
                    let mut v = std::mem::take(&mut ds.need_ranges);
                    v.sort_by_key(|r| r.0);
                    let mut coalesced: Vec<(u64, u64)> = Vec::new();
                    for (mut s_off, mut s_len) in v.into_iter() {
                        if let Some(last) = coalesced.last_mut() {
                            let last_end = last.0 + last.1;
                            if s_off <= last_end {
                                // overlap/adjacent
                                let new_end = (s_off + s_len).max(last_end);
                                last.1 = new_end - last.0;
                                continue;
                            }
                        }
                        coalesced.push((s_off, s_len));
                    }
                    // Send NeedRangesStart with count u32
                    write_frame(
                        stream,
                        FrameType::NeedRangesStart,
                        &(coalesced.len() as u32).to_le_bytes(),
                    )?;
                    for (off, len) in &coalesced {
                        let mut pl = Vec::with_capacity(16);
                        pl.extend_from_slice(&off.to_le_bytes());
                        pl.extend_from_slice(&len.to_le_bytes());
                        write_frame(stream, FrameType::NeedRange, &pl)?;
                    }
                    write_frame(stream, FrameType::NeedRangesEnd, &[])?;
                    // Store back coalesced for validation if needed
                    ds.need_ranges = coalesced;
                } else {
                    write_frame(stream, FrameType::NeedRangesStart, &0u32.to_le_bytes())?;
                    write_frame(stream, FrameType::NeedRangesEnd, &[])?;
                }
            }
            x if x == FrameType::DeltaData as u8 => {
                // payload: offset u64 | data bytes
                if pl.len() < 8 {
                    anyhow::bail!("bad DELTA_DATA");
                }
                let off = u64::from_le_bytes(pl[0..8].try_into().unwrap());
                if let Some(ds) = delta_state.as_ref() {
                    use std::io::{Seek, SeekFrom, Write};
                    let mut f = File::options().read(true).write(true).open(&ds.dst_path)?;
                    let _ = f.seek(SeekFrom::Start(off));
                    let data = &pl[8..];
                    if off.saturating_add(data.len() as u64) > ds.file_size {
                        anyhow::bail!("DELTA_DATA out of range");
                    }
                    f.write_all(data)?;
                }
            }
            x if x == FrameType::DeltaDone as u8 => {
                if let Some(ds) = delta_state.take() {
                    // Apply mtime now that all ranges were written
                    MTIME_STORE.with(|mt| {
                        mt.borrow_mut()
                            .insert(ds.dst_path.to_string_lossy().to_string(), ds.mtime);
                    });
                    apply_preserved_mtime(&ds.dst_path)?;
                }
                write_frame(stream, FrameType::Ok, b"DELTA_OK")?;
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
                preallocate_file_linux(&f, size);
                preallocate_file_linux(&f, size);
                p_files.insert(stream_id, (dst_path, f, size, 0));
                MTIME_STORE.with(|mt| {
                    let mut m = mt.borrow_mut();
                    if let Some((p_abs, _, _, _)) = p_files.get(&stream_id) {
                        m.insert(p_abs.to_string_lossy().to_string(), mtime);
                    }
                });
                if let Some((p, _, _, _)) = p_files.get(&stream_id) {
                    received_paths.insert(p.clone());
                    expected_paths.insert(p.clone());
                }
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
                    let use_set = if !expected_paths.is_empty() {
                        &expected_paths
                    } else {
                        &received_paths
                    };
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
    for entry in walkdir::WalkDir::new(base)
        .into_iter()
        .filter_map(|e| e.ok())
    {
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
        if d == *base {
            continue;
        }
        // Preserve directories that are expected for this session (mirror should keep them)
        if received.contains(&d) {
            continue;
        }
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
    eprintln!(
        "mirror delete: removed {} files, {} dirs",
        files_deleted, dirs_deleted
    );
    Ok((files_deleted, dirs_deleted))
}

pub fn client_start(
    host: &str,
    port: u16,
    dest: &Path,
    src_root: &Path,
    args: &crate::Args,
) -> Result<()> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let mut last_update = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let mut files_sent = 0;
    // Progress update will be added in transfer logic

    let addr = format!("{}:{}", host, port);
    print!("Connecting {}... ", addr);
    let _ = stdout().flush();
    let stream = TcpStream::connect(&addr).with_context(|| format!("connect {}", addr))?;
    tune_socket(&stream);
    let stream = Arc::new(Mutex::new(stream));

    // START payload: dest_len u16 | dest_bytes | flags u8 (bit0 mirror, bit2 include_empty_dirs)
    let dest_s = dest.to_string_lossy();
    let mut payload = Vec::with_capacity(2 + dest_s.len() + 1);
    payload.extend_from_slice(&(dest_s.len() as u16).to_le_bytes());
    payload.extend_from_slice(dest_s.as_bytes());
    // Compute include-empty-dirs semantics: --mir implies include empties
    let include_empty = if args.mirror || args.delete {
        true
    } else if args.subdirs || args.no_empty_dirs {
        false
    } else if args.empty_dirs {
        true
    } else {
        true
    };
    let mut flags: u8 = if args.mirror || args.delete {
        0b0000_0001
    } else {
        0
    };
    if include_empty {
        flags |= 0b0000_0100;
    }
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
                let target_is_dir = std::fs::metadata(&path)
                    .map(|m| m.is_dir())
                    .unwrap_or(false);
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
    // Track last observed byte count for rate computation
    let mut last_bytes = *sent_bytes.lock().unwrap();

    // Manifest handshake: send inventory (files + symlinks + directories), receive need list
    // Build and send manifest
    {
        let mut s = stream.lock().unwrap();
        write_frame(&mut s, FrameType::ManifestStart, &[])?;
        use std::time::UNIX_EPOCH;
        // Files
        for fe in small_files
            .iter()
            .chain(medium_files.iter())
            .chain(large_files.iter())
        {
            let rel = fe.path.strip_prefix(src_root).unwrap_or(&fe.path);
            let rels = rel.to_string_lossy();
            let md = std::fs::metadata(&fe.path)?;
            let mtime = md
                .modified()?
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
            for ent in walkdir::WalkDir::new(src_root)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                if ent.file_type().is_dir() {
                    let rel = ent.path().strip_prefix(src_root).unwrap_or(ent.path());
                    if rel.as_os_str().is_empty() {
                        continue;
                    } // skip root itself
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
        if tneed != FrameType::NeedList as u8 {
            anyhow::bail!("server did not reply NeedList");
        }
        let mut need = std::collections::HashSet::new();
        if plneed.len() >= 4 {
            let mut off = 0usize;
            let _cnt = u32::from_le_bytes(plneed[off..off + 4].try_into().unwrap()) as usize;
            off += 4;
            while off + 2 <= plneed.len() {
                let nlen = u16::from_le_bytes(plneed[off..off + 2].try_into().unwrap()) as usize;
                off += 2;
                if off + nlen > plneed.len() {
                    break;
                }
                let s = std::str::from_utf8(&plneed[off..off + nlen])
                    .unwrap_or("")
                    .to_string();
                off += nlen;
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
                    buf: Vec::with_capacity(4 * 1024 * 1024),
                }
            }
            fn flush(&mut self) -> Result<()> {
                if !self.buf.is_empty() {
                    let frame = build_frame(FrameType::TarData, &self.buf);
                    self.stream.write_all(&frame)?;
                    self.buf.clear();
                }
                Ok(())
            }
        }
        impl<'a> std::io::Write for FrameWriter<'a> {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                let mut rem = b;
                while !rem.is_empty() {
                    let space = 4 * 1024 * 1024 - self.buf.len();
                    let take = space.min(rem.len());
                    self.buf.extend_from_slice(&rem[..take]);
                    rem = &rem[take..];
                    if self.buf.len() == 4 * 1024 * 1024 {
                        let frame = build_frame(FrameType::TarData, &self.buf);
                        self.stream
                            .write_all(&frame)
                            .map_err(|e| std::io::Error::other(e))?;
                        self.buf.clear();
                    }
                }
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { FrameWriter::flush(self).map_err(|e| std::io::Error::other(e)) }
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
        // Send attributes for small files (Windows + POSIX mode on Unix)
        for fe in &small_files {
            let rel = fe.path.strip_prefix(src_root).unwrap_or(&fe.path);
            let rels = rel.to_string_lossy();
            let mut pl = Vec::with_capacity(2 + rels.len() + 1 + 4);
            pl.extend_from_slice(&(rels.len() as u16).to_le_bytes());
            pl.extend_from_slice(rels.as_bytes());
            pl.push(compute_attr_flags(&fe.path));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let md = std::fs::metadata(&fe.path)?;
                let mode = md.permissions().mode();
                pl.extend_from_slice(&mode.to_le_bytes());
            }
            write_frame(&mut s, FrameType::SetAttr, &pl)?;
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
        files_sent += 1;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        if now - last_update >= 2 {
            // Update every 2 seconds
            eprintln!("Progress: {} files sent...", files_sent);
            last_update = now;
        }
        if args.progress {
            println!("{} → (symlink) {}", spath.display(), rels);
        }
    }

    // Multi-connection data plane: push medium+large files via N dedicated sockets
    let mut all_work = Vec::new();
    all_work.extend(
        medium_files
            .into_iter()
            .map(|fe| (fe, 4 * 1024 * 1024usize)),
    );
    all_work.extend(large_files.into_iter().map(|fe| (fe, 4 * 1024 * 1024usize)));
    let work = Arc::new(Mutex::new(all_work));
    let cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);
    let mut workers = cpus.saturating_mul(2);
    if workers < 4 {
        workers = 4;
    }
    if workers > 16 {
        workers = 16;
    }
    let mut handles = vec![];
    for _ in 0..workers {
        let src_root = src_root.to_path_buf();
        let dest = dest.to_path_buf();
        let sent_files = Arc::clone(&sent_files);
        let sent_bytes = Arc::clone(&sent_bytes);
        let progress = args.progress;
        let verify = !args.no_verify;
        let no_restart = args.no_restart;
        let addr = addr.clone();
        let work = Arc::clone(&work);
        let include_empty = include_empty;
        let handle = thread::spawn(move || -> Result<()> {
            let mut s = TcpStream::connect(&addr).with_context(|| format!("connect {}", addr))?;
            tune_socket(&s);
            let dest_s = dest.to_string_lossy();
            let mut payload = Vec::with_capacity(2 + dest_s.len() + 1);
            payload.extend_from_slice(&(dest_s.len() as u16).to_le_bytes());
            payload.extend_from_slice(dest_s.as_bytes());
            let mut flags: u8 = 0;
            if include_empty {
                flags |= 0b0000_0100;
            }
            payload.push(flags);
            write_frame(&mut s, FrameType::Start, &payload)?;
            let (typ, resp) = read_frame(&mut s)?;
            if typ != FrameType::Ok as u8 {
                anyhow::bail!("daemon error: {}", String::from_utf8_lossy(&resp));
            }

            loop {
                let job_opt = {
                    let mut q = work.lock().unwrap();
                    q.pop()
                };
                let (fe, chunk) = match job_opt {
                    Some(x) => x,
                    None => break,
                };

                let rel = fe.path.strip_prefix(&src_root).unwrap_or(&fe.path);
                let dest_rel = rel.to_string_lossy();

                if !no_restart && fe.size >= 104_857_600 {
                    // Delta-sampling pass: 8MiB granules, 64KiB samples
                    let granule: u32 = 8 * 1024 * 1024;
                    let sample: u32 = 64 * 1024;
                    let rel2 = fe.path.strip_prefix(&src_root).unwrap_or(&fe.path);
                    let rels2 = rel2.to_string_lossy();
                    let mut pl0 = Vec::with_capacity(2 + rels2.len() + 8 + 8 + 4 + 4);
                    pl0.extend_from_slice(&(rels2.len() as u16).to_le_bytes());
                    pl0.extend_from_slice(rels2.as_bytes());
                    pl0.extend_from_slice(&fe.size.to_le_bytes());
                    let md2 = std::fs::metadata(&fe.path)?;
                    let mtime2 = md2
                        .modified()?
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64;
                    pl0.extend_from_slice(&mtime2.to_le_bytes());
                    pl0.extend_from_slice(&granule.to_le_bytes());
                    pl0.extend_from_slice(&sample.to_le_bytes());
                    write_frame(&mut s, FrameType::DeltaStart, &pl0)?;
                    // Send samples
                    let mut f0 = File::open(&fe.path)?;
                    let granule64 = granule as u64;
                    let mut buf0 = vec![0u8; sample as usize];
                    let mut off0: u64 = 0;
                    use std::io::{Seek, SeekFrom};
                    while off0 < fe.size {
                        f0.seek(SeekFrom::Start(off0))?;
                        let n0 = f0.read(&mut buf0)?;
                        let h = blake3::hash(&buf0[..n0]);
                        let mut pls = Vec::with_capacity(8 + 2 + h.as_bytes().len());
                        pls.extend_from_slice(&off0.to_le_bytes());
                        pls.extend_from_slice(&(h.as_bytes().len() as u16).to_le_bytes());
                        pls.extend_from_slice(h.as_bytes());
                        write_frame(&mut s, FrameType::DeltaSample, &pls)?;
                        let mid = off0.saturating_add((granule64 / 2).min(fe.size - off0));
                        f0.seek(SeekFrom::Start(mid))?;
                        let n1 = f0.read(&mut buf0)?;
                        let h1 = blake3::hash(&buf0[..n1]);
                        let mut pls1 = Vec::with_capacity(8 + 2 + h1.as_bytes().len());
                        pls1.extend_from_slice(&mid.to_le_bytes());
                        pls1.extend_from_slice(&(h1.as_bytes().len() as u16).to_le_bytes());
                        pls1.extend_from_slice(h1.as_bytes());
                        write_frame(&mut s, FrameType::DeltaSample, &pls1)?;
                        // end-of-granule sample within same granule
                        let end_off = if fe.size > off0 {
                            (off0 + granule64)
                                .min(fe.size)
                                .saturating_sub(sample as u64)
                        } else {
                            off0
                        };
                        f0.seek(SeekFrom::Start(end_off))?;
                        let n2 = f0.read(&mut buf0)?;
                        let h2 = blake3::hash(&buf0[..n2]);
                        let mut pls2 = Vec::with_capacity(8 + 2 + h2.as_bytes().len());
                        pls2.extend_from_slice(&end_off.to_le_bytes());
                        pls2.extend_from_slice(&(h2.as_bytes().len() as u16).to_le_bytes());
                        pls2.extend_from_slice(h2.as_bytes());
                        write_frame(&mut s, FrameType::DeltaSample, &pls2)?;
                        off0 = off0.saturating_add(granule64);
                    }
                    write_frame(&mut s, FrameType::DeltaEnd, &[])?;
                    // Read need ranges
                    let (t_need, pl_need) = read_frame(&mut s)?;
                    let mut need_ranges: Vec<(u64, u64)> = Vec::new();
                    if t_need == FrameType::NeedRangesStart as u8 {
                        let mut count_bytes = [0u8; 4];
                        count_bytes.copy_from_slice(&pl_need[..4]);
                        let _cnt = u32::from_le_bytes(count_bytes) as usize;
                        loop {
                            let (ti, pli) = read_frame(&mut s)?;
                            if ti == FrameType::NeedRange as u8 {
                                if pli.len() >= 16 {
                                    let off = u64::from_le_bytes(pli[0..8].try_into().unwrap());
                                    let len = u64::from_le_bytes(pli[8..16].try_into().unwrap());
                                    need_ranges.push((off, len));
                                }
                            } else if ti == FrameType::NeedRangesEnd as u8 {
                                break;
                            } else {
                                anyhow::bail!("unexpected frame in need list");
                            }
                        }
                    }
                    if !need_ranges.is_empty() && need_ranges.len() as u64 * granule64 < fe.size {
                        let total_need_bytes: u64 = need_ranges.iter().map(|(_, l)| *l).sum();
                        // Send only needed ranges
                        for (mut off, len) in need_ranges.clone() {
                            let mut f2 = File::open(&fe.path)?;
                            f2.seek(SeekFrom::Start(off))?;
                            let mut left = len;
                            let mut b = vec![0u8; 4 * 1024 * 1024];
                            while left > 0 {
                                let want = (left as usize).min(b.len());
                                let n = f2.read(&mut b[..want])?;
                                if n == 0 {
                                    break;
                                }
                                let mut p = Vec::with_capacity(8 + n);
                                p.extend_from_slice(&off.to_le_bytes());
                                p.extend_from_slice(&b[..n]);
                                write_frame(&mut s, FrameType::DeltaData, &p)?;
                                off += n as u64;
                                left -= n as u64;
                                let mut sb = sent_bytes.lock().unwrap();
                                *sb += n as u64;
                            }
                        }
                        write_frame(&mut s, FrameType::DeltaDone, &[])?;
                        let (_tok, _plk) = read_frame(&mut s)?;
                        if verify {
                            let rel = fe.path.strip_prefix(&src_root).unwrap_or(&fe.path);
                            let rels = rel.to_string_lossy();
                            let mut plv = Vec::with_capacity(2 + rels.len());
                            plv.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                            plv.extend_from_slice(rels.as_bytes());
                            write_frame(&mut s, FrameType::VerifyReq, &plv)?;
                            let (tv, hv) = read_frame(&mut s)?;
                            if tv != FrameType::VerifyHash as u8 {
                                anyhow::bail!("verify failed");
                            }
                            let local = hash_file_blake3(&fe.path)?;
                            if hv.len() != 32 || hv.as_slice() != local {
                                anyhow::bail!("hash mismatch");
                            }
                        }
                        let mut sf = sent_files.lock().unwrap();
                        *sf += 1;
                        if progress {
                            let saved = 100.0 - (total_need_bytes as f64 / fe.size as f64 * 100.0);
                            println!(
                                "{} → {} (delta, saved {:.1}%)",
                                fe.path.display(),
                                dest_rel,
                                saved
                            );
                        }
                        continue;
                    }

                    // Large file: zero-copy friendly path
                    let mut pl = Vec::with_capacity(2 + dest_rel.len() + 8 + 8);
                    pl.extend_from_slice(&(dest_rel.len() as u16).to_le_bytes());
                    pl.extend_from_slice(dest_rel.as_bytes());
                    pl.extend_from_slice(&fe.size.to_le_bytes());
                    let md = std::fs::metadata(&fe.path)?;
                    let mtime = md
                        .modified()?
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64;
                    pl.extend_from_slice(&mtime.to_le_bytes());
                    write_frame(&mut s, FrameType::FileRawStart, &pl)?;
                    let file = File::open(&fe.path)?;
                    sendfile_to_stream(&file, &mut s, fe.size)?;
                    // Send POSIX mode via SetAttr (ignored on Windows)
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let mut pla = Vec::with_capacity(2 + dest_rel.len() + 1 + 4);
                        pla.extend_from_slice(&(dest_rel.len() as u16).to_le_bytes());
                        pla.extend_from_slice(dest_rel.as_bytes());
                        pla.push(0u8);
                        let mdm = std::fs::metadata(&fe.path)?;
                        let mode = mdm.permissions().mode();
                        pla.extend_from_slice(&mode.to_le_bytes());
                        write_frame(&mut s, FrameType::SetAttr, &pla)?;
                    }
                    let mut sb = sent_bytes.lock().unwrap();
                    *sb += fe.size;
                    drop(sb);
                    if progress {
                        println!("{} → {} ({} bytes)", fe.path.display(), dest_rel, fe.size);
                    }
                } else {
                    let mut pl = Vec::with_capacity(1 + 2 + dest_rel.len() + 8 + 8);
                    pl.push(0); // stream id per connection
                    pl.extend_from_slice(&(dest_rel.len() as u16).to_le_bytes());
                    pl.extend_from_slice(dest_rel.as_bytes());
                    pl.extend_from_slice(&fe.size.to_le_bytes());
                    let md = std::fs::metadata(&fe.path)?;
                    let mtime = md
                        .modified()?
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64;
                    pl.extend_from_slice(&mtime.to_le_bytes());
                    write_frame(&mut s, FrameType::PFileStart, &pl)?;

                    let mut f = File::open(&fe.path)?;
                    let mut buf = vec![0u8; chunk];
                    loop {
                        let n = f.read(&mut buf)?;
                        if n == 0 {
                            break;
                        }
                        let mut pl = Vec::with_capacity(1 + n);
                        pl.push(0);
                        pl.extend_from_slice(&buf[..n]);
                        write_frame(&mut s, FrameType::PFileData, &pl)?;
                        let mut sb = sent_bytes.lock().unwrap();
                        *sb += n as u64;
                    }

                    write_frame(&mut s, FrameType::PFileEnd, &[0u8])?;
                    // Send POSIX mode via SetAttr (ignored on Windows)
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        let mut pla = Vec::with_capacity(2 + dest_rel.len() + 1 + 4);
                        pla.extend_from_slice(&(dest_rel.len() as u16).to_le_bytes());
                        pla.extend_from_slice(dest_rel.as_bytes());
                        pla.push(0u8);
                        let mdm = std::fs::metadata(&fe.path)?;
                        let mode = mdm.permissions().mode();
                        pla.extend_from_slice(&mode.to_le_bytes());
                        write_frame(&mut s, FrameType::SetAttr, &pla)?;
                    }
                    if progress {
                        println!("{} → {} ({} bytes)", fe.path.display(), dest_rel, fe.size);
                    }
                }

                if verify {
                    let rel = fe.path.strip_prefix(&src_root).unwrap_or(&fe.path);
                    let rels = rel.to_string_lossy();
                    let mut plv = Vec::with_capacity(2 + rels.len());
                    plv.extend_from_slice(&(rels.len() as u16).to_le_bytes());
                    plv.extend_from_slice(rels.as_bytes());
                    write_frame(&mut s, FrameType::VerifyReq, &plv)?;
                    let (tv, hv) = read_frame(&mut s)?;
                    if tv != FrameType::VerifyHash as u8 {
                        anyhow::bail!("verify failed to get hash");
                    }
                    let local = hash_file_blake3(&fe.path)?;
                    if hv.len() != 32 || hv.as_slice() != local {
                        anyhow::bail!("hash mismatch for ");
                    }
                }
                let mut sf = sent_files.lock().unwrap();
                *sf += 1;
                if progress {
                    println!("{} → {} ({} bytes)", fe.path.display(), dest_rel, fe.size);
                }
            }

            write_frame(&mut s, FrameType::Done, b"")?;
            let (_t_ok, _pl) = read_frame(&mut s)?;
            Ok(())
        });
        handles.push(handle);
    }
    for h in handles {
        h.join().unwrap()?;
    }

    // (large files handled via data pool above)
    let mut buf = vec![0u8; 1024 * 1024];
    let mut last_bytes = *sent_bytes.lock().unwrap();
    for fe in &[] as &[robosync::fs_enum::FileEntry] {
        let rel = fe.path.strip_prefix(src_root).unwrap_or(&fe.path);
        let dest_rel = rel.to_string_lossy();
        let mut pl = Vec::with_capacity(2 + dest_rel.len() + 8 + 8);
        pl.extend_from_slice(&(dest_rel.len() as u16).to_le_bytes());
        pl.extend_from_slice(dest_rel.as_bytes());
        pl.extend_from_slice(&fe.size.to_le_bytes());
        let md = std::fs::metadata(&fe.path)?;
        let mtime = md
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
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

pub fn client_pull(
    host: &str,
    port: u16,
    src: &Path,
    dest_root: &Path,
    args: &crate::Args,
) -> Result<()> {
    let addr = format!("{}:{}", host, port);
    print!("Connecting {}... ", addr);
    let _ = stdout().flush();
    let stream = TcpStream::connect(&addr).with_context(|| format!("connect {}", addr))?;
    tune_socket(&stream);
    let stream = Arc::new(Mutex::new(stream));

    // START payload: path on server (src) + flags (mirror + pull + include_empty_dirs)
    let src_s = src.to_string_lossy();
    let mut payload = Vec::with_capacity(2 + src_s.len() + 1);
    payload.extend_from_slice(&(src_s.len() as u16).to_le_bytes());
    payload.extend_from_slice(src_s.as_bytes());
    let mut flags: u8 = 0;
    if args.mirror || args.delete {
        flags |= 0b0000_0001;
    }
    flags |= 0b0000_0010; // pull
    let include_empty = if args.mirror || args.delete {
        true
    } else if args.subdirs || args.no_empty_dirs {
        false
    } else if args.empty_dirs {
        true
    } else {
        true
    };
    if include_empty {
        flags |= 0b0000_0100;
    }
    payload.push(flags);
    {
        let mut s = stream.lock().unwrap();
        write_frame(&mut s, FrameType::Start, &payload)?;
        let (typ, resp) = read_frame(&mut s)?;
        if typ != FrameType::Ok as u8 {
            anyhow::bail!("daemon error: {}", String::from_utf8_lossy(&resp));
        }
        // Send manifest of local destination to allow delta
        write_frame(&mut s, FrameType::ManifestStart, &[])?;
        use std::time::UNIX_EPOCH;
        let filter = crate::fs_enum::FileFilter {
            exclude_files: args.exclude_files.clone(),
            exclude_dirs: args.exclude_dirs.clone(),
            min_size: None,
            max_size: None,
            include_empty_dirs: true,
        };
        let entries = crate::fs_enum::enumerate_directory_filtered(dest_root, &filter)?;
        for fe in entries.into_iter().filter(|e| !e.is_directory) {
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
            write_frame(&mut s, FrameType::ManifestEntry, &pl)?;
        }
        for ent in walkdir::WalkDir::new(dest_root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
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
    let spinner = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    let mut spin_idx = 0usize;
    let mut last_tick = std::time::Instant::now();
    let tick = std::time::Duration::from_millis(250);
    let mut files_recv: u64 = 0;
    let mut bytes_recv: u64 = 0;
    let mut transferred_any = false;

    use std::collections::HashSet as HSet;
    let mut expected: HSet<PathBuf> = HSet::new();
    loop {
        let (t, pl) = {
            let mut s = stream.lock().unwrap();
            read_frame(&mut s)?
        };
        match t {
            x if x == FrameType::TarStart as u8 => {
                let mut s = stream.lock().unwrap();
                let (fc, fb) = handle_tar_stream(&mut s, dest_root, &mut expected)?;
                files_recv = files_recv.saturating_add(fc);
                bytes_recv = bytes_recv.saturating_add(fb);
            }
            x if x == FrameType::SetAttr as u8 => {
                if pl.len() < 2 + 1 {
                    anyhow::bail!("bad SET_ATTR");
                }
                let nlen = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                if pl.len() < 2 + nlen + 1 {
                    anyhow::bail!("bad SET_ATTR len");
                }
                let rel = std::str::from_utf8(&pl[2..2 + nlen]).context("utf8 attr path")?;
                let attr = pl[2 + nlen];
                let relp = PathBuf::from(rel);
                let dst = dest_root.join(relp);
                apply_windows_attrs(&dst, attr);
            }
            x if x == FrameType::MkDir as u8 => {
                if pl.len() < 2 {
                    anyhow::bail!("bad MKDIR");
                }
                let nlen = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                if pl.len() < 2 + nlen {
                    anyhow::bail!("bad MKDIR payload");
                }
                let rel = std::str::from_utf8(&pl[2..2 + nlen]).context("utf8 dir path")?;
                let dir_path = dest_root.join(rel);
                std::fs::create_dir_all(&dir_path).ok();
                expected.insert(dir_path);
            }
            x if x == FrameType::Symlink as u8 => {
                if pl.len() < 2 {
                    anyhow::bail!("bad SYMLINK");
                }
                let nlen = u16::from_le_bytes([pl[0], pl[1]]) as usize;
                if pl.len() < 2 + nlen + 2 {
                    anyhow::bail!("bad SYMLINK payload");
                }
                let rel = std::str::from_utf8(&pl[2..2 + nlen]).context("utf8 sym path")?;
                let off = 2 + nlen;
                let tlen = u16::from_le_bytes([pl[off], pl[off + 1]]) as usize;
                if pl.len() < off + 2 + tlen {
                    anyhow::bail!("bad SYMLINK target len");
                }
                let target =
                    std::str::from_utf8(&pl[off + 2..off + 2 + tlen]).context("utf8 sym target")?;
                let dst_path = dest_root.join(rel);
                if let Some(parent) = dst_path.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
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
                let dst_path = dest_root.join(rel);
                if let Some(parent) = dst_path.parent() {
                    std::fs::create_dir_all(parent).ok();
                }
                let mut f = File::create(&dst_path)
                    .with_context(|| format!("create {}", dst_path.display()))?;
                f.set_len(size).ok();
                preallocate_file_linux(&f, size);
                preallocate_file_linux(&f, size);
                MTIME_STORE.with(|mt| {
                    let mut m = mt.borrow_mut();
                    m.insert(dst_path.to_string_lossy().to_string(), mtime);
                });
                let mut written = 0u64;
                loop {
                    let (t2, pl2) = {
                        let mut s = stream.lock().unwrap();
                        read_frame(&mut s)?
                    };
                    if t2 == FrameType::FileData as u8 {
                        f.write_all(&pl2)?;
                        written += pl2.len() as u64;
                    } else if t2 == FrameType::FileEnd as u8 {
                        break;
                    } else {
                        anyhow::bail!("unexpected frame during file data: {}", t2);
                    }
                    bytes_recv += pl2.len() as u64;
                    if !args.progress && last_tick.elapsed() >= tick {
                        print!(
                            "\r{} received {} files, {:.2} MB",
                            spinner[spin_idx],
                            files_recv,
                            bytes_recv as f64 / 1_048_576.0
                        );
                        let _ = stdout().flush();
                        spin_idx = (spin_idx + 1) % spinner.len();
                        last_tick = std::time::Instant::now();
                    }
                }
                if written != size {
                    eprintln!(
                        "short download: {} {}/{}",
                        dst_path.display(),
                        written,
                        size
                    );
                }
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
        for e in walkdir::WalkDir::new(dest_root)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let p = e.path().to_path_buf();
            if e.file_type().is_dir() {
                all_dirs.push(p);
                continue;
            }
            if e.file_type().is_file() || e.file_type().is_symlink() {
                if !expected.contains(&p) {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
        all_dirs.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
        for d in all_dirs {
            if d == *dest_root {
                continue;
            }
            if expected.contains(&d) {
                continue;
            }
            let _ = std::fs::remove_dir(&d);
        }
    }
    if !args.progress {
        print!("\r\x1b[K");
        let _ = stdout().flush();
        if transferred_any {
            println!(
                "✓ received {} files, {:.2} MB",
                files_recv,
                bytes_recv as f64 / 1_048_576.0
            );
        } else {
            println!("✓ up to date");
        }
    }
    Ok(())
}
