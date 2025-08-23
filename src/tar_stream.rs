//! Simplified tar streaming for small files
//! Pulled from streaming_batch.rs and simplified for Windows focus

use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use tar::{Archive, Builder};
use walkdir::WalkDir;

/// Configuration for tar streaming
#[derive(Debug, Clone)]
pub struct TarConfig {
    /// Buffer size for channel (number of chunks)
    pub channel_buffer: usize,
    /// Size of each chunk in bytes
    pub chunk_size: usize,
}

impl Default for TarConfig {
    fn default() -> Self {
        TarConfig {
            channel_buffer: 64,      // 64 chunks in flight
            chunk_size: 1024 * 1024, // 1MB chunks
        }
    }
}

/// Channel writer that sends data through mpsc channel
struct ChannelWriter {
    tx: mpsc::SyncSender<Vec<u8>>,
    buffer: Vec<u8>,
    chunk_size: usize,
}

impl ChannelWriter {
    fn new(tx: mpsc::SyncSender<Vec<u8>>, chunk_size: usize) -> Self {
        Self {
            tx,
            buffer: Vec::with_capacity(chunk_size),
            chunk_size,
        }
    }

    fn flush_buffer(&mut self) -> io::Result<()> {
        if !self.buffer.is_empty() {
            let chunk = std::mem::replace(&mut self.buffer, Vec::with_capacity(self.chunk_size));
            self.tx
                .send(chunk)
                .map_err(|e| io::Error::new(io::ErrorKind::BrokenPipe, e))?;
        }
        Ok(())
    }
}

impl Write for ChannelWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut written = 0;
        let mut remaining = buf;

        while !remaining.is_empty() {
            let available = self.chunk_size - self.buffer.len();
            let to_write = remaining.len().min(available);

            self.buffer.extend_from_slice(&remaining[..to_write]);
            written += to_write;
            remaining = &remaining[to_write..];

            if self.buffer.len() >= self.chunk_size {
                self.flush_buffer()?;
            }
        }

        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.flush_buffer()
    }
}

impl Drop for ChannelWriter {
    fn drop(&mut self) {
        let _ = self.flush_buffer();
    }
}

/// Stream files through tar without intermediate file
pub fn tar_stream_transfer(
    source: &Path,
    dest: &Path,
    config: &TarConfig,
    show_progress: bool,
    _start_offset: u64,
) -> Result<(u64, u64)> {
    // Ensure destination exists
    fs::create_dir_all(dest)?;

    // Create channel for streaming
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(config.channel_buffer);

    // Progress bar
    let progress = if show_progress {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} [{elapsed_precise}] {msg}")
                .unwrap(),
        );
        pb.set_message("Streaming files via tar...");
        Some(pb)
    } else {
        None
    };

    let source_path = source.to_path_buf();
    let dest_path = dest.to_path_buf();
    let chunk_size = config.chunk_size;
    let progress_clone = progress.clone();

    // Thread 1: Create tar stream
    let packer = thread::spawn(move || -> Result<(u64, u64)> {
        let mut writer = ChannelWriter::new(tx, chunk_size);
        let mut file_count = 0u64;
        let mut total_bytes = 0u64;

        {
            let mut builder = Builder::new(&mut writer);

            // Walk directory and add files
            for entry in WalkDir::new(&source_path)
                .follow_links(false)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if path.is_file() {
                    let rel_path = path.strip_prefix(&source_path).unwrap_or(path);

                    if let Ok(metadata) = path.metadata() {
                        total_bytes += metadata.len();
                        file_count += 1;

                        if let Some(ref pb) = progress_clone {
                            pb.set_message(format!(
                                "Packing {} files ({} MB)",
                                file_count,
                                total_bytes / 1_048_576
                            ));
                        }
                    }

                    // Add file to tar
                    builder.append_path_with_name(path, rel_path)?;
                }
            }

            builder.finish()?;
        }

        writer.flush()?;
        Ok((file_count, total_bytes))
    });

    // Thread 2: Extract tar stream
    let unpacker = thread::spawn(move || -> Result<()> {
        let reader = ChannelReader::new(rx);
        let mut archive = Archive::new(reader);

        // Extract to destination
        archive.unpack(&dest_path)?;
        Ok(())
    });

    // Wait for both threads
    let (file_count, total_bytes) = packer
        .join()
        .map_err(|_| anyhow::anyhow!("Packer thread panicked"))??;

    unpacker
        .join()
        .map_err(|_| anyhow::anyhow!("Unpacker thread panicked"))??;

    if let Some(pb) = progress {
        pb.finish_with_message(format!(
            "Streamed {} files ({} MB)",
            file_count,
            total_bytes / 1_048_576
        ));
    }

    Ok((file_count, total_bytes))
}

