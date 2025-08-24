use anyhow::Result;
use chrono::Utc;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::Mutex;

pub trait Logger: Send + Sync {
    fn start(&self, _src: &Path, _dst: &Path) {}
    fn copy_done(&self, _src: &Path, _dst: &Path, _bytes: u64) {}
    fn delete(&self, _path: &Path) {}
    fn error(&self, _context: &str, _path: &Path, _msg: &str) {}
    fn done(&self, _files: u64, _bytes: u64, _seconds: f64) {}
}

pub struct NoopLogger;
impl Logger for NoopLogger {}

pub struct TextLogger {
    file: Mutex<File>,
}

impl TextLogger {
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let f = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self {
            file: Mutex::new(f),
        })
    }

    fn line(&self, s: &str) {
        if let Ok(mut f) = self.file.lock() {
            let _ = writeln!(f, "[{}] {}", Utc::now().to_rfc3339(), s);
        }
    }
}

impl Logger for TextLogger {
    fn start(&self, src: &Path, dst: &Path) {
        self.line(&format!("START src={} dst={}", src.display(), dst.display()));
    }
    fn copy_done(&self, src: &Path, dst: &Path, bytes: u64) {
        self.line(&format!(
            "COPY src={} dst={} bytes={}",
            src.display(),
            dst.display(),
            bytes
        ));
    }
    fn delete(&self, path: &Path) {
        self.line(&format!("DELETE path={}", path.display()));
    }
    fn error(&self, context: &str, path: &Path, msg: &str) {
        self.line(&format!("ERROR ctx={} path={} msg={}", context, path.display(), msg));
    }
    fn done(&self, files: u64, bytes: u64, seconds: f64) {
        self.line(&format!("DONE files={files} bytes={bytes} seconds={seconds:.3}"));
    }
}
