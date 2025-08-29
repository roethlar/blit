//! Smart buffer sizing for 10GbE saturation (minimal, used APIs only)

use parking_lot::Mutex;

pub struct BufferSizer {
    max_buffer_size: usize,
    min_buffer_size: usize,
    cached_available_memory: Mutex<Option<u64>>,
}

impl BufferSizer {
    pub fn new() -> Self {
        BufferSizer {
            max_buffer_size: 16 * 1024 * 1024, // 16MB max
            min_buffer_size: 1024 * 1024,      // 1MB min
            cached_available_memory: Mutex::new(None),
        }
    }

    /// Get available memory using sysinfo
    fn get_available_memory() -> u64 {
        use sysinfo::System;
        let mut sys = System::new_all();
        sys.refresh_memory();
        sys.available_memory().max(4 * 1024 * 1024 * 1024)
    }

    /// Calculate optimal buffer size based on file size and available memory
    pub fn calculate_buffer_size(&self, file_size: u64, is_network: bool) -> usize {
        // Get or cache available memory
        let available_memory = {
            let mut cached = self.cached_available_memory.lock();
            if let Some(mem) = *cached {
                mem
            } else {
                let mem = Self::get_available_memory();
                *cached = Some(mem);
                mem
            }
        };

        // Base size: bigger for network
        let base_size = if is_network { 8 * 1024 * 1024 } else { 4 * 1024 * 1024 };

        // Scale based on file size
        let optimal_size = if file_size < 10 * 1024 * 1024 {
            self.min_buffer_size
        } else if file_size <= 100 * 1024 * 1024 {
            base_size
        } else {
            base_size.max(self.max_buffer_size)
        };

        // Cap to 10% of available memory, enforce minimum
        let memory_limit = (available_memory / 10) as usize;
        optimal_size.min(memory_limit).max(self.min_buffer_size)
    }
}

impl Default for BufferSizer {
    fn default() -> Self { Self::new() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_detection() {
        let mem = BufferSizer::get_available_memory();
        assert!(mem >= 4 * 1024 * 1024 * 1024);
    }

    #[test]
    fn test_buffer_sizing_with_real_memory() {
        let sizer = BufferSizer::new();
        let net_buf = sizer.calculate_buffer_size(1024 * 1024 * 1024, true);
        assert!(net_buf >= 256 * 1024);
        assert!(net_buf <= 64 * 1024 * 1024);
        let local_buf = sizer.calculate_buffer_size(100 * 1024 * 1024, false);
        assert!(local_buf >= 64 * 1024);
        assert!(local_buf <= 8 * 1024 * 1024);
    }
}
