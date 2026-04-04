# Audit Log Configuration

The capability system's audit log tracks all capability lifecycle events (create, derive, transfer, revoke) for security auditing and debugging. The audit log size is configurable at compile time to balance between audit history retention and memory usage.

## Default Configuration

By default, the audit log retains **1000 entries**, which uses approximately **64 KB** of memory.

## Configuration Options

### Method 1: Cargo Features (Recommended)

Add one of the following features to your build command:

```bash
# Low memory systems (100 entries ≈ 6.4 KB)
cargo build --features audit_log_entries_100

# Constrained systems (500 entries ≈ 32 KB)
cargo build --features audit_log_entries_500

# High security systems (5000 entries ≈ 320 KB)
cargo build --features audit_log_entries_5000

# Development/debugging (10000 entries ≈ 640 KB)
cargo build --features audit_log_entries_10000
```

### Method 2: Direct Source Modification

Alternatively, you can modify the constant directly in `kernel/src/cap.rs`:

```rust
const MAX_AUDIT_LOG_ENTRIES: usize = 5000;  // Change this value
```

## Tuning Guidelines

Choose the appropriate size based on your deployment scenario:

| Scenario | Recommended Size | Memory Usage | Rationale |
|----------|-----------------|--------------|-----------|
| **Embedded/IoT** | 100-500 | 6-32 KB | Minimal memory footprint |
| **Standard Desktop** | 1000 (default) | 64 KB | Balanced history and memory |
| **Server/High Security** | 5000-10000 | 320-640 KB | Extended audit trail |
| **Development/Debug** | 10000+ | 640+ KB | Maximum visibility |

## Memory Impact

Each audit entry is approximately **64 bytes**, consisting of:
- Timestamp (8 bytes)
- Event type (1 byte + padding)
- Thread ID (8 bytes)
- Capability handle (8 bytes)
- Optional parent handle (16 bytes)
- Optional target thread (16 bytes)

Total memory usage = `MAX_AUDIT_LOG_ENTRIES × 64 bytes`

## Behavior

When the audit log reaches the configured maximum:
1. The oldest entry is evicted (FIFO policy)
2. A debug message is logged indicating eviction
3. The eviction counter is incremented
4. The new entry is added

You can query audit statistics using:
```rust
let stats = get_audit_stats();
println!("Audit log: {} entries, {} evictions", stats.size, stats.eviction_count);
```

## Security Considerations

- **Larger logs** provide longer audit trails but consume more memory
- **Smaller logs** reduce memory usage but may lose historical events
- Evicted entries are permanently lost (not persisted to disk)
- Consider your security requirements when choosing the size

## Performance Impact

The audit log uses a `VecDeque` with O(1) push/pop operations. Performance impact is minimal:
- Adding an entry: ~10-20 CPU cycles
- Evicting an entry: ~5-10 CPU cycles
- No impact on capability operations themselves

## Example Usage

### Building for a high-security server:
```bash
cd kernel
cargo build --release --features audit_log_entries_10000
```

### Building for an embedded system:
```bash
cd kernel
cargo build --release --features audit_log_entries_100
```

### Querying audit log at runtime:
```rust
// Get last 100 audit entries
let entries = get_audit_log(100);
for entry in entries {
    println!("{:?}", entry);
}

// Get audit statistics
let stats = get_audit_stats();
println!("Audit log size: {}/{}", stats.size, stats.max_entries);
println!("Total evictions: {}", stats.eviction_count);
```

## Related Files

- `kernel/src/cap.rs` - Audit log implementation
- `kernel/Cargo.toml` - Feature definitions
- Requirements: Req 4.5 (Audit log configuration)
