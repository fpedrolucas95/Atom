//! NIC Driver — Intel e1000 (82540EM) userspace driver
//!
//! Discovers its device via SYS_PCI_QUERY_DEVICE (the kernel passes all
//! class-0x02 DeviceCaps without knowing which driver handles which NIC).
//! Supports the Intel e1000 (vendor=0x8086, device=0x100E) used by QEMU's
//! default `-device e1000` configuration.

#![no_std]
#![no_main]

extern crate alloc;

use core::panic::PanicInfo;
use atom_syscall::debug::log;
use libipc::protocol::{register_service, lookup_service, send_message, try_recv_message, get_payload};
use libipc::messages::{MessageType, NetAssignRingsMsg, NetDriverReadyMsg};
use libring::{RingHeader, RingEntry, RingProducer, RingConsumer};

mod allocator {
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::UnsafeCell;
    use core::ptr::null_mut;

    const HEAP_SIZE: usize = 512 * 1024;

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
            let next = HEAP.next.get();
            let heap_start = HEAP.data.get() as *mut u8;
            let current = *next;
            let aligned = (current + layout.align() - 1) & !(layout.align() - 1);
            if aligned + layout.size() > HEAP_SIZE { return null_mut(); }
            *next = aligned + layout.size();
            heap_start.add(aligned)
        }
        unsafe fn dealloc(&self, _: *mut u8, _: Layout) {}
    }

    #[global_allocator]
    static ALLOCATOR: BumpAllocator = BumpAllocator;
}

// ============================================================================
// e1000 Register Offsets (MMIO, 32-bit aligned)
// ============================================================================

const E1000_CTRL:    u32 = 0x0000; // Device Control
const E1000_STATUS:  u32 = 0x0008; // Device Status
const E1000_EECD:    u32 = 0x0010; // EEPROM/Flash Control
const E1000_EERD:    u32 = 0x0014; // EEPROM Read
const E1000_ICR:     u32 = 0x00C0; // Interrupt Cause Read (clears on read)
const E1000_IMS:     u32 = 0x00D0; // Interrupt Mask Set
const E1000_IMC:     u32 = 0x00D8; // Interrupt Mask Clear
const E1000_RCTL:    u32 = 0x0100; // Receive Control
const E1000_TCTL:    u32 = 0x0400; // Transmit Control
const E1000_TIPG:    u32 = 0x0410; // Transmit IPG
const E1000_RDBAL:   u32 = 0x2800; // RX Descriptor Base Low
const E1000_RDBAH:   u32 = 0x2804; // RX Descriptor Base High
const E1000_RDLEN:   u32 = 0x2808; // RX Descriptor Length
const E1000_RDH:     u32 = 0x2810; // RX Descriptor Head
const E1000_RDT:     u32 = 0x2818; // RX Descriptor Tail
const E1000_TDBAL:   u32 = 0x3800; // TX Descriptor Base Low
const E1000_TDBAH:   u32 = 0x3804; // TX Descriptor Base High
const E1000_TDLEN:   u32 = 0x3808; // TX Descriptor Length
const E1000_TDH:     u32 = 0x3810; // TX Descriptor Head
const E1000_TDT:     u32 = 0x3818; // TX Descriptor Tail
const E1000_MTA:     u32 = 0x5200; // Multicast Table Array (128 x u32)
const E1000_RAL:     u32 = 0x5400; // Receive Address Low
const E1000_RAH:     u32 = 0x5404; // Receive Address High

// CTRL bits
const CTRL_RST:      u32 = 1 << 26; // Full reset
const CTRL_SLU:      u32 = 1 << 6;  // Set Link Up
const CTRL_ASDE:     u32 = 1 << 5;  // Auto-Speed Detection Enable

// RCTL bits
const RCTL_EN:       u32 = 1 << 1;
const RCTL_SBP:      u32 = 1 << 2;
const RCTL_UPE:      u32 = 1 << 3;  // Unicast Promiscuous
const RCTL_MPE:      u32 = 1 << 4;  // Multicast Promiscuous
const RCTL_BAM:      u32 = 1 << 15; // Broadcast Accept
const RCTL_BSIZE_2K: u32 = 0 << 16; // Buffer size 2048 (default)
const RCTL_SECRC:    u32 = 1 << 26; // Strip Ethernet CRC

