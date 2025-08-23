use serde::{Serialize, Deserialize};
use std::path::{Path, PathBuf};
use std::fs::{OpenOptions, File};
use std::io::{BufReader, BufWriter, Write, BufRead};
use anyhow::{Result, Context};
use chrono::Utc;

#[derive(Serialize, Deserialize, Debug)]
pub enum TransferStatus {
    InProgress,
    Completed,
    Interrupted,
    Failed,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct TransferLogEntry {
    pub timestamp: String,
    pub sync_job_id: String,
    pub source: PathBuf,
    pub destination: PathBuf,
    pub temp_path: Option<PathBuf>,
    pub status: TransferStatus,
    pub bytes_transferred: u64,
    pub current_bytes_transferred: u64,
    pub error: Option<String>,
}

pub struct TransferLog {
    log_file_path: PathBuf,
}

impl TransferLog {
    pub fn new(destination_root: &Path) -> Self {
        let log_file_path = destination_root.join(".robosync_transfer.jsonl");
        TransferLog { log_file_path }
    }

    pub fn add_entry(&self, entry: TransferLogEntry) -> Result<()> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.log_file_path)
            .context("Failed to open transfer log file")?;
        let mut writer = BufWriter::new(file);
        serde_json::to_writer(&mut writer, &entry)?;
        writer.write_all(b"\n")?;
        writer.flush()?;
        Ok(())
    }

    pub fn read_log(&self) -> Result<Vec<TransferLogEntry>> {
        if !self.log_file_path.exists() {
            return Ok(Vec::new());
        }
        let file = File::open(&self.log_file_path)
            .context("Failed to open transfer log file for reading")?;
        let reader = BufReader::new(file);
        let mut entries = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let entry: TransferLogEntry = serde_json::from_str(&line)?;
            entries.push(entry);
        }
        Ok(entries)
    }
}
