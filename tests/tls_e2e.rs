use anyhow::Result;
use blit::{net_async, tls, Args};
use std::io::Write;

fn write_file(path: &std::path::Path, size: usize) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::File::create(path)?;
    if size == 0 {
        return Ok(());
    }
    let mut buf = vec![0u8; 1024 * 64];
    let mut remaining = size;
    let mut val: u8 = 0;
    while remaining > 0 {
        for b in buf.iter_mut() {
            *b = val;
            val = val.wrapping_add(1);
        }
        let n = remaining.min(buf.len());
        f.write_all(&buf[..n])?;
        remaining -= n;
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_push_pull_basic() -> Result<()> {
    // Prepare server root and client src/dest
    let srv_tmp = tempfile::tempdir()?;
    let cli_src = tempfile::tempdir()?;
    let cli_dst = tempfile::tempdir()?;

    // Create sample files and dirs in client source
    write_file(&cli_src.path().join("a.txt"), 8 * 1024)?; // small
    write_file(&cli_src.path().join("dir1/b.bin"), 256 * 1024)?; // medium
    write_file(&cli_src.path().join("dir1/dir2/c.dat"), 1_100_000)?; // crosses 1MB

    // Pick a free port and start a real TLS server using net_async implementation
    let port = {
        let sock = std::net::TcpListener::bind("127.0.0.1:0")?;
        let p = sock.local_addr()?.port();
        drop(sock);
        p
    };
    let bind = format!("127.0.0.1:{}", port);
    let tls_config = tls::load_or_generate_server_config(None, None)?;
    let srv_root = srv_tmp.path().to_path_buf();
    let server_task = tokio::spawn(async move {
        let _ = net_async::server::serve_with_tls(&bind, &srv_root, tls_config).await;
    });

    // Wait for server to start accepting connections
    for _ in 0..50u32 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    // Build client args
    let mut args = Args::default();
    args.empty_dirs = true;
    args.net_workers = 2;
    args.net_chunk_mb = 2;

    // Push client src -> server:dest
    let dest_on_server = std::path::Path::new("dest");
    net_async::client::push("127.0.0.1", port, dest_on_server, cli_src.path(), &args).await?;

    // Verify files exist on server
    assert!(srv_tmp.path().join("dest/a.txt").exists());
    assert!(srv_tmp.path().join("dest/dir1/b.bin").exists());
    assert!(srv_tmp.path().join("dest/dir1/dir2/c.dat").exists());

    // Pull server:dest -> client dest root
    net_async::client::pull("127.0.0.1", port, dest_on_server, cli_dst.path(), &args).await?;

    // Verify pulled files
    assert!(cli_dst.path().join("dest/a.txt").exists());
    assert!(cli_dst.path().join("dest/dir1/b.bin").exists());
    assert!(cli_dst.path().join("dest/dir1/dir2/c.dat").exists());

    // Cleanup server task
    server_task.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_list_smoke() -> Result<()> {
    let srv_tmp = tempfile::tempdir()?;
    // Prepare a couple of directories/files
    std::fs::create_dir_all(srv_tmp.path().join("alpha/beta")).ok();
    write_file(&srv_tmp.path().join("alpha/beta/file.txt"), 1024)?;

    let port = {
        let sock = std::net::TcpListener::bind("127.0.0.1:0")?;
        let p = sock.local_addr()?.port();
        drop(sock);
        p
    };
    let bind = format!("127.0.0.1:{}", port);
    let tls_config = tls::load_or_generate_server_config(None, None)?;
    let root = srv_tmp.path().to_path_buf();
    let server_task = tokio::spawn(async move {
        let _ = net_async::server::serve_with_tls(&bind, &root, tls_config).await;
    });
    for _ in 0..50u32 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let url = format!("blit://127.0.0.1:{}/alpha", port);
    net_async::client::complete_remote(&url).await?;

    server_task.abort();
    Ok(())
}

// Local minimal frame I/O for test server
#[allow(dead_code)]
async fn read_frame<S>(stream: &mut S) -> Result<(u8, Vec<u8>)>
where
    S: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut hdr = [0u8; 11];
    stream.read_exact(&mut hdr).await?;
    let (typ, len_u32) = blit::protocol_core::parse_frame_header(&hdr)?;
    let len = len_u32 as usize;
    blit::protocol_core::validate_frame_size(len)?;
    let mut payload = vec![0u8; len];
    if len > 0 {
        stream.read_exact(&mut payload).await?;
    }
    Ok((typ, payload))
}

async fn write_frame<S>(stream: &mut S, t: u8, payload: &[u8]) -> Result<()>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;
    let hdr = blit::protocol_core::build_frame_header(t, payload.len() as u32);
    stream.write_all(&hdr).await?;
    if !payload.is_empty() {
        stream.write_all(payload).await?;
    }
    Ok(())
}
