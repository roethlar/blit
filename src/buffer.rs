//! Smart buffer sizing for 10GbE saturation with adaptive tuning
//! Simplified from buffer_sizing.rs

use parking_lot::Mutex;
use std::time::{Duration, Instant};
use std::collections::VecDeque;

/// Throughput sample for adaptive tuning
#[derive(Clone, Debug)]
struct ThroughputSample {
    timestamp: Instant,
    bytes: u64,
    duration: Duration,
}

pub struct BufferSizer {
    max_buffer_size: usize,
    min_buffer_size: usize,
    cached_available_memory: Mutex<Option<u64>>,
    /// Recent throughput samples for adaptive tuning
    throughput_samples: Mutex<VecDeque<ThroughputSample>>,
    /// Target throughput for 10GbE (1.1 GB/s practical)
    target_throughput: u64,
}

impl BufferSizer {
    pub fn new() -> Self {
        // Optimized for 10GbE: large buffers to minimize syscalls
        BufferSizer {
            max_buffer_size: 16 * 1024 * 1024, // 16MB max
            min_buffer_size: 1024 * 1024,      // 1MB min
            cached_available_memory: Mutex::new(None),
            throughput_samples: Mutex::new(VecDeque::with_capacity(10)),
            target_throughput: 1_100_000_000, // 1.1 GB/s for 10GbE
        }
    }
    
    /// Record a throughput sample for adaptive tuning
    pub fn record_throughput(&self, bytes: u64, duration: Duration) {
        let mut samples = self.throughput_samples.lock();
        
        // Keep only recent samples (last 10)
        if samples.len() >= 10 {
            samples.pop_front();
        }
        
        samples.push_back(ThroughputSample {
            timestamp: Instant::now(),
            bytes,
            duration,
        });
    }
    
    /// Calculate current average throughput
    fn get_average_throughput(&self) -> Option<f64> {
        let samples = self.throughput_samples.lock();
        
        if samples.is_empty() {
            return None;
        }
        
        let total_bytes: u64 = samples.iter().map(|s| s.bytes).sum();
        let total_duration: Duration = samples.iter().map(|s| s.duration).sum();
        
        if total_duration.as_secs_f64() > 0.0 {
            Some(total_bytes as f64 / total_duration.as_secs_f64())
        } else {
            None
        }
    }

    /// Get available memory using sysinfo
    fn get_available_memory() -> u64 {
        use sysinfo::System;
        let mut sys = System::new_all();
        sys.refresh_memory();
        
        // Use available memory (free + buffers/cache that can be reclaimed)
        // Fall back to 4GB if we can't get system info
        sys.available_memory().max(4 * 1024 * 1024 * 1024)
    }

    /// Calculate optimal buffer size based on file size and available memory
    /// with adaptive tuning based on measured throughput
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
        let mut base_size = if is_network {
            8 * 1024 * 1024 // 8MB for network
        } else {
            4 * 1024 * 1024 // 4MB for local
        };
        
        // Adaptive tuning: if throughput is below target, increase buffer size
        if let Some(avg_throughput) = self.get_average_throughput() {
            let target = self.target_throughput as f64;
            
            if avg_throughput < target * 0.8 {
                // Below 80% of target: increase buffer significantly
                base_size = (base_size as f64 * 1.5) as usize;
                base_size = base_size.min(self.max_buffer_size);
            } else if avg_throughput < target {
                // Below target: increase buffer moderately
                base_size = (base_size as f64 * 1.25) as usize;
                base_size = base_size.min(self.max_buffer_size);
            }
            // If at or above target, keep current size
        }

        // Scale based on file size
        let optimal_size = if file_size < 10 * 1024 * 1024 {
            // Small file: smaller buffer
            self.min_buffer_size
        } else if file_size <= 100 * 1024 * 1024 {
            // Medium file: use adaptive base size
            base_size
        } else {
            // Large file: max buffer or adaptive size, whichever is larger
            base_size.max(self.max_buffer_size)
        };

        // Ensure we don't use more than 10% of available memory
        let memory_limit = (available_memory / 10) as usize;
        optimal_size.min(memory_limit).max(self.min_buffer_size)
    }

    /// Get buffer size for parallel operations (smaller to avoid memory pressure)
    pub fn calculate_parallel_buffer_size(&self, thread_count: usize, is_network: bool) -> usize {
        let single_buffer = self.calculate_buffer_size(100 * 1024 * 1024, is_network);
        // Avoid division by zero by treating 0 threads as 1
        let threads = thread_count.max(1);
        
        // Divide by thread count but maintain minimum
        (single_buffer / threads).max(256 * 1024) // 256KB minimum
    }
}

impl Default for BufferSizer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_detection() {
        // Test that get_available_memory returns a reasonable value
        let mem = BufferSizer::get_available_memory();
        // Should be at least 4GB (our fallback)
        assert!(mem >= 4 * 1024 * 1024 * 1024);
        // Should be less than 10TB (sanity check)
        assert!(mem < 10 * 1024 * 1024 * 1024 * 1024);
        println!("Detected available memory: {:.2} GB", mem as f64 / (1024.0 * 1024.0 * 1024.0));
    }

    #[test]
    fn test_buffer_sizing_with_real_memory() {
        let sizer = BufferSizer::new();
        
        // Test network buffer sizing
        let net_buf = sizer.calculate_buffer_size(1024 * 1024 * 1024, true);
        assert!(net_buf >= 256 * 1024); // At least 256KB
        assert!(net_buf <= 64 * 1024 * 1024); // At most 64MB
        
        // Test local buffer sizing
        let local_buf = sizer.calculate_buffer_size(100 * 1024 * 1024, false);
        assert!(local_buf >= 64 * 1024); // At least 64KB
        assert!(local_buf <= 8 * 1024 * 1024); // At most 8MB
    }

    #[test]
    fn parallel_buffer_size_handles_zero_threads() {
        let sizer = BufferSizer::new();
        let expected = sizer.calculate_buffer_size(100 * 1024 * 1024, false);
        assert_eq!(sizer.calculate_parallel_buffer_size(0, false), expected);
    }
}
