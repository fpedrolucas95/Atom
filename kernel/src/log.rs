// Kernel Logging Subsystem
//
// Implements the Atom kernel’s structured logging framework, providing
// multi-level, timestamped log output for diagnostics, debugging, and
// crash analysis during development.
//
// Key responsibilities:
// - Provide standardized log levels (Debug, Info, Warn, Error, Panic)
// - Attach timestamps and subsystem origin to every log entry
// - Include source location only for DEBUG entries (file:line)
// - Output logs to the serial port unconditionally
// - Optionally mirror logs to the VGA text console with color coding
//
// Design principles:
// - Zero-cost filtering: log messages below the current level are dropped early
// - Early-boot friendly: works before full scheduler or user space exists
// - Deterministic output suitable for debugging kernel bring-up
// - Minimal formatting logic inside the hot path
//
// Implementation details:
// - Log level is stored in a global mutable variable (`CURRENT_LOG_LEVEL`)
// - Timestamps are derived from kernel timer ticks (coarse but monotonic)
// - Serial output is always enabled and considered the ground truth
// - VGA output is optional and guarded by a runtime flag
// - Each log includes severity, timestamp, subsystem origin, and message
//
// Developer ergonomics:
// - Convenience macros (`log_debug!`, `log_info!`, etc.) wrap `_log`
// - Macros automatically capture `file!()` and `line!()` for debug context
// - Color-coded VGA output improves readability during interactive debugging
//
// Correctness and safety notes:
// - Uses `unsafe` global state; assumes serialized access during early boot
// - Timestamp precision depends on interrupt timer configuration
// - VGA logging acquires a lock and should be avoided in critical paths
//
// Intended usage:
// - Kernel initialization tracing and subsystem bring-up
// - Debugging faults, IPC behavior, scheduling, and memory management
// - Panic-time diagnostics when the system cannot continue
//
// Future considerations:
// - Per-module log filtering
// - Structured log sinks (e.g. ring buffers or user-space log servers)
// - Runtime-configurable backends via user-space logging services

use core::fmt;
use core::fmt::Write;
use crate::serial;
use crate::vga::{self, Color};
use spin::Mutex;
use core::sync::atomic::{AtomicBool, Ordering};

/// Ring buffer for kernel log storage (accessible by userspace)
const LOG_BUFFER_SIZE: usize = 8192;
static LOG_BUFFER: Mutex<LogBuffer> = Mutex::new(LogBuffer::new());
/// Flag to track if heap is available for allocations
#[allow(dead_code)]
static HEAP_AVAILABLE: AtomicBool = AtomicBool::new(false);

struct LogBuffer {
    data: [u8; LOG_BUFFER_SIZE],
    write_pos: usize,
    len: usize,
}

impl LogBuffer {
    const fn new() -> Self {
        Self {
            data: [0u8; LOG_BUFFER_SIZE],
            write_pos: 0,
            len: 0,
        }
    }

    fn write_byte(&mut self, b: u8) {
        self.data[self.write_pos] = b;
        self.write_pos = (self.write_pos + 1) % LOG_BUFFER_SIZE;
        if self.len < LOG_BUFFER_SIZE {
            self.len += 1;
        }
    }

    fn read_all(&self) -> &[u8] {
        if self.len < LOG_BUFFER_SIZE {
            &self.data[..self.len]
        } else {
            &self.data[..]
        }
    }
}

impl Write for LogBuffer {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            self.write_byte(b);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
#[allow(dead_code)]
pub enum LogLevel {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
    Panic = 4,
}

impl LogLevel {
    pub const fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO ",
            LogLevel::Warn => "WARN ",
            LogLevel::Error => "ERROR",
            LogLevel::Panic => "PANIC",
        }
    }

    pub const fn color(&self) -> Color {
        match self {
            LogLevel::Debug => Color::DarkGray,
            LogLevel::Info => Color::White,
            LogLevel::Warn => Color::Yellow,
            LogLevel::Error => Color::LightRed,
            LogLevel::Panic => Color::Red,
        }
    }
}

static mut CURRENT_LOG_LEVEL: LogLevel = LogLevel::Debug;
static mut VGA_OUTPUT_ENABLED: bool = false;

pub fn init() {
    set_level(LogLevel::Info);
}

/// Mark that the heap is now available for allocations
/// Call this after heap initialization
#[allow(dead_code)]
pub fn set_heap_available() {
    HEAP_AVAILABLE.store(true, Ordering::Release);
}

pub fn set_level(level: LogLevel) {
    unsafe {
        CURRENT_LOG_LEVEL = level;
    }
}

pub fn get_level() -> LogLevel {
    unsafe { CURRENT_LOG_LEVEL }
}

