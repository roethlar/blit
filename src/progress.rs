//! Cargo-style progress display
//! 
//! This creates a progress display similar to cargo where:
//! - File operations scroll above 
//! - Progress spinner/status stays fixed at bottom
//! - Clean, professional appearance

use crossterm::{
    cursor,
    style::{Color, Stylize},
    terminal,
    ExecutableCommand,
};
use indicatif::{ProgressBar, ProgressStyle};
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct CargoProgress {
    spinner: ProgressBar,
    start_time: Instant,
    last_status: Arc<Mutex<String>>,
    show_files: bool,
}

impl CargoProgress {
    pub fn new(verbose: bool) -> Self {
        // Set up terminal for cargo-style display
        let _ = terminal::enable_raw_mode();
        let _ = io::stdout().execute(cursor::Hide);
        
        let spinner = ProgressBar::new_spinner();
        spinner.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner())
                .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
        );
        
        // Enable steady tick for smooth animation
        spinner.enable_steady_tick(Duration::from_millis(100));
        
        Self {
            spinner,
            start_time: Instant::now(),
            last_status: Arc::new(Mutex::new(String::new())),
            show_files: verbose,
        }
    }
    
    /// Print a file operation above the progress line (cargo-style)
    pub fn print_file_op(&self, operation: &str, path: &str) {
        if self.show_files {
            // Clear current progress line
            self.spinner.suspend(|| {
                println!(
                    "  {} {}",
                    operation.with(Color::Green).bold(),
                    path.with(Color::Cyan)
                );
            });
        }
    }
    
    /// Update the bottom status line (like cargo's "Compiling..." line)
    pub fn set_status(&self, stage: &str, current: u64, total: u64, details: Option<&str>) {
        let elapsed = self.start_time.elapsed();
        
        let msg = if total > 0 {
            format!(
                "{} ({}/{}) in {:.1}s{}",
                stage.with(Color::Green).bold(),
                current,
                total,
                elapsed.as_secs_f64(),
                details.map(|d| format!(" - {}", d)).unwrap_or_default()
            )
        } else {
            format!(
                "{} in {:.1}s{}",
                stage.with(Color::Green).bold(),
                elapsed.as_secs_f64(),
                details.map(|d| format!(" - {}", d)).unwrap_or_default()
            )
        };
        
        self.spinner.set_message(msg);
        
        // Store for cleanup
        if let Ok(mut status) = self.last_status.lock() {
            *status = format!("{} ({}/{})", stage, current, total);
        }
    }
    
    /// Update with throughput info
    pub fn set_status_with_throughput(&self, stage: &str, files: u64, bytes: u64) {
        let elapsed = self.start_time.elapsed();
        let elapsed_secs = elapsed.as_secs_f64();
        
        let throughput = if elapsed_secs > 0.1 {
            format!(" @ {:.1} MB/s", bytes as f64 / elapsed_secs / 1_048_576.0)
        } else {
            String::new()
        };
        
        let msg = format!(
            "{} {} files ({:.1} MB) in {:.1}s{}",
            stage.with(Color::Green).bold(),
            files,
            bytes as f64 / 1_048_576.0,
            elapsed_secs,
            throughput
        );
        
        self.spinner.set_message(msg);
    }
    
    /// Finish with success message
    pub fn finish_success(&self, files: u64, bytes: u64) {
        let elapsed = self.start_time.elapsed();
        let throughput = bytes as f64 / elapsed.as_secs_f64() / 1_048_576.0;
        
        self.spinner.finish_with_message(format!(
            "{} {} files ({:.1} MB) in {:.1}s ({:.1} MB/s)",
            "Completed".with(Color::Green).bold(),
            files,
            bytes as f64 / 1_048_576.0,
            elapsed.as_secs_f64(),
            throughput
        ));
        
        self.cleanup();
    }
    
    /// Finish with error
    pub fn finish_error(&self, msg: &str) {
        self.spinner.finish_with_message(format!(
            "{} {}",
            "Failed".with(Color::Red).bold(),
            msg
        ));
        
        self.cleanup();
    }
    
    fn cleanup(&self) {
        let _ = io::stdout().execute(cursor::Show);
        let _ = terminal::disable_raw_mode();
    }
}

impl Drop for CargoProgress {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Simple spinner for non-cargo style (minimal mode)
pub struct SimpleSpinner {
    spinner: ProgressBar,
    start_time: Instant,
}

impl SimpleSpinner {
    pub fn new() -> Self {
        let spinner = ProgressBar::new_spinner();
        spinner.set_style(
            ProgressStyle::default_spinner()
                .template("  {spinner} {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner())
        );
        spinner.enable_steady_tick(Duration::from_millis(120));
        
        Self {
            spinner,
            start_time: Instant::now(),
        }
    }
    
    pub fn set_message(&self, msg: &str) {
        self.spinner.set_message(msg.to_string());
    }
    
    pub fn finish_with_message(&self, msg: &str) {
        self.spinner.finish_with_message(msg.to_string());
    }
}