//! Core delta-transfer algorithm implementation
//!
//! This module contains the modern implementation of rsync's delta-transfer algorithm,
//! based on the rolling checksum approach from the original match.c

use crate::checksum::{get_checksum1, strong_checksum, ChecksumType, RollingChecksum};
use anyhow::Result;
use std::collections::HashMap;

/// Block size for delta algorithm (will be configurable)
pub const DEFAULT_BLOCK_SIZE: usize = 1024;

/// Delta-transfer algorithm implementation
pub struct DeltaAlgorithm {
    block_size: usize,
    checksum_type: ChecksumType,
}

impl Default for DeltaAlgorithm {
    fn default() -> Self {
        Self::new(DEFAULT_BLOCK_SIZE)
    }
}

impl DeltaAlgorithm {
    pub fn new(block_size: usize) -> Self {
        Self {
            block_size,
            checksum_type: ChecksumType::default(),
        }
    }

    pub fn with_checksum_type(mut self, checksum_type: ChecksumType) -> Self {
        self.checksum_type = checksum_type;
        self
    }


    /// Generate checksums for destination file blocks
    pub fn generate_checksums(&self, data: &[u8]) -> Result<Vec<BlockChecksum>> {
        let mut checksums = Vec::new();
        let mut offset = 0;

        while offset < data.len() {
            let block_end = std::cmp::min(offset + self.block_size, data.len());
            let block = &data[offset..block_end];

            let rolling_checksum = get_checksum1(block);
            let strong_checksum = strong_checksum(block, self.checksum_type)?;

            // Pad to 32 bytes for consistency
            let mut strong_hash = [0u8; 32];
            let copy_len = std::cmp::min(strong_checksum.len(), 32);
            strong_hash[..copy_len].copy_from_slice(&strong_checksum[..copy_len]);

            checksums.push(BlockChecksum {
                offset: offset as u64,
                rolling_checksum,
                strong_checksum: strong_hash,
                length: block.len(),
            });

            offset = block_end;
        }

        Ok(checksums)
    }

    /// Find matching blocks using rolling checksum (based on rsync's hash_search)
    pub fn find_matches(&self, source: &[u8], checksums: &[BlockChecksum]) -> Result<Vec<Match>> {
        if checksums.is_empty() {
            // No destination blocks, everything is literal data
            let (literal_data, is_compressed) = self.process_literal_data(source)?;
            return Ok(vec![Match::Literal {
                offset: 0,
                data: literal_data,
                is_compressed,
            }]);
        }

        // Build hash table for O(1) lookups (like rsync's build_hash_table)
        let mut hash_table = HashMap::new();
        for (i, checksum) in checksums.iter().enumerate() {
            hash_table
                .entry(checksum.rolling_checksum)
                .or_insert_with(Vec::new)
                .push(i);
        }

        let mut matches = Vec::new();
        let mut offset = 0;
        let mut last_match = 0;
        let mut rolling = RollingChecksum::new(self.block_size);

        if source.len() < self.block_size {
            // Source too small for block matching
            let (literal_data, is_compressed) = self.process_literal_data(source)?;
            return Ok(vec![Match::Literal {
                offset: 0,
                data: literal_data,
                is_compressed,
            }]);
        }

        // Initialize rolling checksum with first block
        let initial_block_size = std::cmp::min(self.block_size, source.len());
        rolling.init(&source[0..initial_block_size]);

        let end = source.len() + 1 - initial_block_size;

        while offset < end {
            let current_checksum = rolling.value();

            // Look for potential matches in hash table
            if let Some(indices) = hash_table.get(&current_checksum) {
                let mut found_match = false;

                for &block_index in indices {
                    let block_checksum = &checksums[block_index];

                    // Verify strong checksum to avoid false positives
                    let block_end = std::cmp::min(offset + block_checksum.length, source.len());
                    let current_block = &source[offset..block_end];

                    if current_block.len() == block_checksum.length {
                        let strong_hash = strong_checksum(current_block, self.checksum_type)?;
                        let mut expected_hash = [0u8; 32];
                        let copy_len = std::cmp::min(strong_hash.len(), 32);
                        expected_hash[..copy_len].copy_from_slice(&strong_hash[..copy_len]);

                        if expected_hash == block_checksum.strong_checksum {
                            // Found a match!

                            // First, emit any literal data before this match
                            if offset > last_match {
                                let literal_chunk = &source[last_match..offset];
                                let (literal_data, is_compressed) =
                                    self.process_literal_data(literal_chunk)?;
                                matches.push(Match::Literal {
                                    offset: last_match as u64,
                                    data: literal_data,
                                    is_compressed,
                                });
                            }

                            // Emit the block match
                            matches.push(Match::Block {
                                source_offset: offset as u64,
                                target_offset: block_checksum.offset,
                                length: block_checksum.length,
                            });

                            // Skip ahead past this match
                            offset += block_checksum.length;
                            last_match = offset;
                            found_match = true;

                            // Reinitialize rolling checksum if we haven't reached the end
                            if offset < end {
                                let next_block_size =
                                    std::cmp::min(self.block_size, source.len() - offset);
                                rolling.init(&source[offset..offset + next_block_size]);
                            }
                            break;
                        }
                    }
                }

                if found_match {
                    continue;
                }
            }

            // No match found, roll the checksum forward by one byte
            if offset + initial_block_size < source.len() {
                rolling.roll(source[offset], source[offset + initial_block_size]);
            }
            offset += 1;
        }

        // Emit any remaining literal data
        if last_match < source.len() {
            let literal_chunk = &source[last_match..];
            let (literal_data, is_compressed) = self.process_literal_data(literal_chunk)?;
            matches.push(Match::Literal {
                offset: last_match as u64,
                data: literal_data,
                is_compressed,
            });
        }

        Ok(matches)
    }

