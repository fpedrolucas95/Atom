//! Shared Memory SPSC Ring Buffer
//!
//! Provides a lock-free Single-Producer Single-Consumer ring buffer
//! designed for zero-copy communication between userspace processes
//! via shared memory.

#![no_std]

use core::sync::atomic::{AtomicUsize, Ordering};

/// A fixed-size packet buffer entry in the ring
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RingEntry {
    /// Length of data in the buffer
    pub len: u16,
    /// Reserved/Flags
    pub flags: u16,
    /// Offset to data in the shared region (if not using inline data)
    pub offset: u32,
    /// Inline data for small packets
    pub data: [u8; 1528], // MTU sized
}

/// The ring header stored at the start of shared memory
#[repr(C)]
pub struct RingHeader {
    /// Read index (owned by consumer)
    pub head: AtomicUsize,
    /// Write index (owned by producer)
    pub tail: AtomicUsize,
    /// Number of entries in the ring
    pub capacity: usize,
}

pub struct RingProducer<'a> {
    header: &'a RingHeader,
    entries: &'a mut [RingEntry],
}

impl<'a> RingProducer<'a> {
    pub fn new(header: &'a RingHeader, entries: &'a mut [RingEntry]) -> Self {
        Self { header, entries }
    }

    pub fn can_push(&self) -> bool {
        let tail = self.header.tail.load(Ordering::Relaxed);
        let head = self.header.head.load(Ordering::Acquire);
        (tail + 1) % self.header.capacity != head
    }

    #[allow(clippy::result_unit_err)]
    pub fn push(&mut self, entry: RingEntry) -> Result<(), ()> {
        let tail = self.header.tail.load(Ordering::Relaxed);
        let head = self.header.head.load(Ordering::Acquire);

        if (tail + 1) % self.header.capacity == head {
            return Err(());
        }

        self.entries[tail] = entry;
        self.header
            .tail
            .store((tail + 1) % self.header.capacity, Ordering::Release);
        Ok(())
    }
}

pub struct RingConsumer<'a> {
    header: &'a RingHeader,
    entries: &'a [RingEntry],
}

impl<'a> RingConsumer<'a> {
    pub fn new(header: &'a RingHeader, entries: &'a [RingEntry]) -> Self {
        Self { header, entries }
    }

    pub fn can_pop(&self) -> bool {
        let head = self.header.head.load(Ordering::Relaxed);
        let tail = self.header.tail.load(Ordering::Acquire);
        head != tail
    }

    pub fn pop(&mut self) -> Option<RingEntry> {
        let head = self.header.head.load(Ordering::Relaxed);
        let tail = self.header.tail.load(Ordering::Acquire);

        if head == tail {
            return None;
        }

        let entry = self.entries[head];
        self.header
            .head
            .store((head + 1) % self.header.capacity, Ordering::Release);
        Some(entry)
    }
}
