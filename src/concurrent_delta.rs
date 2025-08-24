//! Concurrent delta analysis for intelligent transfer decisions
//!
//! This module implements the key insight from Grok/Gemini: analyze files
//! WHILE copying others, not before. Make decisions based on actual data.

use anyhow::Result;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
// use blake3::Hasher; // Not currently used

/// Block size for delta analysis (default 1MB for efficiency)
const ANALYSIS_BLOCK_SIZE: usize = 1024 * 1024;

/// Delta analysis result for a file
#[derive(Debug, Clone)]
pub struct DeltaAnalysis {
    pub source_path: PathBuf,
    pub dest_path: PathBuf,
    pub source_size: u64,
    pub dest_size: u64,
    pub blocks_total: usize,
    pub blocks_different: usize,
    pub percentage_different: f32,
    pub should_use_delta: bool,
    pub analysis_time_ms: u128,
}

/// Concurrent delta analyzer that runs in background
pub struct ConcurrentDeltaAnalyzer {
    /// Results of completed analyses
    results: Arc<Mutex<HashMap<PathBuf, DeltaAnalysis>>>,
    /// Files currently being analyzed
    in_progress: Arc<Mutex<HashMap<PathBuf, bool>>>,
    /// Thread handles for analysis tasks
    handles: Vec<thread::JoinHandle<()>>,
}

impl ConcurrentDeltaAnalyzer {
    /// Create a new concurrent delta analyzer
    pub fn new() -> Self {
        Self {
            results: Arc::new(Mutex::new(HashMap::new())),
            in_progress: Arc::new(Mutex::new(HashMap::new())),
            handles: Vec::new(),
        }
    }

    /// Start analyzing a file pair in the background
    pub fn analyze_file(&mut self, source: PathBuf, dest: PathBuf) {
        // Skip if already analyzing or completed
        {
            let in_progress = self.in_progress.lock().unwrap();
            if in_progress.contains_key(&source) {
                return;
            }
            let results = self.results.lock().unwrap();
            if results.contains_key(&source) {
                return;
            }
        }

        // Mark as in progress
        {
            let mut in_progress = self.in_progress.lock().unwrap();
            in_progress.insert(source.clone(), true);
        }

        let results = Arc::clone(&self.results);
        let in_progress = Arc::clone(&self.in_progress);
        let source_clone = source.clone();

        // Spawn background analysis thread
        let handle = thread::spawn(move || {
            let start_time = std::time::Instant::now();

            // Perform the analysis
            if let Ok(analysis) = analyze_file_pair(&source, &dest) {
                let mut analysis = analysis;
                analysis.analysis_time_ms = start_time.elapsed().as_millis();

                // Store result
                let mut results = results.lock().unwrap();
                results.insert(source.clone(), analysis);
            }

            // Remove from in progress
            let mut in_progress = in_progress.lock().unwrap();
            in_progress.remove(&source_clone);
        });

        self.handles.push(handle);
    }

    /// Check if analysis is complete for a file
    pub fn get_analysis(&self, source: &Path) -> Option<DeltaAnalysis> {
        let results = self.results.lock().unwrap();
        results.get(source).cloned()
    }

    /// Check if analysis is still in progress
    pub fn is_analyzing(&self, source: &Path) -> bool {
        let in_progress = self.in_progress.lock().unwrap();
        in_progress.contains_key(source)
    }

