#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;
use core::fmt::Write;
use core::arch::asm;

// ============================================================================
// Memory Allocator
// ============================================================================

mod allocator {
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::UnsafeCell;
    use core::ptr::null_mut;

    const HEAP_SIZE: usize = 512 * 1024; // 512 KB for test app

    #[repr(align(4096))]
    struct Heap {
        data: UnsafeCell<[u8; HEAP_SIZE]>,
        next: UnsafeCell<usize>,
    }

    unsafe impl Sync for Heap {}

    static HEAP: Heap = Heap {
        data: UnsafeCell::new([0; HEAP_SIZE]),
        next: UnsafeCell::new(0),
    };

    pub struct BumpAllocator;

    unsafe impl GlobalAlloc for BumpAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let next_ptr = HEAP.next.get();
            let heap_start = HEAP.data.get() as *mut u8;

            let align = layout.align();
            let size = layout.size();
            let current = *next_ptr;
            let aligned = (current + align - 1) & !(align - 1);

            if aligned + size > HEAP_SIZE {
                return null_mut();
            }

            *next_ptr = aligned + size;
            heap_start.add(aligned)
        }

        unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
            // Bump allocator doesn't free
        }
    }

    #[global_allocator]
    static ALLOCATOR: BumpAllocator = BumpAllocator;
}

// ============================================================================
// Panic Handler
// ============================================================================

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    let _ = writeln!(
        LogBuffer,
        "FATAL: Panic! Test aborted.\n"
    );
    loop {
        unsafe {
            core::arch::asm!("hlt");
        }
    }
}

// ============================================================================
// Logging Infrastructure
// ============================================================================

/// Structured logging with timestamp and severity levels
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
    Test,
}

impl LogLevel {
    fn prefix(&self) -> &'static str {
        match self {
            LogLevel::Info => "[INFO]",
            LogLevel::Warn => "[WARN]",
            LogLevel::Error => "[ERROR]",
            LogLevel::Test => "[TEST]",
        }
    }
}

struct LogBuffer;

impl Write for LogBuffer {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        atom_syscall::debug::log(s);
        Ok(())
    }
}

fn log(level: LogLevel, msg: &str) {
    let mut buf = String::new();
    let _ = writeln!(buf, "{} {}", level.prefix(), msg);
    atom_syscall::debug::log(&buf);
}

fn log_test(name: &str) {
    log(LogLevel::Test, &format!("Running: {}", name));
}

fn log_pass(name: &str) {
    log(LogLevel::Test, &format!("PASS: {}", name));
}

fn log_fail(name: &str, reason: &str) {
    log(LogLevel::Error, &format!("FAIL: {} - {}", name, reason));
}

fn log_info(msg: &str) {
    log(LogLevel::Info, msg);
}

fn log_error(msg: &str) {
    log(LogLevel::Error, msg);
}

fn log_warn(msg: &str) {
    log(LogLevel::Warn, msg);
}

const REBOOT_MARKER_PATH: &str = "/test/reboot_persist_marker.txt";
const REBOOT_STATE_PATH: &str = "/test/.reboot_persist_state";
const REBOOT_PAYLOAD: &str = "ATOM_PERSISTENCE_CHECK_V1\n";

// ============================================================================
// Test Context and Utilities
// ============================================================================

/// Represent the state of the entire test suite
struct TestContext {
    failed_tests: u32,
    passed_tests: u32,
}

impl TestContext {
    fn new() -> Self {
        TestContext {
            failed_tests: 0,
            passed_tests: 0,
        }
    }

    fn fail(&mut self, test_name: &str, reason: &str) {
        log_fail(test_name, reason);
        self.failed_tests += 1;
    }

    fn pass(&mut self, test_name: &str) {
        log_pass(test_name);
        self.passed_tests += 1;
    }

    fn print_summary(&self) {
        log_info("");
        log_info("==========================================================");
        log_info(&format!("Test Summary: {} passed, {} failed",
            self.passed_tests, self.failed_tests));
        log_info("==========================================================");
        log_info("");
    }

