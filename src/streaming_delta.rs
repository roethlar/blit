//! Streaming delta transfer algorithm that doesn't load entire files into memory
//!
//! This implementation processes files in chunks to handle files of any size
//! without memory exhaustion.

use crate::algorithm::BlockChecksum;
use crate::checksum::{get_checksum1, strong_checksum, ChecksumType};
use anyhow::Result;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

const CHUNK_SIZE: usize = 16 * 1024 * 1024; // 16MB chunks for processing

/// Streaming delta algorithm that processes files without loading them into memory
pub struct StreamingDelta {
    block_size: usize,
    checksum_type: ChecksumType,
}

impl StreamingDelta {
    pub fn new(block_size: usize) -> Self {
        Self {
            block_size,
            checksum_type: ChecksumType::default(),
        }
    }

    /// Generate checksums for destination file blocks using streaming reads
    pub fn generate_checksums_streaming(&self, path: &Path) -> Result<Vec<BlockChecksum>> {
        let file = File::open(path)?;
        let file_size = file.metadata()?.len();
        let mut reader = BufReader::new(file);
        let mut checksums = Vec::new();
        let mut offset = 0u64;
        let mut buffer = vec![0u8; self.block_size];
        let mut last_progress = std::time::Instant::now();

        while offset < file_size {
            let bytes_to_read = std::cmp::min(self.block_size, (file_size - offset) as usize);
            reader.read_exact(&mut buffer[..bytes_to_read])?;

            // Report progress on large files
            if file_size > 100 * 1024 * 1024
                && last_progress.elapsed() >= std::time::Duration::from_secs(2)
            {
                eprint!(
                    "\r      Checksum generation: {:.1}% ({}/{} MB)",
                    (offset as f64 / file_size as f64) * 100.0,
                    offset / (1024 * 1024),
                    file_size / (1024 * 1024)
                );
                last_progress = std::time::Instant::now();
            }

            let rolling_checksum = get_checksum1(&buffer[..bytes_to_read]);
            let strong_checksum = strong_checksum(&buffer[..bytes_to_read], self.checksum_type)?;

            let mut strong_hash = [0u8; 32];
            let copy_len = std::cmp::min(strong_checksum.len(), 32);
            strong_hash[..copy_len].copy_from_slice(&strong_checksum[..copy_len]);

            checksums.push(BlockChecksum {
                offset,
                rolling_checksum,
                strong_checksum: strong_hash,
                length: bytes_to_read,
            });

            offset += bytes_to_read as u64;
        }

        // Clear the progress line if we showed any progress
        if file_size > 100 * 1024 * 1024 {
            eprint!("\r                                                                      \r");
        }

        Ok(checksums)
    }