/// Stream an explicit list of files (src path + tar path) through tar without staging
pub fn tar_stream_transfer_list(
    files: &[(PathBuf, PathBuf)],
    dest: &Path,
    config: &TarConfig,
    show_progress: bool,
) -> Result<(u64, u64)> {
    // Ensure destination exists
    fs::create_dir_all(dest)?;

    // Create channel for streaming
    let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(config.channel_buffer);

    // Progress bar
    let progress = if show_progress {
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} [{elapsed_precise}] {msg}")
                .unwrap(),
        );
        pb.set_message("Streaming selected files via tar...");
        Some(pb)
    } else {
        None
    };

    let files_list = files.to_owned();
    let dest_path = dest.to_path_buf();
    let chunk_size = config.chunk_size;
    let progress_clone = progress.clone();

    // Thread 1: Create tar stream for explicit list
    let packer = thread::spawn(move || -> Result<(u64, u64)> {
        let mut writer = ChannelWriter::new(tx, chunk_size);
        let mut file_count = 0u64;
        let mut total_bytes = 0u64;

        {
            let mut builder = Builder::new(&mut writer);

            for (src_path, tar_rel_path) in files_list.iter() {
                if let Ok(metadata) = src_path.metadata() {
                    total_bytes += metadata.len();
                    file_count += 1;
                    if let Some(ref pb) = progress_clone {
                        pb.set_message(format!(
                            "Packing {} files ({} MB)",
                            file_count,
                            total_bytes / 1_048_576
                        ));
                    }
                }

                builder.append_path_with_name(src_path, tar_rel_path)?;
            }

            builder.finish()?;
        }

        writer.flush()?;
        Ok((file_count, total_bytes))
    });

    // Thread 2: Extract tar stream
    let unpacker = thread::spawn(move || -> Result<()> {
        let reader = ChannelReader::new(rx);
        let mut archive = Archive::new(reader);
        archive.unpack(&dest_path)?;
        Ok(())
    });

    // Wait for both threads
    let (file_count, total_bytes) = packer
        .join()
        .map_err(|_| anyhow::anyhow!("Packer thread panicked"))??;

    unpacker
        .join()
        .map_err(|_| anyhow::anyhow!("Unpacker thread panicked"))??;

    if let Some(pb) = progress {
        pb.finish_with_message(format!(
            "Streamed {} files ({} MB)",
            file_count,
            total_bytes / 1_048_576
        ));
    }

    Ok((file_count, total_bytes))
}

/// Channel reader that receives data from mpsc channel
struct ChannelReader {
    rx: mpsc::Receiver<Vec<u8>>,
    buffer: Vec<u8>,
    buffer_pos: usize,
}

impl ChannelReader {
    fn new(rx: mpsc::Receiver<Vec<u8>>) -> Self {
        Self {
            rx,
            buffer: Vec::new(),
            buffer_pos: 0,
        }
    }
}

impl Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // If we have data in our buffer, use it first
        if self.buffer_pos < self.buffer.len() {
            let available = self.buffer.len() - self.buffer_pos;
            let to_copy = available.min(buf.len());
            buf[..to_copy]
                .copy_from_slice(&self.buffer[self.buffer_pos..self.buffer_pos + to_copy]);
            self.buffer_pos += to_copy;
            return Ok(to_copy);
        }

        // Buffer is empty, get new chunk from channel
        match self.rx.recv() {
            Ok(chunk) => {
                if chunk.is_empty() {
                    return Ok(0);
                }

                self.buffer = chunk;
                self.buffer_pos = 0;

                // Now copy from the new buffer
                let to_copy = self.buffer.len().min(buf.len());
                buf[..to_copy].copy_from_slice(&self.buffer[..to_copy]);
                self.buffer_pos = to_copy;
                Ok(to_copy)
            }
            Err(_) => Ok(0), // Channel closed, EOF
        }
    }
}