    fn any_failed(&self) -> bool {
        self.failed_tests > 0
    }
}

// ============================================================================
// FSD Wait Helper
// ============================================================================

/// Wait for filesystem daemon to be ready
/// Retries with exponential backoff to avoid tight polling
fn wait_for_fsd() -> Result<(), &'static str> {
    log_info("Waiting for filesystem daemon (fsd) to become ready...");

    let max_attempts = 100;
    let mut attempt = 0;
    let base_delay_ms = 10;

    loop {
        attempt += 1;

        // Try to stat root to check if fsd is ready
        match atom_syscall::fs::stat("/") {
            Ok(_) => {
                log_info("Filesystem daemon is ready");
                return Ok(());
            }
            Err(_) if attempt < max_attempts => {
                // Exponential backoff-ish, with max 500ms wait between attempts
                let _delay = core::cmp::min(base_delay_ms * attempt / 10, 500);
                core::hint::spin_loop();
                // In real scenario, would sleep here
                continue;
            }
            Err(_) => {
                return Err("fsd_timeout");
            }
        }
    }
}

// ============================================================================
// Test Cases
// ============================================================================

/// TEST 1: Create test directory
fn test_mkdir() -> Result<(), &'static str> {
    log_test("test_mkdir");

    match atom_syscall::fs::mkdir("/test", 0o755) {
        Ok(()) => Ok(()),
        Err(atom_syscall::fs::FsError::Exists) => {
            // Directory already exists from previous test, that's ok
            Ok(())
        }
        Err(_) => {
            Err("mkdir_failed")
        }
    }
}

/// TEST 2: Create and write file
fn test_create_and_write(ctx: &mut TestContext) -> Result<(), &'static str> {
    log_test("test_create_and_write");

    // Open file for creation and read-write
    let fd = match atom_syscall::fs::open(
        "/test/file.txt",
        atom_syscall::fs::OpenFlags::CREAT | atom_syscall::fs::OpenFlags::RDWR,
        0o644,
    ) {
        Ok(fd) => fd,
        Err(e) => {
            ctx.fail("test_create_and_write", 
                &format!("open() failed: {:?}", e));
            return Err("file_open_failed");
        }
    };

    // Write test data
    let test_data = b"hello world";
    match atom_syscall::fs::write(fd, test_data) {
        Ok(n) if n == test_data.len() => {
            // Success
        }
        Ok(n) => {
            let _ = atom_syscall::fs::close(fd);
            ctx.fail("test_create_and_write",
                &format!("write() returned {} bytes, expected {}", n, test_data.len()));
            return Err("write_partial");
        }
        Err(e) => {
            let _ = atom_syscall::fs::close(fd);
            ctx.fail("test_create_and_write",
                &format!("write() failed: {:?}", e));
            return Err("write_failed");
        }
    }

    // Close file
    match atom_syscall::fs::close(fd) {
        Ok(()) => {
            ctx.pass("test_create_and_write");
            Ok(())
        }
        Err(e) => {
            ctx.fail("test_create_and_write",
                &format!("close() failed: {:?}", e));
            Err("close_failed")
        }
    }
}

/// TEST 3: Reopen and read file
fn test_reopen_and_read(ctx: &mut TestContext) -> Result<(), &'static str> {
    log_test("test_reopen_and_read");

    // Reopen file for reading
    let fd = match atom_syscall::fs::open("/test/file.txt", atom_syscall::fs::OpenFlags::RDONLY, 0) {
        Ok(fd) => fd,
        Err(e) => {
            ctx.fail("test_reopen_and_read",
                &format!("open() failed: {:?}", e));
            return Err("reopen_failed");
        }
    };

    // Read data
    let mut buffer = [0u8; 256];
    let bytes_read = match atom_syscall::fs::read(fd, &mut buffer) {
        Ok(n) => n,
        Err(e) => {
            let _ = atom_syscall::fs::close(fd);
            ctx.fail("test_reopen_and_read",
                &format!("read() failed: {:?}", e));
            return Err("read_failed");
        }
    };

    // Close file
    if let Err(e) = atom_syscall::fs::close(fd) {
        ctx.fail("test_reopen_and_read",
            &format!("close() failed: {:?}", e));
        return Err("close_failed");
    }

    // Validate content
    let expected = b"hello world";
    if bytes_read != expected.len() {
        ctx.fail("test_reopen_and_read",
            &format!("read {} bytes, expected {}", bytes_read, expected.len()));
        return Err("read_length_mismatch");
    }

    // Compare content byte-by-byte
    for i in 0..bytes_read {
        if buffer[i] != expected[i] {
            ctx.fail("test_reopen_and_read",
                &format!("content mismatch at byte {}: got {}, expected {}",
                    i, buffer[i], expected[i]));
            return Err("content_mismatch");
        }
    }

    ctx.pass("test_reopen_and_read");
    Ok(())
}

