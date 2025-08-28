
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::Result;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use blit::protocol;
use blit::protocol_core;

use super::app::{UiMsg, Focus};
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
pub fn request_remote_dir(tx_ui: &Sender<UiMsg>, pane: Focus, host: String, port: u16, path: PathBuf) {
    let tx = tx_ui.clone();
    RUNTIME.spawn(async move {
        match read_remote_dir_async(&host, port, &path).await {
            Ok(entries) => {
                let _ = tx.send(UiMsg::RemoteEntries { pane, entries });
            }
            Err(e) => {
                let _ = tx.send(UiMsg::Error(format!("Failed to connect to {}:{}: {}", host, port, e)));
            }
        }
    });
}
/// Async LIST request.
async fn read_remote_dir_async(host: &str, port: u16, path: &Path) -> Result<Vec<Entry>> {
    use tokio::time::{timeout, Duration};

    let addr = format!("{}:{}", host, port);
    let mut stream = timeout(Duration::from_millis(2000), TcpStream::connect(&addr)).await
        .map_err(|_| anyhow::anyhow!("Connection timeout"))?
        .map_err(|e| anyhow::anyhow!("Connection failed: {}", e))?;

    // Build LIST_REQ payload
    let path_str = path.to_string_lossy();
    let path_bytes = path_str.as_bytes();
    let mut payload = Vec::with_capacity(2 + path_bytes.len());
    payload.extend_from_slice(&(path_bytes.len() as u16).to_le_bytes());
    payload.extend_from_slice(path_bytes);

    // Build and send frame header via centralized helper
    let hdr = protocol_core::build_frame_header(protocol::frame::LIST_REQ, payload.len() as u32);
    stream.write_all(&hdr).await?;
    stream.write_all(&payload).await?;
    stream.flush().await?;

    // Read and validate response header
    let mut resp_hdr = [0u8; 11];
    timeout(Duration::from_millis(1000), stream.read_exact(&mut resp_hdr)).await
        .map_err(|_| anyhow::anyhow!("Response timeout"))?
        .map_err(|e| anyhow::anyhow!("Read error: {}", e))?;
    let (frame_type, payload_len_u32) = protocol_core::parse_frame_header(&resp_hdr)?;
    let payload_len = payload_len_u32 as usize;
    protocol_core::validate_frame_size(payload_len)?;

    if frame_type == protocol::frame::ERROR {
        // Read a short error message for diagnostics
        let mut err_payload = vec![0u8; payload_len.min(1024)];
        let _ = stream.read_exact(&mut err_payload).await;
        let err_msg = String::from_utf8_lossy(&err_payload);
        return Err(anyhow::anyhow!("Server error: {}", err_msg));
    }

    if frame_type != protocol::frame::LIST_RESP {
        return Err(anyhow::anyhow!("Unexpected frame type: {}", frame_type));
    }

    // Read LIST_RESP payload
    let mut payload = vec![0u8; payload_len];
    timeout(Duration::from_millis(1000), stream.read_exact(&mut payload)).await
        .map_err(|_| anyhow::anyhow!("Payload read timeout"))??;

    // Parse entries: u32 count, then repeated (u8 kind, u16 name_len, name)
    let mut entries = Vec::new();
    entries.push(Entry { name: "..".to_string(), is_dir: true, is_symlink: false });

    if payload.len() < 4 { return Ok(entries); }
    let count = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
    let mut offset = 4;
    for _ in 0..count {
        if offset + 3 > payload.len() { break; }
        let kind = payload[offset];
        offset += 1;
        let name_len = u16::from_le_bytes([payload[offset], payload[offset + 1]]) as usize;
        offset += 2;
        if offset + name_len > payload.len() { break; }
        let name = String::from_utf8_lossy(&payload[offset..offset + name_len]).to_string();
        offset += name_len;
        if kind == 2 {
            entries.push(Entry { name: "[More entries on server...]".to_string(), is_dir: false, is_symlink: false });
            continue;
        }
        entries.push(Entry { name, is_dir: kind == 1, is_symlink: false });
    }

    Ok(entries)
}