    /// Process literal data (no compression for now)
    fn process_literal_data(&self, data: &[u8]) -> Result<(Vec<u8>, bool)> {
        Ok((data.to_vec(), false))
    }
}

/// Block checksum information
#[derive(Debug, Clone)]
pub struct BlockChecksum {
    pub offset: u64,
    pub rolling_checksum: u32,
    pub strong_checksum: [u8; 32], // BLAKE3/xxHash/MD5 hash
    pub length: usize,
}

/// Represents a match between source and target
#[derive(Debug, Clone)]
pub enum Match {
    /// Block matches existing data at given offset  
    Block {
        source_offset: u64,
        target_offset: u64,
        length: usize,
    },
    /// Literal data that needs to be copied
    Literal {
        offset: u64,
        data: Vec<u8>,
        is_compressed: bool,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_checksums() {
        let algorithm = DeltaAlgorithm::new(4);
        let data = b"Hello, World!";

        let checksums = algorithm
            .generate_checksums(data)
            .expect("Failed to generate checksums");

        // Should have ceil(13/4) = 4 blocks
        assert_eq!(checksums.len(), 4);

        // First block should be "Hell"
        assert_eq!(checksums[0].length, 4);
        assert_eq!(checksums[0].offset, 0);

        // Last block should be "!"
        assert_eq!(checksums[3].length, 1);
        assert_eq!(checksums[3].offset, 12);
    }

    #[test]
    fn test_find_matches_identical() {
        let algorithm = DeltaAlgorithm::new(4);
        let data = b"Hello, World!";

        let checksums = algorithm
            .generate_checksums(data)
            .expect("Failed to generate checksums");
        let matches = algorithm
            .find_matches(data, &checksums)
            .expect("Failed to find matches");

        // Should be all block matches for identical data
        let block_matches: Vec<_> = matches
            .iter()
            .filter(|m| matches!(m, Match::Block { .. }))
            .collect();

        assert!(!block_matches.is_empty(), "Should have found block matches");
    }

    #[test]
    fn test_find_matches_no_destination() {
        let algorithm = DeltaAlgorithm::new(4);
        let data = b"Hello, World!";

        let matches = algorithm
            .find_matches(data, &[])
            .expect("Failed to find matches");

        // Should be one literal match containing all data
        assert_eq!(matches.len(), 1);
        match &matches[0] {
            Match::Literal {
                data: literal_data,
                is_compressed,
                ..
            } => {
                assert_eq!(literal_data, data);
                assert!(!is_compressed); // No compression config set
            }
            _ => panic!("Expected literal match"),
        }
    }
}
