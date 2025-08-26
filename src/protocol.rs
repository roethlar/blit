//! Shared protocol constants for RoboSync framed transport

// Protocol header constants
pub const MAGIC: &[u8; 4] = b"RSNC";
pub const VERSION: u16 = 1;

// Maximum frame payload size (64MB) - prevents DoS via memory exhaustion
// Using 64MB to accommodate large file chunks while preventing abuse
pub const MAX_FRAME_SIZE: usize = 64 * 1024 * 1024;

// Frame type IDs (keep numeric stable for compat with classic path)
pub mod frame {
    pub const START: u8 = 1;
    pub const OK: u8 = 2;
    pub const ERROR: u8 = 3;
    pub const FILE_START: u8 = 4;
    pub const FILE_DATA: u8 = 5;
    pub const FILE_END: u8 = 6;
    pub const DONE: u8 = 7;
    pub const TAR_START: u8 = 8;
    pub const TAR_DATA: u8 = 9;
    pub const TAR_END: u8 = 10;
    pub const PFILE_START: u8 = 11;
    pub const PFILE_DATA: u8 = 12;
    pub const PFILE_END: u8 = 13;
    pub const MANIFEST_START: u8 = 14;
    pub const MANIFEST_ENTRY: u8 = 15;
    pub const MANIFEST_END: u8 = 16;
    pub const NEED_LIST: u8 = 17;
    pub const SYMLINK: u8 = 18;
    pub const MKDIR: u8 = 19;
    pub const COMPRESSED_MANIFEST: u8 = 20;
    pub const DELTA_START: u8 = 21;
    pub const DELTA_SAMPLE: u8 = 22;
    pub const DELTA_END: u8 = 23;
    pub const NEED_RANGES_START: u8 = 24;
    pub const NEED_RANGE: u8 = 25;
    pub const NEED_RANGES_END: u8 = 26;
    pub const DELTA_DATA: u8 = 27;
    pub const DELTA_DONE: u8 = 28;
    pub const FILE_RAW_START: u8 = 29;
    pub const SET_ATTR: u8 = 30;
    pub const VERIFY_REQ: u8 = 31;
    pub const VERIFY_HASH: u8 = 32;
    pub const VERIFY_DONE: u8 = 33;  // Signals end of batch verification
    // Management frames
    pub const LIST_REQ: u8 = 40;
    pub const LIST_RESP: u8 = 41;
    pub const REMOVE_TREE_REQ: u8 = 42;
    pub const REMOVE_TREE_RESP: u8 = 43;
}

// Compression flags for START frame (bits 4-5 in flags byte)
pub mod compress_flags {
    pub const NONE: u8 = 0x00;
    pub const COMP_ZSTD: u8 = 0b00010000;  // Bit 4 set
    pub const COMP_LZ4: u8 = 0b00100000;   // Bit 5 set
    // Legacy values for TAR_START payload (single byte)
    pub const TAR_ZSTD: u8 = 0x01;
    pub const TAR_LZ4: u8 = 0x02;
}

// Centralized timeout constants for consistent behavior across async/legacy paths
pub mod timeouts {
    // Base timeout for frame header reads (ms)
    pub const FRAME_HEADER_MS: u64 = 300;
    
    // Base timeout for writes (ms)
    pub const WRITE_BASE_MS: u64 = 500;
    
    // Base timeout for reads (ms)
    pub const READ_BASE_MS: u64 = 300;
    
    // Additional timeout per MB of data (ms)
    pub const PER_MB_MS: u64 = 1;
    
    // Progress tick interval for UI updates (ms)
    pub const PROGRESS_TICK_MS: u64 = 250;
    
    // Connection establishment timeout (ms)
    pub const CONNECT_MS: u64 = 200;
    
    // Calculate write deadline based on payload size (ms)
    // 500ms base + 1ms per 1MB payload (ceil)
    pub fn write_deadline_ms(payload_len: usize) -> u64 {
        let mb = (payload_len as u64 + 1_048_575) / 1_048_576;
        WRITE_BASE_MS + mb * PER_MB_MS
    }
    
    // Calculate read deadline based on payload size (ms)
    // 300ms base + 1ms per 1MB payload (ceil)
    pub fn read_deadline_ms(payload_len: usize) -> u64 {
        let mb = (payload_len as u64 + 1_048_575) / 1_048_576;
        READ_BASE_MS + mb * PER_MB_MS
    }
}