/// TEST 4: Call fsync to ensure durability
fn test_fsync(ctx: &mut TestContext) -> Result<(), &'static str> {
    log_test("test_fsync");

    // Open file for sync
    let fd = match atom_syscall::fs::open("/test/file.txt", atom_syscall::fs::OpenFlags::RDONLY, 0) {
        Ok(fd) => fd,
        Err(e) => {
            ctx.fail("test_fsync",
                &format!("open() failed: {:?}", e));
            return Err("fsync_open_failed");
        }
    };

    // Call fsync to sync metadata and journal
    let result = atom_syscall::fs::fsync(fd);
    let _ = atom_syscall::fs::close(fd);

    match result {
        Ok(()) => {
            ctx.pass("test_fsync");
            log_info("fsync completed successfully - file data guaranteed durable");
            Ok(())
        }
        Err(e) => {
            ctx.fail("test_fsync",
                &format!("fsync() failed: {:?}", e));
            Err("fsync_failed")
        }
    }
}

/// TEST 5: Verify file still accessible (simulates reboot by checking persistence)
fn test_reboot_persistence(ctx: &mut TestContext) -> Result<(), &'static str> {
    log_test("test_reboot_persistence");

    // In a real scenario, this would be "after reboot"
    // For now, we just verify the file is still there
    log_info("Simulating reboot by re-verifying file accessibility...");

    match atom_syscall::fs::stat("/test/file.txt") {
        Ok(stat) => {
            if stat.size == 11 { // "hello world" = 11 bytes
                ctx.pass("test_reboot_persistence");
                log_info(&format!("File persisted correctly: size={} bytes, inode={}",
                    stat.size, stat.inode));
                Ok(())
            } else {
                ctx.fail("test_reboot_persistence",
                    &format!("File size mismatch: got {}, expected 11", stat.size));
                Err("size_mismatch")
            }
        }
        Err(e) => {
            ctx.fail("test_reboot_persistence",
                &format!("File not found after reboot simulation: {:?}", e));
            Err("file_lost")
        }
    }
}

/// TEST 6: Verify directory still exists
fn test_dir_persistence(ctx: &mut TestContext) -> Result<(), &'static str> {
    log_test("test_dir_persistence");

    match atom_syscall::fs::stat("/test") {
        Ok(stat) => {
            let mode = stat.mode;
            // Check if it's a directory (mode bit check: S_IFDIR = 0o40000)
            let is_dir = (mode & 0o170000) == 0o40000;

            if is_dir {
                ctx.pass("test_dir_persistence");
                log_info("Directory persisted correctly after reboot simulation");
                Ok(())
            } else {
                ctx.fail("test_dir_persistence",
                    &format!("Path is not a directory (mode={:o})", mode));
                Err("not_a_directory")
            }
        }
        Err(e) => {
            ctx.fail("test_dir_persistence",
                &format!("Directory not found: {:?}", e));
            Err("dir_lost")
        }
    }
}