// TCTL bits
const TCTL_EN:       u32 = 1 << 1;
const TCTL_PSP:      u32 = 1 << 3;  // Pad Short Packets
const TCTL_CT_SHIFT: u32 = 4;
const TCTL_COLD_SHIFT: u32 = 12;

// TX descriptor command bits
const TDESC_CMD_EOP:  u8 = 1 << 0; // End of Packet
const TDESC_CMD_IFCS: u8 = 1 << 1; // Insert FCS
const TDESC_CMD_RS:   u8 = 1 << 3; // Report Status

// TX descriptor status bits
const TDESC_STA_DD: u8 = 1 << 0; // Descriptor Done

// RX descriptor status bits
const RDESC_STA_DD:  u8 = 1 << 0; // Descriptor Done
const RDESC_STA_EOP: u8 = 1 << 1; // End of Packet

// Interrupt bits
const ICR_TXDW:  u32 = 1 << 0; // TX Descriptor Written Back
const ICR_RXT0:  u32 = 1 << 7; // RX Timer Interrupt

const NUM_TX_DESC: usize = 8;
const NUM_RX_DESC: usize = 8;
const RX_BUF_SIZE: usize = 2048;

// ============================================================================
// Descriptor Layouts (must be 16-byte aligned)
// ============================================================================

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct TxDesc {
    addr:    u64,
    length:  u16,
    cso:     u8,
    cmd:     u8,
    status:  u8,
    css:     u8,
    special: u16,
}

#[repr(C, align(16))]
#[derive(Clone, Copy)]
struct RxDesc {
    addr:    u64,
    length:  u16,
    checksum: u16,
    status:  u8,
    errors:  u8,
    special: u16,
}

// ============================================================================
// e1000 Driver State
// ============================================================================

struct E1000 {
    mmio: u64,          // userspace VA of MMIO BAR0
    mac:  [u8; 6],
    tx_descs: *mut TxDesc,
    tx_bufs_va:  *mut u8,
    tx_bufs_phys: u64,
    tx_tail:  usize,
    rx_descs: *mut RxDesc,
    rx_bufs:  *mut u8,
    rx_tail:  usize,
}

impl E1000 {
    #[inline]
    fn read32(&self, reg: u32) -> u32 {
        unsafe { core::ptr::read_volatile((self.mmio + reg as u64) as *const u32) }
    }

    #[inline]
    fn write32(&self, reg: u32, val: u32) {
        unsafe { core::ptr::write_volatile((self.mmio + reg as u64) as *mut u32, val) }
    }

    fn read_eeprom(&self, offset: u8) -> u16 {
        self.write32(E1000_EERD, 1 | ((offset as u32) << 8));
        loop {
            let v = self.read32(E1000_EERD);
            if v & (1 << 4) != 0 {
                return (v >> 16) as u16;
            }
            core::hint::spin_loop();
        }
    }

    fn read_mac(&mut self) {
        // Try EEPROM first (word 0 = bytes 0-1, word 1 = bytes 2-3, word 2 = bytes 4-5)
        let w0 = self.read_eeprom(0);
        let w1 = self.read_eeprom(1);
        let w2 = self.read_eeprom(2);
        self.mac = [
            (w0 & 0xFF) as u8, (w0 >> 8) as u8,
            (w1 & 0xFF) as u8, (w1 >> 8) as u8,
            (w2 & 0xFF) as u8, (w2 >> 8) as u8,
        ];
    }

