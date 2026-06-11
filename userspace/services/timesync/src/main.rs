#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use alloc::string::String;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicUsize, Ordering};

use atom_syscall::debug::log;
use atom_syscall::fs;
use atom_syscall::ipc::{create_port, try_recv, wait_any, PortId};
use atom_syscall::thread::{get_ticks, yield_now};
use libipc::messages::{
    MessageHeader, MessageType, TimeGetStateMsg, TimeSetConfigMsg, TimeStateReplyMsg, TIME_LOCALES,
    TIME_ZONES,
};
use libipc::protocol::{get_payload, register_service, send_message};

const HEAP_SIZE: usize = 256 * 1024;
const CONFIG_PATH: &str = "/user/config/date_time.cfg";
const SYNC_INTERVAL_TICKS: u64 = 6 * 60 * 60 * 100;
const RETRY_INTERVAL_TICKS: u64 = 60 * 100;

struct BumpAllocator {
    heap: UnsafeCell<[u8; HEAP_SIZE]>,
    next: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

impl BumpAllocator {
    const fn new() -> Self {
        Self {
            heap: UnsafeCell::new([0; HEAP_SIZE]),
            next: AtomicUsize::new(0),
        }
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let align = layout.align().max(16);
        loop {
            let current = self.next.load(Ordering::Relaxed);
            let aligned = (current + align - 1) & !(align - 1);
            let end = aligned.saturating_add(layout.size());
            if end > HEAP_SIZE {
                return core::ptr::null_mut();
            }
            if self
                .next
                .compare_exchange_weak(current, end, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return (self.heap.get() as *mut u8).add(aligned);
            }
        }
    }

    unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

#[alloc_error_handler]
fn alloc_error(_: Layout) -> ! {
    loop {
        yield_now();
    }
}

#[derive(Clone, Copy)]
struct TimeConfig {
    automatic: bool,
    format_24h: bool,
    locale_id: u8,
    timezone_id: u8,
}

impl TimeConfig {
    const fn default() -> Self {
        Self {
            automatic: true,
            format_24h: true,
            locale_id: 1,
            timezone_id: 1,
        }
    }

    fn load() -> Self {
        let mut config = Self::default();
        let Ok(bytes) = fs::read_file(CONFIG_PATH) else {
            return config;
        };
        let Ok(text) = core::str::from_utf8(&bytes) else {
            return config;
        };

        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "automatic" => config.automatic = value.trim() != "0",
                "format_24h" => config.format_24h = value.trim() != "0",
                "locale" => {
                    if let Some(v) = parse_u8(value.trim()) {
                        if (v as usize) < TIME_LOCALES.len() {
                            config.locale_id = v;
                        }
                    }
                }
                "timezone" => {
                    if let Some(v) = parse_u8(value.trim()) {
                        if (v as usize) < TIME_ZONES.len() {
                            config.timezone_id = v;
                        }
                    }
                }
                _ => {}
            }
        }
        config
    }

    fn save(&self) {
        let mut text = String::new();
        text.push_str("automatic=");
        text.push(if self.automatic { '1' } else { '0' });
        text.push_str("\nformat_24h=");
        text.push(if self.format_24h { '1' } else { '0' });
        text.push_str("\nlocale=");
        push_u8(&mut text, self.locale_id);
        text.push_str("\ntimezone=");
        push_u8(&mut text, self.timezone_id);
        text.push('\n');

        let _ = fs::mkdir_all("/user/config", 0o755);
        let _ = fs::write_file(CONFIG_PATH, text.as_bytes());
    }
}

struct TimeService {
    port: PortId,
    config: TimeConfig,
    unix_seconds: u64,
    reference_tick: u64,
    utc_offset_minutes: i32,
    synced: bool,
    syncing: bool,
    last_error: u8,
    next_sync_tick: u64,
}

impl TimeService {
    fn new(port: PortId) -> Self {
        Self {
            port,
            config: TimeConfig::load(),
            unix_seconds: 0,
            reference_tick: get_ticks(),
            utc_offset_minutes: 0,
            synced: false,
            syncing: false,
            last_error: 0,
            next_sync_tick: get_ticks(),
        }
    }