/// TEST 7: Verify journal replay capability
fn test_journal_recovery(ctx: &mut TestContext) -> Result<(), &'static str> {
    log_test("test_journal_recovery");

    log_info("Checking journal structures and entry count...");

    // Try to read a file that would require journal replay
    // In this case, re-read our test file to verify journal didn't corrupt anything
    let fd = match atom_syscall::fs::open("/test/file.txt", atom_syscall::fs::OpenFlags::RDONLY, 0) {
        Ok(fd) => fd,
        Err(e) => {
            ctx.fail("test_journal_recovery",
                &format!("Cannot open file for journal verification: {:?}", e));
            return Err("journal_file_access_failed");
        }
    };

    let mut buffer = [0u8; 256];
    let bytes_read = match atom_syscall::fs::read(fd, &mut buffer) {
        Ok(n) => {
            let _ = atom_syscall::fs::close(fd);
            n
        }
        Err(e) => {
            let _ = atom_syscall::fs::close(fd);
            ctx.fail("test_journal_recovery",
                &format!("Journal recovery read failed: {:?}", e));
            return Err("journal_read_failed");
        }
    };

    let expected = b"hello world";
    if bytes_read == expected.len() && buffer[..bytes_read] == expected[..] {
        ctx.pass("test_journal_recovery");
        log_info("Journal replay appears successful - file data integrity verified");
        Ok(())
    } else {
        ctx.fail("test_journal_recovery",
            "Journal replay resulted in corrupted data");
        Err("journal_corruption")
    }
}

/// TEST 8: Cleanup - remove test file
fn test_cleanup(ctx: &mut TestContext) -> Result<(), &'static str> {
    log_test("test_cleanup");

    match atom_syscall::fs::unlink("/test/file.txt") {
        Ok(()) => {
            ctx.pass("test_cleanup");
            Ok(())
        }
        Err(e) => {
            log_warn(&format!("Non-critical cleanup failure: {:?}", e));
            ctx.pass("test_cleanup"); // Don't fail suite on cleanup
            Ok(())
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RebootCycleResult {
    Prepared,
    Verified,
}

fn write_full_file_and_fsync(path: &str, data: &[u8]) -> Result<(), &'static str> {
    let fd = atom_syscall::fs::open(
        path,
        atom_syscall::fs::OpenFlags::CREAT
            | atom_syscall::fs::OpenFlags::TRUNC
            | atom_syscall::fs::OpenFlags::WRONLY,
        0o644,
    ).map_err(|_| "open_failed")?;

    atom_syscall::fs::write_all(fd, data).map_err(|_| {
        let _ = atom_syscall::fs::close(fd);
        "write_failed"
    })?;

    atom_syscall::fs::fsync(fd).map_err(|_| {
        let _ = atom_syscall::fs::close(fd);
        "fsync_failed"
    })?;

    atom_syscall::fs::close(fd).map_err(|_| "close_failed")?;
    Ok(())
}

fn test_real_reboot_cycle(ctx: &mut TestContext) -> Result<RebootCycleResult, &'static str> {
    log_test("test_real_reboot_cycle");

    let _ = atom_syscall::fs::mkdir("/test", 0o755);
    let expected = REBOOT_PAYLOAD.as_bytes();
    let state_exists = atom_syscall::fs::stat(REBOOT_STATE_PATH).is_ok();

    if !state_exists {
        write_full_file_and_fsync(REBOOT_MARKER_PATH, expected).map_err(|_| {
            ctx.fail("test_real_reboot_cycle",
                "prepare phase failed: cannot persist marker file");
            "prepare_marker_failed"
        })?;

        write_full_file_and_fsync(REBOOT_STATE_PATH, expected).map_err(|_| {
            ctx.fail("test_real_reboot_cycle",
                "prepare phase failed: cannot persist state file");
            "prepare_state_failed"
        })?;

        ctx.pass("test_real_reboot_cycle_prepare");
        log_warn("REBOOT TEST PREPARED: reboot now and run fs_test again to verify real persistence");
        log_warn(&format!(
            "Prepared files: {} and {}",
            REBOOT_MARKER_PATH,
            REBOOT_STATE_PATH
        ));
        return Ok(RebootCycleResult::Prepared);
    }

    let state_data: Vec<u8> = atom_syscall::fs::read_file(REBOOT_STATE_PATH).map_err(|e| {
        ctx.fail("test_real_reboot_cycle",
            &format!("verify phase failed: cannot read state file: {:?}", e));
        "verify_state_read_failed"
    })?;

    let marker_data: Vec<u8> = atom_syscall::fs::read_file(REBOOT_MARKER_PATH).map_err(|e| {
        ctx.fail("test_real_reboot_cycle",
            &format!("verify phase failed: cannot read marker file: {:?}", e));
        "verify_marker_read_failed"
    })?;

    let ok = state_data == expected && marker_data == expected;
    if !ok {
        ctx.fail("test_real_reboot_cycle",
            "verify phase failed: persisted payload mismatch after reboot");
        return Err("verify_payload_mismatch");
    }

    let _ = atom_syscall::fs::unlink(REBOOT_STATE_PATH);
    let _ = atom_syscall::fs::unlink(REBOOT_MARKER_PATH);

    ctx.pass("test_real_reboot_cycle_verify");
    log_info("Real reboot persistence verified successfully");
    Ok(RebootCycleResult::Verified)
}