    fn init(&mut self, tx_phys: u64, rx_phys: u64, rx_buf_phys: u64) {
        // Reset
        self.write32(E1000_CTRL, self.read32(E1000_CTRL) | CTRL_RST);
        for _ in 0..10000 { core::hint::spin_loop(); }

        // Set link up, auto-speed
        self.write32(E1000_CTRL, CTRL_SLU | CTRL_ASDE);

        // Clear multicast table
        for i in 0..128u32 {
            self.write32(E1000_MTA + i * 4, 0);
        }

        // Read MAC
        self.read_mac();

        // Program MAC into receive address register
        let ral = (self.mac[0] as u32)
            | ((self.mac[1] as u32) << 8)
            | ((self.mac[2] as u32) << 16)
            | ((self.mac[3] as u32) << 24);
        let rah = (self.mac[4] as u32)
            | ((self.mac[5] as u32) << 8)
            | (1 << 31); // Address Valid
        self.write32(E1000_RAL, ral);
        self.write32(E1000_RAH, rah);

        // TX ring
        self.write32(E1000_TDBAL, (tx_phys & 0xFFFF_FFFF) as u32);
        self.write32(E1000_TDBAH, (tx_phys >> 32) as u32);
        self.write32(E1000_TDLEN, (NUM_TX_DESC * 16) as u32);
        self.write32(E1000_TDH, 0);
        self.write32(E1000_TDT, 0);
        self.write32(E1000_TCTL,
            TCTL_EN | TCTL_PSP | (0x10 << TCTL_CT_SHIFT) | (0x40 << TCTL_COLD_SHIFT));
        self.write32(E1000_TIPG, 0x0060200A); // standard IPG for 802.3

        // RX ring — point each descriptor at its buffer
        for i in 0..NUM_RX_DESC {
            let desc = unsafe { &mut *self.rx_descs.add(i) };
            desc.addr = rx_buf_phys + (i * RX_BUF_SIZE) as u64;
            desc.status = 0;
        }
        self.write32(E1000_RDBAL, (rx_phys & 0xFFFF_FFFF) as u32);
        self.write32(E1000_RDBAH, (rx_phys >> 32) as u32);
        self.write32(E1000_RDLEN, (NUM_RX_DESC * 16) as u32);
        self.write32(E1000_RDH, 0);
        self.rx_tail = NUM_RX_DESC - 1;
        self.write32(E1000_RDT, self.rx_tail as u32);
        self.write32(E1000_RCTL,
            RCTL_EN | RCTL_SBP | RCTL_UPE | RCTL_MPE | RCTL_BAM | RCTL_BSIZE_2K | RCTL_SECRC);

        // Enable RX + TX interrupts
        self.write32(E1000_IMS, ICR_RXT0 | ICR_TXDW);
    }

    /// Transmit a packet. Returns false if TX ring is full.
    fn send(&mut self, data: &[u8]) -> bool {
        if data.is_empty() || data.len() > 1514 { return false; }

        let desc = unsafe { &mut *self.tx_descs.add(self.tx_tail) };

        // Wait for descriptor to be done (ring full guard)
        // DD bit is set by hardware when transmission is complete.
        // On first use (status=0, not yet transmitted), we can use it freely.
        if desc.status != 0 && desc.status & TDESC_STA_DD == 0 {
            return false; // descriptor still in use by hardware
        }

        // Copy packet into TX buffer
        let buf_va = unsafe { self.tx_bufs_va.add(self.tx_tail * 1514) };
        unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), buf_va, data.len()); }

        desc.addr   = self.tx_bufs_phys + (self.tx_tail * 1514) as u64;
        desc.length = data.len() as u16;
        desc.cmd    = TDESC_CMD_EOP | TDESC_CMD_IFCS | TDESC_CMD_RS;
        desc.status = 0;

        self.tx_tail = (self.tx_tail + 1) % NUM_TX_DESC;
        // Fence to ensure descriptor write is visible before updating TDT
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        self.write32(E1000_TDT, self.tx_tail as u32);
        true
    }

    /// Poll for a received packet. Returns length or 0.
    fn recv(&mut self, out: &mut [u8; RX_BUF_SIZE]) -> usize {
        let next = (self.rx_tail + 1) % NUM_RX_DESC;
        let desc = unsafe { &mut *self.rx_descs.add(next) };

        // Memory fence to ensure we see hardware writes to the descriptor
        core::sync::atomic::fence(core::sync::atomic::Ordering::Acquire);

        if desc.status & RDESC_STA_DD == 0 {
            return 0; // nothing yet
        }

        // Check for errors
        if desc.errors != 0 {
            // Return descriptor to hardware even on error
            desc.status = 0;
            self.rx_tail = next;
            self.write32(E1000_RDT, self.rx_tail as u32);
            return 0;
        }

        let len = desc.length as usize;
        if len == 0 || len > RX_BUF_SIZE {
            desc.status = 0;
            self.rx_tail = next;
            self.write32(E1000_RDT, self.rx_tail as u32);
            return 0;
        }

        let src = unsafe {
            core::slice::from_raw_parts(
                self.rx_bufs.add(next * RX_BUF_SIZE),
                len,
            )
        };
        out[..len].copy_from_slice(src);

        // Return descriptor to hardware
        desc.status = 0;
        self.rx_tail = next;
        // Fence to ensure descriptor write is visible before updating RDT
        core::sync::atomic::fence(core::sync::atomic::Ordering::Release);
        self.write32(E1000_RDT, self.rx_tail as u32);

        len
    }
}

