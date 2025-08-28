//! Checksum and hashing utilities

use anyhow::Result;

/// Available checksum algorithms
#[derive(Debug, Clone, Copy)]
pub enum ChecksumType {
    Blake3,
    XxHash3,
    Md5, // For compatibility
}

impl Default for ChecksumType {
    fn default() -> Self {
        Self::Blake3
    }
}

/// CHAR_OFFSET constant from rsync (for compatibility)
const CHAR_OFFSET: u32 = 31;

/// Fast rolling checksum implementation (based on rsync's get_checksum1)
/// This implements the same algorithm as rsync for compatibility
pub struct RollingChecksum {
    s1: u32,
    s2: u32,
    block_size: usize,
}

impl RollingChecksum {
    pub fn new(block_size: usize) -> Self {
        Self {
            s1: 0,
            s2: 0,
            block_size,
        }
    }

    /// Initialize checksum for a block of data (rsync-compatible)
    pub fn init(&mut self, data: &[u8]) {
        self.s1 = 0;
        self.s2 = 0;

        // Use rsync's algorithm: process 4 bytes at a time for speed
        let len = data.len();
        let mut i = 0;

        // Process 4 bytes at a time (rsync optimization)
        while i + 4 <= len {
            let b0 = data[i] as u32;
            let b1 = data[i + 1] as u32;
            let b2 = data[i + 2] as u32;
            let b3 = data[i + 3] as u32;

            self.s2 = self.s2.wrapping_add(
                4u32.wrapping_mul(self.s1.wrapping_add(b0))
                    .wrapping_add(3u32.wrapping_mul(b1))
                    .wrapping_add(2u32.wrapping_mul(b2))
                    .wrapping_add(b3)
                    .wrapping_add(10u32.wrapping_mul(CHAR_OFFSET)),
            );

            self.s1 = self.s1.wrapping_add(
                b0.wrapping_add(b1)
                    .wrapping_add(b2)
                    .wrapping_add(b3)
                    .wrapping_add(4u32.wrapping_mul(CHAR_OFFSET)),
            );

            i += 4;
        }

        // Process remaining bytes
        while i < len {
            let byte = data[i] as u32;
            self.s1 = self.s1.wrapping_add(byte.wrapping_add(CHAR_OFFSET));
            self.s2 = self.s2.wrapping_add(self.s1);
            i += 1;
        }
    }

    /// Roll the checksum by removing old byte and adding new byte
    pub fn roll(&mut self, old_byte: u8, new_byte: u8) {
        let old = old_byte as u32 + CHAR_OFFSET;
        let new = new_byte as u32 + CHAR_OFFSET;

        self.s1 = self.s1.wrapping_sub(old).wrapping_add(new);
        self.s2 = self
            .s2
            .wrapping_sub(self.block_size as u32 * old)
            .wrapping_add(self.s1);
    }

    /// Get current checksum value (rsync format: s1 in lower 16 bits, s2 in upper 16 bits)
    pub fn value(&self) -> u32 {
        (self.s1 & 0xFFFF) | (self.s2 << 16)
    }
}

/// Compute rolling checksum for a block (rsync-compatible)
pub fn get_checksum1(data: &[u8]) -> u32 {
    let mut checksum = RollingChecksum::new(data.len());
    checksum.init(data);
    checksum.value()
}

/// Compute strong checksum for data
pub fn strong_checksum(data: &[u8], checksum_type: ChecksumType) -> Result<Vec<u8>> {
    match checksum_type {
        ChecksumType::Blake3 => {
            let hash = blake3::hash(data);
            Ok(hash.as_bytes().to_vec())
        }
        ChecksumType::XxHash3 => {
            // Use blake3 as a fast alternative to xxhash
            Ok(blake3::hash(data).as_bytes()[..8].to_vec())
        }
        ChecksumType::Md5 => {
            // SECURITY WARNING: MD5 is cryptographically broken
            eprintln!("⚠️  WARNING: MD5 is cryptographically broken and should not be used");
            eprintln!("   Consider using Blake3 (default) or SHA-256 for security");
            Ok(md5::compute(data).to_vec())
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rolling_checksum_basic() {
        let data = b"Hello, World!";
        let checksum = get_checksum1(data);

        // Verify it produces a consistent result
        let checksum2 = get_checksum1(data);
        assert_eq!(checksum, checksum2);
    }

    #[test]
    fn test_rolling_checksum_rolling() {
        let data = b"abcdef";
        let mut rolling = RollingChecksum::new(3);

        // Initialize with first 3 bytes: "abc"
        rolling.init(&data[0..3]);
        let initial = rolling.value();

        // Roll to next position: remove 'a', add 'd' -> "bcd"
        rolling.roll(data[0], data[3]);
        let rolled = rolling.value();

        // Should be different
        assert_ne!(initial, rolled);

        // Verify by computing fresh checksum for "bcd"
        let fresh = get_checksum1(&data[1..4]);
        assert_eq!(rolled, fresh);
    }
}
