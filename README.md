# RoboSync v2.1 🚀

**High-performance file synchronization with robocopy-style CLI**

RoboSync v2.1 is a cross-platform file synchronization tool built for speed and simplicity. It provides robocopy-compatible command line options while delivering consistently faster performance than rsync.

## 🎯 Key Features

- **⚡ High Performance**: Consistently 2-6× faster than rsync across different workloads
- **🎮 Robocopy CLI**: Familiar command syntax with `--mir`, `-l`, `--xf`, etc.
- **📊 Simple Progress**: Lightweight activity indicator without performance impact
- **🧠 Smart Strategy**: Tar streaming for small files, parallel copy for medium, chunked for large
- **🌍 Cross-Platform**: Native support for Linux, macOS, and Windows
- **🔒 Reliable**: Proper empty directory handling, mirror mode, file filtering

## 🚀 Quick Start

### Installation

```bash
# Build from source
cargo build --release

# Binary will be at: target/release/robosync
```

### Basic Usage

```bash
# Simple copy
robosync /source /destination

# Mirror mode (most common) - copy and delete extra files
robosync /source /dest --mir

# Dry run to see what would happen
robosync /source /dest --mir -l

# Exclude files and directories
robosync /source /dest --xf "*.tmp" --xd "node_modules" --xd ".git"

# Skip empty directories (robocopy /S style)
robosync /source /dest -S
```

## 📊 Performance Benchmarks

**Linux Performance vs rsync:**

| Test Case | RoboSync v2.1 | rsync | Improvement |
|-----------|---------------|-------|-------------|
| Small files (1000 × 13B) | 0.019s | 0.052s | **2.7× faster** |
| Medium files (50 × 1MB) | 0.010s | 0.061s | **6.1× faster** |
| Large files (5 × 100MB) | 0.087s | 0.192s | **2.2× faster** |

*Performance varies by hardware, filesystem, and file characteristics*

## 🎛️ Command Reference

### Essential Options

| Option | Description | Robocopy Equivalent |
|--------|-------------|-------------------|
| `--mir` | Mirror directory (copy + delete extra) | `/MIR` |
| `--delete` | Delete extra files in destination | `/PURGE` |
| `-l` | Dry run - list only, don't copy | `/L` |
| `-c` | Use checksums instead of size+timestamp | - |
| `-v` | Verbose output | `/V` |
| `-p` | Show progress display | - |

### Directory Handling

| Option | Description | Robocopy Equivalent |
|--------|-------------|-------------------|
| `-e` | Copy subdirectories including empty ones (default) | `/E` |
| `-s` | Copy subdirectories but not empty ones | `/S` |

### File Selection

| Option | Description | Robocopy Equivalent |
|--------|-------------|-------------------|
| `--xf <pattern>` | Exclude files matching pattern | `/XF` |
| `--xd <pattern>` | Exclude directories matching pattern | `/XD` |

### Advanced Options

| Option | Description |
|--------|-------------|
| `-t <n>` | Number of threads (0 = auto) |
| `--force-tar` | Force tar streaming for small files |
| `--no-tar` | Disable tar streaming |
| `-r <n>` | Retry count on failures (default: 3) |
| `-w <n>` | Wait seconds between retries (default: 1) |

## 🎨 Progress Display

RoboSync v2.1 features a simple, lightweight progress indicator:

```
RoboSync v2.1... found 316646, copying... comparing... done!
```

Provides immediate feedback that the tool is working without any performance impact. Use `-v` for detailed verbose output.

## 🏗️ Architecture

RoboSync uses intelligent strategy selection based on file characteristics:

- **Small files (<1MB)**: Tar streaming for reduced syscall overhead
- **Medium files (1-100MB)**: Parallel copy with optimal thread distribution  
- **Large files (>100MB)**: Chunked copy with memory-mapped I/O
- **Empty directories**: Proper creation with `-E`/`-S` flag support
- **Mirror mode**: Efficient detection and removal of extra files

## 🌍 Platform Support

### Linux 🐧
- Optimized for ext4, xfs, btrfs filesystems
- High-performance parallel I/O
- Memory-mapped file operations

### macOS 🍎  
- APFS optimizations
- Extended attribute preservation
- Conservative threading for stability

### Windows 🪟
- Native Win32 API integration
- NTFS optimizations
- Fast directory enumeration

## 📝 Examples

### Backup with exclusions
```bash
robosync ~/Documents/ /backup/docs/ \
  --mirror \
  --xd ".cache" --xd "node_modules" \
  --xf "*.tmp" --xf ".DS_Store" \
  -v
```

### Server synchronization
```bash
robosync /var/www/ /mnt/backup/www/ \
  --mirror \
  -v -p
```

### Preview changes (dry run)
```bash
robosync /source /dest --mir -l -v
```

## 🔧 Building

```bash
# Debug build
cargo build

# Optimized release build
cargo build --release

# Run tests
cargo test

# Lint
cargo clippy
```

## 📜 License

MIT License - see LICENSE file for details.

---

**Ready to sync at robocopy speed?** 🚀

RoboSync v2.1 delivers the familiar robocopy experience with consistently faster performance across all platforms.