pub fn enable_vga_output() {
    unsafe {
        VGA_OUTPUT_ENABLED = true;
    }
}

#[allow(dead_code)]
pub fn disable_vga_output() {
    unsafe {
        VGA_OUTPUT_ENABLED = false;
    }
}

fn get_timestamp_ms() -> u64 {
    let ticks = crate::interrupts::get_ticks();
    ticks * 10
}

fn format_timestamp(ms: u64) -> (u64, u64) {
    let seconds = ms / 1000;
    let milliseconds = ms % 1000;
    (seconds, milliseconds)
}

/// Read log buffer contents (for userspace)
/// Returns a copy of the log buffer data
pub fn read_log_buffer() -> alloc::vec::Vec<u8> {
    let buffer = LOG_BUFFER.lock();
    buffer.read_all().to_vec()
}

pub fn _log(level: LogLevel, origin: &str, args: fmt::Arguments, file: &str, line: u32) {
    if level < get_level() {
        return;
    }

    let timestamp_ms = get_timestamp_ms();
    let (seconds, milliseconds) = format_timestamp(timestamp_ms);

    let is_debug = level == LogLevel::Debug;

    let level_str = level.as_str();
    let args_for_vga = args.clone();

    // Write to ring buffer for userspace access (no allocation needed)
    {
        let mut buffer = LOG_BUFFER.lock();
        if is_debug {
            let _ = write!(
                buffer,
                "[{}.{:03}] {} [{}] {} ({}:{})\n",
                seconds, milliseconds, level_str, origin, args, file, line
            );
        } else {
            let _ = write!(
                buffer,
                "[{}.{:03}] {} [{}] {}\n",
                seconds, milliseconds, level_str, origin, args
            );
        }
    }

    if is_debug {
        serial::_print(format_args!(
            "[t={}.{:03}s] [{}] [{}] {} ({}:{})\n",
            seconds,
            milliseconds,
            level_str,
            origin,
            args,
            file,
            line
        ));
    } else {
        serial::_print(format_args!(
            "[t={}.{:03}s] [{}] [{}] {}\n",
            seconds,
            milliseconds,
            level_str,
            origin,
            args
        ));
    }

    unsafe {
        if VGA_OUTPUT_ENABLED {
            write_vga_log(
                seconds,
                milliseconds,
                level,
                origin,
                args_for_vga,
                file,
                line,
            );
        }
    }
}

unsafe fn write_vga_log(
    seconds: u64,
    milliseconds: u64,
    level: LogLevel,
    origin: &str,
    args: fmt::Arguments,
    file: &str,
    line: u32,
) {
    use core::fmt::Write;

    vga::write_colored(
        &alloc::format!("[t={}.{:03}s] ", seconds, milliseconds),
        Color::DarkGray,
        Color::Black,
    );

    vga::write_colored(
        &alloc::format!("[{}] ", level.as_str()),
        level.color(),
        Color::Black,
    );

    vga::write_colored(
        &alloc::format!("[{}] ", origin),
        Color::LightBlue,
        Color::Black,
    );

    let mut writer = vga::WRITER.lock();
    writer.set_color(Color::White, Color::Black);
    let _ = writer.write_fmt(args);

    if level == LogLevel::Debug {
        let _ = writer.write_fmt(format_args!(" ({}:{})", file, line));
    }

    writer.write_byte(b'\n');
}


#[macro_export]
macro_rules! log_debug {
    ($origin:expr, $($arg:tt)*) => {
        $crate::log::_log(
            $crate::log::LogLevel::Debug,
            $origin,
            format_args!($($arg)*),
            file!(),
            line!()
        )
    };
}

#[macro_export]
macro_rules! log_info {
    ($origin:expr, $($arg:tt)*) => {
        $crate::log::_log(
            $crate::log::LogLevel::Info,
            $origin,
            format_args!($($arg)*),
            file!(),
            line!()
        )
    };
}

#[macro_export]
macro_rules! log_warn {
    ($origin:expr, $($arg:tt)*) => {
        $crate::log::_log(
            $crate::log::LogLevel::Warn,
            $origin,
            format_args!($($arg)*),
            file!(),
            line!()
        )
    };
}

#[macro_export]
macro_rules! log_error {
    ($origin:expr, $($arg:tt)*) => {
        $crate::log::_log(
            $crate::log::LogLevel::Error,
            $origin,
            format_args!($($arg)*),
            file!(),
            line!()
        )
    };
}

#[macro_export]
macro_rules! log_panic {
    ($origin:expr, $($arg:tt)*) => {
        $crate::log::_log(
            $crate::log::LogLevel::Panic,
            $origin,
            format_args!($($arg)*),
            file!(),
            line!()
        )
    };
}