// ============================================================================
// Entry Point
// ============================================================================

const SUPPORTED_NICS: &[(u16, u16)] = &[
    (0x8086, 0x100E), // Intel e1000 (82540EM) — QEMU default
    (0x8086, 0x10D3), // Intel e1000e (82574L)
    (0x8086, 0x100F), // Intel e1000 (82545EM)
    (0x8086, 0x1502), // Intel 82579LM
    (0x8086, 0x1503), // Intel 82579V
    (0x1AF4, 0x1000), // VirtIO Net
];

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main();
}

fn main() -> ! {
    log("nic_driver: starting e1000 driver");

    // ── 1. Discover supported NIC via PCI enumeration ────────────────────────
    let devices = match atom_syscall::io::pci_enumerate() {
        Ok(d) => d,
        Err(_) => {
            log("nic_driver: PCI enumeration failed");
            loop { atom_syscall::thread::sleep_ms(1000); }
        }
    };

    log("nic_driver: PCI enumeration found devices:");
    let mut nic_info = None;

    fn write_hex(buf: &mut [u8], pos: &mut usize, val: u64, digits: usize) {
        for i in (0..digits).rev() {
            let nibble = (val >> (i * 4)) & 0xF;
            buf[*pos] = if nibble < 10 { b'0' + nibble as u8 } else { b'a' + (nibble - 10) as u8 };
            *pos += 1;
        }
    }

    for dev in &devices {
        // Log everything seen — essential for debugging topology shifts
        let mut msg = [0u8; 128];
        let mut pos = 0;
        let s = "  ";
        msg[pos..pos+s.len()].copy_from_slice(s.as_bytes()); pos += s.len();

        write_hex(&mut msg, &mut pos, dev.bus as u64, 2); msg[pos] = b':'; pos += 1;
        write_hex(&mut msg, &mut pos, dev.device as u64, 2); msg[pos] = b'.'; pos += 1;
        write_hex(&mut msg, &mut pos, dev.function as u64, 1);
        let s = " vendor=0x";
        msg[pos..pos+s.len()].copy_from_slice(s.as_bytes()); pos += s.len();
        write_hex(&mut msg, &mut pos, dev.vendor_id as u64, 4);
        let s = " device=0x";
        msg[pos..pos+s.len()].copy_from_slice(s.as_bytes()); pos += s.len();
        write_hex(&mut msg, &mut pos, dev.device_id as u64, 4);
        let s = " class=0x";
        msg[pos..pos+s.len()].copy_from_slice(s.as_bytes()); pos += s.len();
        write_hex(&mut msg, &mut pos, dev.class as u64, 2);

        if let Ok(s) = core::str::from_utf8(&msg[..pos]) {
            log(s);
        }

        if nic_info.is_none() {
            if SUPPORTED_NICS.iter().any(|&(vid, did)| dev.vendor_id == vid && dev.device_id == did) {
                nic_info = Some(*dev);
            }
        }
    }

    let nic = match nic_info {
        Some(d) => d,
        None => {
            log("nic_driver: no supported NIC found — exiting cleanly");
            atom_syscall::thread::exit(0);
        }
    };

    let mut msg = [0u8; 128];
    let mut pos = 0;
    let s = "nic_driver: claiming NIC at ";
    msg[pos..pos+s.len()].copy_from_slice(s.as_bytes()); pos += s.len();
    write_hex(&mut msg, &mut pos, nic.bus as u64, 2); msg[pos] = b':'; pos += 1;
    write_hex(&mut msg, &mut pos, nic.device as u64, 2); msg[pos] = b'.'; pos += 1;
    write_hex(&mut msg, &mut pos, nic.function as u64, 1);
    if let Ok(s) = core::str::from_utf8(&msg[..pos]) {
        log(s);
    }

    let device_cap = match atom_syscall::io::pci_request_device(nic.bus, nic.device, nic.function) {
        Ok(h) => h,
        Err(_) => {
            log("nic_driver: failed to obtain DeviceCap");
            loop { atom_syscall::thread::sleep_ms(1000); }
        }
    };

    // ── 2. Map MMIO BAR0 ─────────────────────────────────────────────────────
    let mut bar0 = atom_syscall::atom_abi::PciBarInfo::default();
    if atom_syscall::raw::pci_get_bar(device_cap, 0, &mut bar0) != 0 || bar0.is_mmio == 0 {
        log("nic_driver: failed to get MMIO BAR0");
        loop { atom_syscall::thread::sleep_ms(1000); }
    }

    let mut mmio_va: u64 = 0;
    if bar0.mmio_cap == 0 {
        log("nic_driver: BAR0 mmio_cap is 0 (kernel failed to create MMIO cap)");
        loop { atom_syscall::thread::sleep_ms(1000); }
    }
    if atom_syscall::raw::map_mmio(bar0.mmio_cap, &mut mmio_va) != 0 || mmio_va == 0 {
        log("nic_driver: failed to map MMIO");
        loop { atom_syscall::thread::sleep_ms(1000); }
    }

    log("nic_driver: MMIO mapped");

    // ── 3. Allocate DMA memory for TX/RX descriptors and buffers ─────────────
    // Layout: [TX descs 128B][RX descs 128B][TX bufs 8*1514B][RX bufs 8*2048B]
    let tx_desc_size  = NUM_TX_DESC * 16;
    let rx_desc_size  = NUM_RX_DESC * 16;
    let tx_buf_size   = NUM_TX_DESC * 1514;
    let rx_buf_size   = NUM_RX_DESC * RX_BUF_SIZE;
    let total_dma     = tx_desc_size + rx_desc_size + tx_buf_size + rx_buf_size;
    let dma_pages     = (total_dma + 4095) / 4096;

    let dma_params = atom_syscall::atom_abi::DmaAllocParams {
        size: (dma_pages * 4096) as u64,
        align: 4096,
        flags: 0,
    };
    let mut dma_info = atom_syscall::atom_abi::DmaMappingInfo::default();
    let dma_cap = atom_syscall::raw::dma_alloc(&dma_params, &mut dma_info);
    if dma_cap == 0 || dma_info.user_va == 0 {
        log("nic_driver: DMA alloc failed");
        loop { atom_syscall::thread::sleep_ms(1000); }
    }

    let va_base  = dma_info.user_va;
    let phys_base = dma_info.phys_addr;

    let tx_desc_va   = va_base as *mut TxDesc;
    let rx_desc_va   = (va_base + tx_desc_size as u64) as *mut RxDesc;
    let tx_buf_va    = (va_base + tx_desc_size as u64 + rx_desc_size as u64) as *mut u8;
    let rx_buf_va    = (va_base + tx_desc_size as u64 + rx_desc_size as u64 + tx_buf_size as u64) as *mut u8;

    let tx_desc_phys = phys_base;
    let rx_desc_phys = phys_base + tx_desc_size as u64;
    let rx_buf_phys  = phys_base + tx_desc_size as u64 + rx_desc_size as u64 + tx_buf_size as u64;

    // Zero descriptors
    unsafe {
        core::ptr::write_bytes(va_base as *mut u8, 0, dma_pages * 4096);
    }

    log("nic_driver: DMA allocated");

    // ── 4. Bind IRQ ───────────────────────────────────────────────────────────
    let mut irq_info = atom_syscall::atom_abi::IrqBindInfo::default();
    atom_syscall::raw::device_bind_irq(device_cap, &mut irq_info);

    // ── 5. Register IPC port and IRQ listener ────────────────────────────────
    let port = atom_syscall::ipc::create_port().expect("failed to create port");
    if irq_info.irq_cap != 0 {
        atom_syscall::raw::irq_listen(irq_info.irq_cap, port);
    }

    // ── 6. Initialize e1000 ───────────────────────────────────────────────────
    let mut nic = E1000 {
        mmio: mmio_va,
        mac: [0u8; 6],
        tx_descs: tx_desc_va,
        tx_bufs_va:   tx_buf_va,
        tx_bufs_phys: phys_base + tx_desc_size as u64 + rx_desc_size as u64,
        tx_tail:  0,
        rx_descs: rx_desc_va,
        rx_bufs:  rx_buf_va,
        rx_tail:  0,
    };
    nic.init(tx_desc_phys, rx_desc_phys, rx_buf_phys);
    log("nic_driver: e1000 initialized");

    // Log the MAC address
    {
        let m = nic.mac;
        let mut msg = [0u8; 64];
        let prefix = b"nic_driver: MAC=";
        msg[..prefix.len()].copy_from_slice(prefix);
        let mut pos = prefix.len();
        for (i, &b) in m.iter().enumerate() {
            let hi = b >> 4;
            let lo = b & 0xF;
            msg[pos] = if hi < 10 { b'0' + hi } else { b'a' + hi - 10 };
            msg[pos+1] = if lo < 10 { b'0' + lo } else { b'a' + lo - 10 };
            pos += 2;
            if i < 5 { msg[pos] = b':'; pos += 1; }
        }
        if let Ok(s) = core::str::from_utf8(&msg[..pos]) {
            log(s);
        }
    }

    // ── 7. Register with namesvc ──────────────────────────────────────────────
    register_service("nic_driver", port).expect("failed to register nic_driver");
    log("nic_driver: registered, waiting for netd");

    // ── 7b. Wait for netd and send MAC address ────────────────────────────────
    let netd_port = loop {
        match lookup_service("netd") {
            Ok(p) => break p,
            Err(_) => atom_syscall::thread::sleep_ms(50),
        }
    };
    let ready_msg = NetDriverReadyMsg { mac: nic.mac, _pad: [0u8; 2] };
    send_message(netd_port, MessageType::NetDriverReady, &ready_msg.to_bytes())
        .expect("nic_driver: failed to send MAC to netd");
    log("nic_driver: sent MAC to netd");

    // ── 8. Main loop ──────────────────────────────────────────────────────────
    let mut tx_consumer: Option<RingConsumer> = None;
    let mut rx_producer: Option<RingProducer> = None;
    let mut ipc_buf = [0u8; 512];
    let mut rx_pkt  = [0u8; RX_BUF_SIZE];
    let mut rx_check_counter = 0u32;

    loop {
        // Handle IPC (ring assignment from netd, IRQ notifications)
        let ports = [port];
        if atom_syscall::ipc::wait_any(&ports, 0).is_ok() {
            while let Ok(Some((header, len))) = try_recv_message(port, &mut ipc_buf) {
                if header.msg_type == MessageType::NetAssignRings {
                    let payload = get_payload(&ipc_buf, len);
                    if let Some(msg) = NetAssignRingsMsg::from_bytes(payload) {
                        setup_rings(&mut tx_consumer, &mut rx_producer, msg);
                        log("nic_driver: rings configured, link up");
                    }
                } else if header.msg_type == MessageType::IrqNotification {
                    // IRQ fired — clear it and process
                    if irq_info.irq_cap != 0 {
                        atom_syscall::raw::irq_ack(irq_info.irq_cap);
                    }
                    let _icr = nic.read32(E1000_ICR); // clears interrupt
                }
            }
        }

        // Drain TX ring → send to wire
        if let Some(ref mut tx) = tx_consumer {
            while tx.can_pop() {
                if let Some(entry) = tx.pop() {
                    let len = entry.len as usize;
                    if len > 0 && len <= 1514 {
                        if nic.send(&entry.data[..len]) {
                            log("nic_driver: TX packet sent");
                        } else {
                            log("nic_driver: TX ring full, packet dropped");
                        }
                    }
                }
            }
        }

        // Drain received packets → push to RX ring
        if let Some(ref mut rx) = rx_producer {
            // Periodically log RX descriptor status for diagnostics
            rx_check_counter += 1;
            if rx_check_counter >= 5000 {
                rx_check_counter = 0;
                let rdh = nic.read32(E1000_RDH);
                let rdt = nic.read32(E1000_RDT);
                let status = nic.read32(E1000_STATUS);
                let rctl = nic.read32(E1000_RCTL);
                // Log if RDH != expected (hardware advanced past our tail)
                let expected_rdh = (nic.rx_tail + 1) % NUM_RX_DESC;
                if rdh != expected_rdh as u32 {
                    // Hardware has received packets we haven't processed
                    log("nic_driver: RDH mismatch — hardware received packets");
                }
                // Log link status
                if status & 0x2 != 0 {
                    // Link up bit
                } else {
                    log("nic_driver: WARNING link down");
                }
                let _ = (rdh, rdt, rctl);
            }

            loop {
                let len = nic.recv(&mut rx_pkt);
                if len == 0 { break; }
                log("nic_driver: RX packet received");
                if rx.can_push() {
                    let mut entry = RingEntry {
                        len: len as u16,
                        flags: 0,
                        offset: 0,
                        data: [0u8; 1528],
                    };
                    let copy_len = len.min(1528);
                    entry.data[..copy_len].copy_from_slice(&rx_pkt[..copy_len]);
                    rx.push(entry).ok();
                } else {
                    log("nic_driver: RX ring full, packet dropped");
                }
            }
        }

        atom_syscall::thread::yield_now();
    }
}