    fn state(&self) -> TimeStateReplyMsg {
        TimeStateReplyMsg {
            unix_seconds: self.unix_seconds,
            reference_tick: self.reference_tick,
            utc_offset_minutes: self.utc_offset_minutes,
            automatic: self.config.automatic,
            format_24h: self.config.format_24h,
            synced: self.synced,
            syncing: self.syncing,
            locale_id: self.config.locale_id,
            timezone_id: self.config.timezone_id,
            last_error: self.last_error,
        }
    }

    fn send_state(&self, reply_port: PortId) {
        let _ = send_message(
            reply_port,
            MessageType::TimeStateReply,
            &self.state().to_bytes(),
        );
    }

    fn sync_from_internet(&mut self) {
        self.syncing = true;
        self.last_error = 0;

        let netd = match libipc::protocol::lookup_service("netd") {
            Ok(port) => port,
            Err(_) => {
                self.sync_failed(1);
                return;
            }
        };

        let response = match libnet::http_get(netd, "example.com", "/", 80) {
            Ok(response) if response.status == 200 => response,
            _ => {
                self.sync_failed(2);
                return;
            }
        };

        let unix_seconds = match response.date.as_deref().and_then(parse_http_date) {
            Some(value) => value,
            None => {
                self.sync_failed(3);
                return;
            }
        };

        self.unix_seconds = unix_seconds;
        self.reference_tick = get_ticks();
        self.utc_offset_minutes = timezone_offset_minutes(self.config.timezone_id, unix_seconds);
        self.synced = true;
        self.syncing = false;
        self.last_error = 0;
        self.next_sync_tick = self.reference_tick.saturating_add(SYNC_INTERVAL_TICKS);
        log("timesync: internet time synchronized");
    }

    fn sync_failed(&mut self, error: u8) {
        self.syncing = false;
        self.last_error = error;
        self.next_sync_tick = get_ticks().saturating_add(RETRY_INTERVAL_TICKS);
        log(&alloc::format!(
            "timesync: internet synchronization failed (error={})",
            error
        ));
    }

    fn handle_message(&mut self, bytes: &[u8], len: usize) {
        if len < MessageHeader::SIZE {
            return;
        }
        let Some(header) = MessageHeader::from_bytes(bytes) else {
            return;
        };
        let payload = get_payload(bytes, len);
        match header.msg_type {
            MessageType::TimeGetState => {
                if let Some(msg) = TimeGetStateMsg::from_bytes(payload) {
                    self.send_state(msg.reply_port);
                }
            }
            MessageType::TimeSetConfig => {
                if let Some(msg) = TimeSetConfigMsg::from_bytes(payload) {
                    let timezone_changed = self.config.timezone_id != msg.timezone_id;
                    self.config = TimeConfig {
                        automatic: msg.automatic,
                        format_24h: msg.format_24h,
                        locale_id: msg.locale_id,
                        timezone_id: msg.timezone_id,
                    };
                    self.config.save();
                    if timezone_changed || (self.config.automatic && !self.synced) {
                        self.sync_from_internet();
                    }
                    self.send_state(msg.reply_port);
                }
            }
            MessageType::TimeSyncNow => {
                if let Some(msg) = TimeGetStateMsg::from_bytes(payload) {
                    self.sync_from_internet();
                    self.send_state(msg.reply_port);
                }
            }
            _ => {}
        }
    }

    fn run(&mut self) -> ! {
        let mut buffer = [0u8; 128];
        loop {
            while let Ok(Some(len)) = try_recv(self.port, &mut buffer) {
                self.handle_message(&buffer, len);
            }

            let now = get_ticks();
            if self.config.automatic && now >= self.next_sync_tick && !self.syncing {
                self.sync_from_internet();
            }

            let _ = wait_any(&[self.port], 1000);
        }
    }
}

fn parse_u8(text: &str) -> Option<u8> {
    let mut value = 0u8;
    if text.is_empty() {
        return None;
    }
    for byte in text.bytes() {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(byte - b'0')?;
    }
    Some(value)
}

fn push_u8(out: &mut String, value: u8) {
    if value >= 100 {
        out.push((b'0' + value / 100) as char);
    }
    if value >= 10 {
        out.push((b'0' + (value / 10) % 10) as char);
    }
    out.push((b'0' + value % 10) as char);
}

fn two_digits(value: &[u8]) -> Option<u8> {
    if value.len() != 2 || !value[0].is_ascii_digit() || !value[1].is_ascii_digit() {
        return None;
    }
    Some((value[0] - b'0') * 10 + value[1] - b'0')
}