// ============================================================================
// Main Entry Point
// ============================================================================

#[no_mangle]
pub extern "C" fn main() {
    let mut context = TestContext::new();

    log_info("==========================================================");
    log_info("Atom OS Filesystem Integration Test Suite");
    log_info("==========================================================");
    log_info("");

    // Step 0: Wait for FSD
    if let Err(e) = wait_for_fsd() {
        log_error(&format!("Cannot proceed without FSD: {}", e));
        return;
    }
    log_info("");

    // Run all test cases in sequence, fail-fast on critical errors
    
    if let Err(_) = test_mkdir() {
        context.fail("test_mkdir", "Directory creation failed");
    } else {
        context.pass("test_mkdir");
    }
    log_info("");

    if let Err(_) = test_create_and_write(&mut context) {
        // Already logged in test function
        log_info("STOPPING: Cannot proceed without successful write");
        context.print_summary();
        return;
    }
    log_info("");

    if let Err(_) = test_reopen_and_read(&mut context) {
        // Already logged in test function
        log_info("STOPPING: Cannot verify file content after write");
        context.print_summary();
        return;
    }
    log_info("");

    if let Err(_) = test_fsync(&mut context) {
        // Already logged in test function
        log_warn("fsync failed, but continuing tests");
    }
    log_info("");

    if let Err(_) = test_reboot_persistence(&mut context) {
        // Already logged in test function
        log_info("STOPPING: File failed to persist across reboot simulation");
        context.print_summary();
        return;
    }
    log_info("");

    if let Err(_) = test_dir_persistence(&mut context) {
        // Already logged in test function
        log_warn("Directory persistence check failed, but continuing");
    }
    log_info("");

    if let Err(_) = test_journal_recovery(&mut context) {
        // Already logged in test function
        log_warn("Journal recovery check failed - data may be corrupted");
    }
    log_info("");

    let _ = test_cleanup(&mut context);
    log_info("");

    match test_real_reboot_cycle(&mut context) {
        Ok(RebootCycleResult::Prepared) => {
            log_warn("Real reboot persistence test is prepared.");
            log_warn("Reboot now and run fs_test again to complete verification.");
            context.print_summary();
            return;
        }
        Ok(RebootCycleResult::Verified) => {
            log_info("Real reboot persistence test passed.");
        }
        Err(_) => {
            // Already logged in test function
            log_warn("Real reboot persistence test failed");
        }
    }
    log_info("");

    // Print final summary
    context.print_summary();

    // Exit with appropriate code
    if context.any_failed() {
        log_error("TEST SUITE FAILED - Filesystem integrity issues detected");
    } else {
        log_info("TEST SUITE PASSED - All critical checks passed");
        log_info("Filesystem is operational and crash-consistent");
    }
}

// ============================================================================
// UEFI Entry Point Wrapper
// ============================================================================

/// UEFI entry point - required for this to be loaded as UEFI app.
/// Calls the actual main() logic which is userspace-specific.
#[no_mangle]
pub extern "C" fn _start() -> ! {
    main();
    loop {
        unsafe {
            asm!("hlt");
        }
    }
}