fn setup_rings<'a>(
    tx_consumer: &mut Option<RingConsumer<'a>>,
    rx_producer: &mut Option<RingProducer<'a>>,
    msg: NetAssignRingsMsg,
) {
    let va = match atom_syscall::shared_mem::map_region(
        msg.region_id,
        0,
        atom_syscall::shared_mem::RegionFlags::READ | atom_syscall::shared_mem::RegionFlags::WRITE,
    ) {
        Ok(v) => v,
        Err(_) => { log("nic_driver: failed to map netd rings"); return; }
    };

    let cap = msg.ring_capacity as usize;
    let hdr_sz = core::mem::size_of::<RingHeader>();
    let ent_sz = cap * core::mem::size_of::<RingEntry>();

    let tx_hdr = va as *const RingHeader;
    let rx_hdr = (va + hdr_sz as u64) as *const RingHeader;
    let tx_ent = (va + (hdr_sz * 2) as u64) as *const RingEntry;
    let rx_ent = (va + (hdr_sz * 2 + ent_sz) as u64) as *mut RingEntry;

    *tx_consumer = Some(RingConsumer::new(
        unsafe { &*tx_hdr },
        unsafe { core::slice::from_raw_parts(tx_ent, cap) },
    ));
    *rx_producer = Some(RingProducer::new(
        unsafe { &*rx_hdr },
        unsafe { core::slice::from_raw_parts_mut(rx_ent, cap) },
    ));
}

#[panic_handler]
fn panic(_: &PanicInfo) -> ! {
    log("nic_driver: PANIC");
    loop { atom_syscall::thread::sleep_ms(1000); }
}
