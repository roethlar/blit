use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::Result;
use std::sync::Arc;
// no tokio::io imports needed here; StreamAny defines read_exact
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;

use blit::protocol;
use blit::protocol_core;

use super::app::{Focus, UiMsg};
use super::ui::Entry;

// Central runtime for remote operations
lazy_static::lazy_static! {
    static ref RUNTIME: tokio::runtime::Runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .expect("Failed to create remote ops runtime");
}

/// Public entry: request remote directory listing (non-blocking).
/// Sends UiMsg::RemoteEntries or UiMsg::Error on the provided channel.
pub fn request_remote_dir(
    tx_ui: &Sender<UiMsg>,
    pane: Focus,
    host: String,
    port: u16,
    path: PathBuf,
) {
    let tx = tx_ui.clone();
    RUNTIME.spawn(async move {
        match read_remote_dir_async(&host, port, &path).await {
            Ok(entries) => {
                let _ = tx.send(UiMsg::RemoteEntries { pane, entries });
            }
            Err(e) => {
                let _ = tx.send(UiMsg::Error(format!(
                    "Failed to connect to {}:{}: {}",
                    host, port, e
                )));
            }
        }
    });
}

/// Create a directory on the remote server under the given base path.
pub fn request_remote_mkdir(
    tx_ui: &Sender<UiMsg>,
    host: String,
    port: u16,
    base: PathBuf,
    name: String,
) {
    let tx = tx_ui.clone();
    RUNTIME.spawn(async move {
        let res = mkdir_remote_async(&host, port, &base, &name).await;
        if let Err(e) = res {
            let _ = tx.send(UiMsg::Error(format!(
                "Failed to create folder on {}:{}: {}",
                host, port, e
            )));
        }
    });
}