fn parse_http_date(value: &str) -> Option<u64> {
    let mut parts = value.split_ascii_whitespace();
    let _weekday = parts.next()?;
    let day = parse_decimal(parts.next()?)? as u32;
    let month = match parts.next()? {
        "Jan" => 1,
        "Feb" => 2,
        "Mar" => 3,
        "Apr" => 4,
        "May" => 5,
        "Jun" => 6,
        "Jul" => 7,
        "Aug" => 8,
        "Sep" => 9,
        "Oct" => 10,
        "Nov" => 11,
        "Dec" => 12,
        _ => return None,
    };
    let year = parse_decimal(parts.next()?)? as i32;
    let time = parts.next()?.as_bytes();
    if time.len() != 8 || time[2] != b':' || time[5] != b':' {
        return None;
    }
    let hour = two_digits(&time[0..2])? as i64;
    let minute = two_digits(&time[3..5])? as i64;
    let second = two_digits(&time[6..8])? as i64;
    let days = days_from_civil(year, month, day);
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(hour * 3_600 + minute * 60 + second)?;
    u64::try_from(seconds).ok()
}

fn parse_decimal(value: &str) -> Option<u64> {
    let mut result = 0u64;
    for byte in value.bytes() {
        if !byte.is_ascii_digit() {
            return None;
        }
        result = result.checked_mul(10)?.checked_add((byte - b'0') as u64)?;
    }
    Some(result)
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let adjusted_year = year - i32::from(month <= 2);
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = month as i32 + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i32 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    (era * 146_097 + day_of_era - 719_468) as i64
}

fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096)
            / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year =
        day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year as i32, month as u32, day as u32)
}

fn weekday(year: i32, month: u32, day: u32) -> u32 {
    (days_from_civil(year, month, day) + 4).rem_euclid(7) as u32
}

fn nth_sunday(year: i32, month: u32, nth: u32) -> u32 {
    1 + (7 - weekday(year, month, 1)) % 7 + (nth - 1) * 7
}

fn last_sunday(year: i32, month: u32, last_day: u32) -> u32 {
    last_day - weekday(year, month, last_day)
}

fn transition_utc(year: i32, month: u32, day: u32, hour: u32) -> u64 {
    (days_from_civil(year, month, day) * 86_400 + hour as i64 * 3_600) as u64
}

fn timezone_offset_minutes(timezone_id: u8, unix_seconds: u64) -> i32 {
    let (year, _, _) = civil_from_days((unix_seconds / 86_400) as i64);
    match timezone_id {
        0 => 0,
        1 => -180,
        2 => {
            let start = transition_utc(year, 3, nth_sunday(year, 3, 2), 7);
            let end = transition_utc(year, 11, nth_sunday(year, 11, 1), 6);
            if unix_seconds >= start && unix_seconds < end { -240 } else { -300 }
        }
        3 => {
            let start = transition_utc(year, 3, nth_sunday(year, 3, 2), 10);
            let end = transition_utc(year, 11, nth_sunday(year, 11, 1), 9);
            if unix_seconds >= start && unix_seconds < end { -420 } else { -480 }
        }
        4 | 5 => {
            let start = transition_utc(year, 3, last_sunday(year, 3, 31), 1);
            let end = transition_utc(year, 10, last_sunday(year, 10, 31), 1);
            let standard = if timezone_id == 4 { 0 } else { 60 };
            if unix_seconds >= start && unix_seconds < end {
                standard + 60
            } else {
                standard
            }
        }
        6 => 540,
        7 => 330,
        8 => {
            let april_end =
                transition_utc(year, 4, nth_sunday(year, 4, 1), 16).saturating_sub(86_400);
            let october_start =
                transition_utc(year, 10, nth_sunday(year, 10, 1), 16).saturating_sub(86_400);
            if unix_seconds < april_end || unix_seconds >= october_start { 660 } else { 600 }
        }
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    let port = loop {
        match create_port() {
            Ok(port) => break port,
            Err(_) => yield_now(),
        }
    };
    while register_service("timesync", port).is_err() {
        yield_now();
    }
    log("timesync: service ready");
    TimeService::new(port).run();
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    log("timesync: PANIC");
    loop {
        yield_now();
    }
}