    /// Wait for a specific analysis to complete (with timeout)
    pub fn wait_for_analysis(&self, source: &Path, timeout_ms: u64) -> Option<DeltaAnalysis> {
        let start = std::time::Instant::now();

        loop {
            // Check if complete
            if let Some(analysis) = self.get_analysis(source) {
                return Some(analysis);
            }

            // Check timeout
            if start.elapsed().as_millis() > timeout_ms as u128 {
                return None;
            }

            // Not analyzing and not complete means it failed or wasn't started
            if !self.is_analyzing(source) {
                return None;
            }

            // Brief sleep to avoid spinning
            thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// Get all completed analyses
    pub fn get_all_results(&self) -> HashMap<PathBuf, DeltaAnalysis> {
        let results = self.results.lock().unwrap();
        results.clone()
    }
}

impl Default for ConcurrentDeltaAnalyzer {
    fn default() -> Self {
        Self::new()
    }
}

/// Analyze a file pair to determine if delta transfer makes sense
pub fn analyze_file_pair(source: &Path, dest: &Path) -> Result<DeltaAnalysis> {
    // Get file sizes
    let source_meta = std::fs::metadata(source)?;
    let dest_meta = std::fs::metadata(dest)?;

    let source_size = source_meta.len();
    let dest_size = dest_meta.len();

    // Quick decision: if sizes are very different, don't use delta
    let size_ratio = if source_size > dest_size {
        source_size as f64 / dest_size.max(1) as f64
    } else {
        dest_size as f64 / source_size.max(1) as f64
    };

    if size_ratio > 1.5 {
        // Sizes too different, probably different files
        return Ok(DeltaAnalysis {
            source_path: source.to_path_buf(),
            dest_path: dest.to_path_buf(),
            source_size,
            dest_size,
            blocks_total: 0,
            blocks_different: 0,
            percentage_different: 100.0,
            should_use_delta: false,
            analysis_time_ms: 0,
        });
    }

    // Sample blocks to estimate difference
    let blocks_total = source_size
        .min(dest_size)
        .div_ceil(ANALYSIS_BLOCK_SIZE as u64) as usize;

    // For very large files, sample instead of checking everything
    let sample_rate = if blocks_total > 1000 {
        // Sample 10% of blocks for files with >1000 blocks
        10
    } else if blocks_total > 100 {
        // Sample 25% of blocks for files with >100 blocks
        4
    } else {
        // Check all blocks for smaller files
        1
    };

    let mut source_file = File::open(source)?;
    let mut dest_file = File::open(dest)?;

    let mut blocks_different = 0;
    let mut blocks_checked = 0;
    let mut source_buf = vec![0u8; ANALYSIS_BLOCK_SIZE];
    let mut dest_buf = vec![0u8; ANALYSIS_BLOCK_SIZE];

    for block_idx in (0..blocks_total).step_by(sample_rate) {
        let offset = block_idx as u64 * ANALYSIS_BLOCK_SIZE as u64;

        // Read source block
        source_file.seek(SeekFrom::Start(offset))?;
        let source_bytes = source_file.read(&mut source_buf)?;

        // Read dest block
        dest_file.seek(SeekFrom::Start(offset))?;
        let dest_bytes = dest_file.read(&mut dest_buf)?;

        blocks_checked += 1;

        // Quick byte comparison first
        if source_bytes != dest_bytes || source_buf[..source_bytes] != dest_buf[..dest_bytes] {
            blocks_different += 1;
        } else {
            // If bytes match, check hash for certainty (optional, might skip for speed)
            let source_hash = blake3::hash(&source_buf[..source_bytes]);
            let dest_hash = blake3::hash(&dest_buf[..dest_bytes]);
            if source_hash != dest_hash {
                blocks_different += 1;
            }
        }

        // Early exit if too many differences
        if blocks_different > blocks_checked / 2 {
            // More than 50% different in sample, not worth delta
            return Ok(DeltaAnalysis {
                source_path: source.to_path_buf(),
                dest_path: dest.to_path_buf(),
                source_size,
                dest_size,
                blocks_total,
                blocks_different: blocks_total / 2, // Estimate
                percentage_different: 50.0,
                should_use_delta: false,
                analysis_time_ms: 0,
            });
        }
    }

    // Extrapolate from sample to estimate total difference
    let estimated_different = if sample_rate > 1 {
        (blocks_different * sample_rate).min(blocks_total)
    } else {
        blocks_different
    };

    let percentage_different = (estimated_different as f32 / blocks_total.max(1) as f32) * 100.0;

    // Decision logic: use delta if <20% different and file is large enough
    // Lowered minimum from 10MB → 4MB to benefit medium-size updates
    let should_use_delta = percentage_different < 20.0 && source_size >= 4 * 1024 * 1024; // 4MB minimum

    Ok(DeltaAnalysis {
        source_path: source.to_path_buf(),
        dest_path: dest.to_path_buf(),
        source_size,
        dest_size,
        blocks_total,
        blocks_different: estimated_different,
        percentage_different,
        should_use_delta,
        analysis_time_ms: 0,
    })
}

/// Quick check if a file pair might benefit from delta (no deep analysis)
pub fn quick_delta_check(source: &Path, dest: &Path) -> bool {
    // If destination doesn't exist, can't use delta
    if !dest.exists() {
        return false;
    }

    // Get file sizes
    let source_size = std::fs::metadata(source).map(|m| m.len()).unwrap_or(0);
    let dest_size = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);

    // Quick heuristics
    // Lowered minimum from 10MB → 4MB
    if source_size < 4 * 1024 * 1024 || dest_size < 4 * 1024 * 1024 {
        return false; // Too small to benefit
    }

    // Check size similarity
    let size_ratio = if source_size > dest_size {
        source_size as f64 / dest_size.max(1) as f64
    } else {
        dest_size as f64 / source_size.max(1) as f64
    };

    size_ratio < 1.2 // Sizes within 20% of each other
}