async fn mkdir_remote_async(host: &str, port: u16, base: &Path, name: &str) -> Result<()> {
    use tokio::time::{timeout, Duration};
    let addr = format!("{}:{}", host, port);
    // Connect TCP
    let tcp = timeout(Duration::from_millis(5000), TcpStream::connect(&addr))
        .await
        .map_err(|_| anyhow::anyhow!("Connection timeout"))??;
    // Try TLS first; fall back to plaintext if handshake fails
    let mut stream_any: StreamAny = {
        let cfg = blit::tls::build_client_config_tofu(host, port);
        let cx = TlsConnector::from(Arc::new(cfg));
        let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
            .map_err(|_| anyhow::anyhow!("Invalid server name for TLS: {}", host))?;
        match timeout(Duration::from_millis(5000), cx.connect(server_name, tcp)).await {
            Ok(Ok(tls)) => StreamAny::Tls(Box::new(tls)),
            _ => {
                // Plaintext fallback with a fresh socket
                let tcp2 = timeout(Duration::from_millis(1000), TcpStream::connect(&addr))
                    .await
                    .map_err(|_| anyhow::anyhow!("Connection timeout"))??;
                StreamAny::Plain(tcp2)
            }
        }
    };

    // START payload: base path on server
    let base_s = base.to_string_lossy();
    let mut payload = Vec::with_capacity(2 + base_s.len() + 1);
    payload.extend_from_slice(&(base_s.len() as u16).to_le_bytes());
    payload.extend_from_slice(base_s.as_bytes());
    payload.push(0);
    let hdr = protocol_core::build_frame_header(protocol::frame::START, payload.len() as u32);
    stream_any.write_all(&hdr).await?;
    stream_any.write_all(&payload).await?;
    // Read OK
    let mut resp_hdr = [0u8; 11];
    timeout(Duration::from_millis(5000), stream_any.read_exact(&mut resp_hdr))
        .await
        .map_err(|_| anyhow::anyhow!("Response timeout"))??;
    let (t_ok, len) = protocol_core::parse_frame_header(&resp_hdr)?;
    if t_ok != protocol::frame::OK {
        return Err(anyhow::anyhow!("Daemon did not ACK START"));
    }
    if len > 0 {
        let mut skip = vec![0u8; len as usize];
        let _ = stream_any.read_exact(&mut skip).await;
    }

    // Send MKDIR name
    let mut pl = Vec::with_capacity(2 + name.len());
    pl.extend_from_slice(&(name.len() as u16).to_le_bytes());
    pl.extend_from_slice(name.as_bytes());
    let hdr2 = protocol_core::build_frame_header(protocol::frame::MKDIR, pl.len() as u32);
    stream_any.write_all(&hdr2).await?;
    stream_any.write_all(&pl).await?;
    // Read OK
    let mut resp2 = [0u8; 11];
    timeout(Duration::from_millis(5000), stream_any.read_exact(&mut resp2))
        .await
        .map_err(|_| anyhow::anyhow!("MKDIR response timeout"))??;
    let (t2, len2) = protocol_core::parse_frame_header(&resp2)?;
    if t2 != protocol::frame::OK {
        return Err(anyhow::anyhow!("MKDIR failed"));
    }
    if len2 > 0 {
        let mut skip = vec![0u8; len2 as usize];
        let _ = stream_any.read_exact(&mut skip).await;
    }

    // Graceful DONE
    let hdr3 = protocol_core::build_frame_header(protocol::frame::DONE, 0);
    stream_any.write_all(&hdr3).await?;
    let mut resp3 = [0u8; 11];
    let _ = timeout(Duration::from_millis(5000), stream_any.read_exact(&mut resp3)).await;
    Ok(())
}
/// Async LIST request.
async fn read_remote_dir_async(host: &str, port: u16, path: &Path) -> Result<Vec<Entry>> {
    use tokio::time::{timeout, Duration};
    let addr = format!("{}:{}", host, port);
    // Establish connection (try TLS, then fallback to plaintext)
    // First, connect TCP
    let tcp = timeout(Duration::from_millis(5000), TcpStream::connect(&addr))
        .await
        .map_err(|_| anyhow::anyhow!("Connection timeout"))??;
    let mut stream_any: StreamAny = {
        let cfg = blit::tls::build_client_config_tofu(host, port);
        let cx = TlsConnector::from(Arc::new(cfg));
        let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
            .map_err(|_| anyhow::anyhow!("Invalid server name for TLS: {}", host))?;
        match timeout(Duration::from_millis(5000), cx.connect(server_name, tcp)).await {
            Ok(Ok(tls)) => StreamAny::Tls(Box::new(tls)),
            _ => {
                let tcp2 = timeout(Duration::from_millis(1000), TcpStream::connect(&addr))
                    .await
                    .map_err(|_| anyhow::anyhow!("Connection timeout"))??;
                StreamAny::Plain(tcp2)
            }
        }
    };

    // Build LIST_REQ payload
    let path_str = path.to_string_lossy();
    let path_bytes = path_str.as_bytes();
    let mut payload = Vec::with_capacity(2 + path_bytes.len());
    payload.extend_from_slice(&(path_bytes.len() as u16).to_le_bytes());
    payload.extend_from_slice(path_bytes);

    // Build and send frame header via centralized helper
    let hdr = protocol_core::build_frame_header(protocol::frame::LIST_REQ, payload.len() as u32);
    stream_any.write_all(&hdr).await?;
    stream_any.write_all(&payload).await?;

    // Read and validate response header
    let mut resp_hdr = [0u8; 11];
    timeout(
        Duration::from_millis(5000),
        stream_any.read_exact(&mut resp_hdr),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Response timeout"))?
    .map_err(|e| anyhow::anyhow!("Read error: {}", e))?;
    let (frame_type, payload_len_u32) = protocol_core::parse_frame_header(&resp_hdr)?;
    let payload_len = payload_len_u32 as usize;
    protocol_core::validate_frame_size(payload_len)?;

    if frame_type == protocol::frame::ERROR {
        // Read a short error message for diagnostics
        let mut err_payload = vec![0u8; payload_len.min(1024)];
        let _ = stream_any.read_exact(&mut err_payload).await;
        let err_msg = String::from_utf8_lossy(&err_payload);
        return Err(anyhow::anyhow!("Server error: {}", err_msg));
    }

    if frame_type != protocol::frame::LIST_RESP {
        return Err(anyhow::anyhow!("Unexpected frame type: {}", frame_type));
    }

    // Read LIST_RESP payload
    let mut payload = vec![0u8; payload_len];
    timeout(
        Duration::from_millis(5000),
        stream_any.read_exact(&mut payload),
    )
    .await
    .map_err(|_| anyhow::anyhow!("Payload read timeout"))??;

    // Parse entries: u32 count, then repeated (u8 kind, u16 name_len, name)
    let mut entries = Vec::new();
    entries.push(Entry {
        name: "..".to_string(),
        is_dir: true,
        is_symlink: false,
    });

    if payload.len() < 4 {
        return Ok(entries);
    }
    let count = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
    let mut offset = 4;
    for _ in 0..count {
        if offset + 3 > payload.len() {
            break;
        }
        let kind = payload[offset];
        offset += 1;
        let name_len = u16::from_le_bytes([payload[offset], payload[offset + 1]]) as usize;
        offset += 2;
        if offset + name_len > payload.len() {
            break;
        }
        let name = String::from_utf8_lossy(&payload[offset..offset + name_len]).to_string();
        offset += name_len;
        if kind == 2 {
            entries.push(Entry {
                name: "[More entries on server...]".to_string(),
                is_dir: false,
                is_symlink: false,
            });
            continue;
        }
        entries.push(Entry {
            name,
            is_dir: kind == 1,
            is_symlink: false,
        });
    }

    Ok(entries)
}

enum StreamAny {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
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
}
