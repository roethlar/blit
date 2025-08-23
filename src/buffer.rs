//! Smart buffer sizing for 10GbE saturation
//! Simplified from buffer_sizing.rs

use parking_lot::Mutex;

#[cfg(windows)]
use winapi::um::sysinfoapi::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

pub struct BufferSizer {
    max_buffer_size: usize,
    min_buffer_size: usize,
    cached_available_memory: Mutex<Option<u64>>,
}

impl BufferSizer {
    pub fn new() -> Self {
        // Optimized for 10GbE: large buffers to minimize syscalls
        BufferSizer {
            max_buffer_size: 16 * 1024 * 1024, // 16MB max
            min_buffer_size: 1024 * 1024,      // 1MB min
            cached_available_memory: Mutex::new(None),
        }
    }

    /// Get available memory on Windows
    #[cfg(windows)]
    fn get_available_memory() -> u64 {
        unsafe {
            let mut mem_status: MEMORYSTATUSEX = std::mem::zeroed();
            mem_status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;

            if GlobalMemoryStatusEx(&mut mem_status) != 0 {
                mem_status.ullAvailPhys
            } else {
                // Fallback to 4GB if API fails
                4 * 1024 * 1024 * 1024
            }
        }
    }

    #[cfg(not(windows))]
    fn get_available_memory() -> u64 {
        4 * 1024 * 1024 * 1024 // 4GB fallback for non-Windows
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

        // For network transfers, use larger buffers
        let base_size = if is_network {
            8 * 1024 * 1024 // 8MB for network
        } else {
            4 * 1024 * 1024 // 4MB for local
        };

        // Scale based on file size
        let optimal_size = if file_size < 10 * 1024 * 1024 {
            // Small file: smaller buffer
            self.min_buffer_size
        } else if file_size < 100 * 1024 * 1024 {
            // Medium file: medium buffer
            base_size
        } else {
            // Large file: max buffer
            self.max_buffer_size
        };

        // Ensure we don't use more than 10% of available memory
        let memory_limit = (available_memory / 10) as usize;
        optimal_size.min(memory_limit).max(self.min_buffer_size)
    }

    /// Get buffer size for parallel operations (smaller to avoid memory pressure)
    pub fn calculate_parallel_buffer_size(&self, thread_count: usize, is_network: bool) -> usize {
        let single_buffer = self.calculate_buffer_size(100 * 1024 * 1024, is_network);

        // Divide by thread count but maintain minimum
        (single_buffer / thread_count).max(256 * 1024) // 256KB minimum
    }
}