    /// Apply delta transfer using streaming reads and writes
    pub fn apply_delta_streaming(
        &self,
        source_path: &Path,
        dest_path: &Path,
        temp_path: &Path,
        checksums: &[BlockChecksum],
    ) -> Result<u64> {
        // Build hash table for O(1) lookups
        let mut hash_table = HashMap::new();
        for (i, checksum) in checksums.iter().enumerate() {
            hash_table
                .entry(checksum.rolling_checksum)
                .or_insert_with(Vec::new)
                .push(i);
        }

        let source_file = File::open(source_path)?;
        let source_size = source_file.metadata()?.len();
        let mut source_reader = BufReader::new(source_file);

        let dest_file = File::open(dest_path)?;
        let mut dest_reader = BufReader::new(dest_file);

        let temp_file = File::create(temp_path)?;
        let mut writer = BufWriter::new(temp_file);

        let mut transferred_bytes = 0u64;
        let mut source_offset = 0u64;

        // Use a reasonable buffer size for streaming based on file size
        let buffer_size = if source_size < 10 * 1024 * 1024 {
            1024 * 1024 // 1MB for smaller files
        } else if source_size < 100 * 1024 * 1024 {
            4 * 1024 * 1024 // 4MB for medium files
        } else {
            16 * 1024 * 1024 // 16MB for large files
        };

        let mut lookahead_buffer = vec![0u8; buffer_size];
        let mut last_progress_report = std::time::Instant::now();
        let report_interval = std::time::Duration::from_secs(5);

        while source_offset < source_size {
            let bytes_to_read = std::cmp::min(
                std::cmp::min(CHUNK_SIZE, (source_size - source_offset) as usize),
                buffer_size, // Also limit by actual buffer size
            );
            source_reader.read_exact(&mut lookahead_buffer[..bytes_to_read])?;

            let (matches, non_match_len) =
                self.find_all_matches(&lookahead_buffer[..bytes_to_read], &hash_table, checksums)?;

            if !matches.is_empty() {
                // Process all matches found in the lookahead buffer
                let mut last_match_end = 0;
                for (start, end, block_indices) in matches {
                    // Write literal data before the match
                    if start > last_match_end {
                        writer.write_all(&lookahead_buffer[last_match_end..start])?;
                        transferred_bytes += (start - last_match_end) as u64;
                    }

                    // Write matching blocks from the destination
                    for &block_index in &block_indices {
                        let block_checksum = &checksums[block_index];
                        dest_reader.seek(SeekFrom::Start(block_checksum.offset))?;
                        let mut block_buffer = vec![0u8; block_checksum.length];
                        dest_reader.read_exact(&mut block_buffer)?;
                        writer.write_all(&block_buffer)?;
                    }
                    last_match_end = end;
                }

                // Write any remaining literal data after the last match
                if last_match_end < bytes_to_read {
                    writer.write_all(&lookahead_buffer[last_match_end..bytes_to_read])?;
                    transferred_bytes += (bytes_to_read - last_match_end) as u64;
                }

                source_offset += bytes_to_read as u64;

                // Report progress periodically for large files
                if last_progress_report.elapsed() >= report_interval {
                    eprintln!(
                        "      Delta progress: {:.1}% ({}/{} MB processed, {} MB transferred)",
                        (source_offset as f64 / source_size as f64) * 100.0,
                        source_offset / (1024 * 1024),
                        source_size / (1024 * 1024),
                        transferred_bytes / (1024 * 1024)
                    );
                    last_progress_report = std::time::Instant::now();
                }
            } else if non_match_len > 0 {
                // No matches found in the entire lookahead buffer, write as a literal.
                writer.write_all(&lookahead_buffer[..non_match_len])?;
                transferred_bytes += non_match_len as u64;
                source_offset += non_match_len as u64;
            } else {
                // No more data to process.
                break;
            }
        }

        writer.flush()?;
        Ok(transferred_bytes)
    }

    #[allow(clippy::type_complexity)]
    fn find_all_matches(
        &self,
        buffer: &[u8],
        hash_table: &HashMap<u32, Vec<usize>>,
        checksums: &[BlockChecksum],
    ) -> Result<(Vec<(usize, usize, Vec<usize>)>, usize)> {
        let mut matches = Vec::new();
        let mut current_offset = 0;

        while current_offset + self.block_size <= buffer.len() {
            let window = &buffer[current_offset..current_offset + self.block_size];
            let rolling_checksum = get_checksum1(window);

            if let Some(indices) = hash_table.get(&rolling_checksum) {
                let strong_hash = strong_checksum(window, self.checksum_type)?;
                let mut found_match_in_block = false;
                for &block_index in indices {
                    let block_checksum = &checksums[block_index];
                    if strong_hash[..32] == block_checksum.strong_checksum {
                        // Found a match, now find how many consecutive blocks match
                        let mut consecutive_blocks = vec![block_index];
                        let mut next_offset = current_offset + self.block_size;
                        while next_offset + self.block_size <= buffer.len() {
                            let next_window = &buffer[next_offset..next_offset + self.block_size];
                            let next_rolling_checksum = get_checksum1(next_window);
                            if let Some(next_indices) = hash_table.get(&next_rolling_checksum) {
                                let next_strong_hash =
                                    strong_checksum(next_window, self.checksum_type)?;
                                let mut found_consecutive = false;
                                for &next_block_index in next_indices {
                                    let next_block_checksum = &checksums[next_block_index];
                                    if next_strong_hash[..32] == next_block_checksum.strong_checksum
                                    {
                                        consecutive_blocks.push(next_block_index);
                                        next_offset += self.block_size;
                                        found_consecutive = true;
                                        break;
                                    }
                                }
                                if !found_consecutive {
                                    break;
                                }
                            } else {
                                break;
                            }
                        }
                        let match_start = current_offset;
                        let match_end = next_offset;
                        matches.push((match_start, match_end, consecutive_blocks));
                        current_offset = match_end;
                        found_match_in_block = true;
                        break;
                    }
                }
                if !found_match_in_block {
                    // Skip ahead by a larger step when no match is found
                    // This significantly speeds up processing of non-matching regions
                    current_offset += std::cmp::min(self.block_size / 4, 1024).max(1);
                }
            } else {
                // No rolling checksum match, skip ahead more aggressively
                current_offset += std::cmp::min(self.block_size / 4, 1024).max(1);
            }
        }

        let non_match_len = if matches.is_empty() { buffer.len() } else { 0 };

        Ok((matches, non_match_len))
    }
}
