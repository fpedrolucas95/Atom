//! Atom Desktop Environment — UI Shell

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::panic::PanicInfo;

use alloc::string::String;
use alloc::vec::Vec;

use atom_syscall::graphics::{
    Color, Framebuffer, SharedSurface, SharedRegionId, SharedMemFlags,
    shared_region_create, shared_region_map, shared_region_unmap, shared_region_destroy,
    get_framebuffer,
};
use atom_syscall::ipc::{create_port, send, try_recv, recv_envelope, wait_any, IpcEnvelope, PortId};
use atom_syscall::interrupts::register_irq_handler;
use atom_syscall::thread::{exit, get_ticks, yield_now};
use atom_syscall::debug::log;
use atom_syscall::input::{MouseDriver, keyboard_poll, scancode_to_ascii, scancodes};
use atom_syscall::fs;
use atom_syscall::SyscallError;

use libipc::messages::{
    MessageType, MessageHeader, WindowId, SurfaceAssignMsg, TerminateRequestMsg,
    AppRegisterMsg, SurfacePresentMsg, KeyEvent, KeyModifiers, MouseMoveEvent,
    MouseButtonEvent, MouseButton, MouseScrollEvent, OpenInTabMsg, ApplyWallpaperMsg,
    WallpaperAppliedMsg, WallpaperFailedMsg, AppLaunchRequestMsg, AppLaunchReplyMsg,
    TimeGetStateMsg, TimeStateReplyMsg, AudioGetStateMsg, AudioPlayFileMsg,
    AudioSetStateMsg, AudioStateReplyMsg, launch_status,
};
use libipc::protocol::send_message_async;
use libimage::{DecodedImage, ImageDecoder, JpgDecoder, PngDecoder};

mod mem {
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::UnsafeCell;
    use core::ptr;
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    const MIN_BLOCK: usize = 32;
    const ALIGN: usize = 16;

    #[inline]
    fn align_up(v: usize, a: usize) -> usize {
        (v + a - 1) & !(a - 1)
    }

    #[repr(C)]
    struct FreeNode {
        size: usize,
        next: *mut FreeNode,
    }

    #[repr(C)]
    struct AllocHeader {
        size: usize,
        block_offset: usize,
    }

    const HEADER_SIZE: usize = core::mem::size_of::<AllocHeader>();

    struct Spinlock {
        flag: AtomicBool,
    }
    impl Spinlock {
        const fn new() -> Self {
            Self { flag: AtomicBool::new(false) }
        }
        #[inline]
        fn lock(&self) {
            while self
                .flag
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                core::hint::spin_loop();
            }
        }
        #[inline]
        fn unlock(&self) {
            self.flag.store(false, Ordering::Release);
        }
    }

    struct Inner {
        heap_start: usize,
        heap_end: usize,
        free_list: *mut FreeNode,
        scratch_start: usize,
        scratch_end: usize,
        scratch_next: usize,
    }

    pub struct Heap {
        lock: Spinlock,
        inner: UnsafeCell<Inner>,
        scratch_active: AtomicBool,
    }

    unsafe impl Sync for Heap {}

    impl Heap {
        pub const fn new() -> Self {
            Self {
                lock: Spinlock::new(),
                inner: UnsafeCell::new(Inner {
                    heap_start: 0,
                    heap_end: 0,
                    free_list: ptr::null_mut(),
                    scratch_start: 0,
                    scratch_end: 0,
                    scratch_next: 0,
                }),
                scratch_active: AtomicBool::new(false),
            }
        }

        pub fn init(&self, start: usize, size: usize) {
            self.lock.lock();
            unsafe {
                let inner = &mut *self.inner.get();
                let aligned = align_up(start, ALIGN);
                let end = start + size;
                inner.heap_start = aligned;
                inner.heap_end = end;
                let node = aligned as *mut FreeNode;
                (*node).size = end - aligned;
                (*node).next = ptr::null_mut();
                inner.free_list = node;
            }
            self.lock.unlock();
        }

        pub fn push_scratch(&self, start: usize, size: usize) {
            self.lock.lock();
            unsafe {
                let inner = &mut *self.inner.get();
                let aligned = align_up(start, ALIGN);
                inner.scratch_start = aligned;
                inner.scratch_end = start + size;
                inner.scratch_next = aligned;
            }
            self.scratch_active.store(true, Ordering::SeqCst);
            self.lock.unlock();
        }

        pub fn pop_scratch(&self) {
            self.scratch_active.store(false, Ordering::SeqCst);
            self.lock.lock();
            unsafe {
                let inner = &mut *self.inner.get();
                inner.scratch_start = 0;
                inner.scratch_end = 0;
                inner.scratch_next = 0;
            }
            self.lock.unlock();
        }

        unsafe fn alloc_scratch(&self, inner: &mut Inner, size: usize, align: usize) -> *mut u8 {
            let base = align_up(inner.scratch_next, align.max(ALIGN));
            let total = base + size;
            if total > inner.scratch_end {
                return ptr::null_mut();
            }
            inner.scratch_next = total;
            base as *mut u8
        }

        unsafe fn alloc_heap(&self, inner: &mut Inner, size: usize, align: usize) -> *mut u8 {
            let align = align.max(ALIGN);
            let mut prev: *mut FreeNode = ptr::null_mut();
            let mut cur = inner.free_list;

            while !cur.is_null() {
                let block_addr = cur as usize;
                let block_size = (*cur).size;
                let block_end = block_addr + block_size;
                let payload_addr = align_up(block_addr + HEADER_SIZE, align);
                let split_at = align_up(payload_addr + size, ALIGN);

                if split_at <= block_end {
                    let remainder = block_end - split_at;

                    if prev.is_null() {
                        inner.free_list = (*cur).next;
                    } else {
                        (*prev).next = (*cur).next;
                    }

                    let reserved_total;
                    if remainder >= MIN_BLOCK {
                        let rem_node = split_at as *mut FreeNode;
                        (*rem_node).size = remainder;
                        (*rem_node).next = inner.free_list;
                        inner.free_list = rem_node;
                        reserved_total = split_at - block_addr;
                    } else {
                        reserved_total = block_size;
                    }

                    let header = (payload_addr - HEADER_SIZE) as *mut AllocHeader;
                    (*header).size = reserved_total;
                    (*header).block_offset = payload_addr - HEADER_SIZE - block_addr;
                    return payload_addr as *mut u8;
                }

                prev = cur;
                cur = (*cur).next;
            }

            ptr::null_mut()
        }
    }

    unsafe impl GlobalAlloc for Heap {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            let size = layout.size().max(1);
            let align = layout.align().max(ALIGN);

            if self.scratch_active.load(Ordering::SeqCst) {
                self.lock.lock();
                let inner = &mut *self.inner.get();
                let p = self.alloc_scratch(inner, size, align);
                self.lock.unlock();
                return p;
            }

            self.lock.lock();
            let inner = &mut *self.inner.get();
            let p = self.alloc_heap(inner, size, align);
            self.lock.unlock();
            p
        }

        unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
            if ptr.is_null() {
                return;
            }
            let addr = ptr as usize;

            self.lock.lock();
            let inner = &mut *self.inner.get();

            if addr >= inner.scratch_start && addr < inner.scratch_end {
                self.lock.unlock();
                return;
            }
            if addr < inner.heap_start || addr > inner.heap_end {
                self.lock.unlock();
                return;
            }

            let header = (addr - HEADER_SIZE) as *mut AllocHeader;
            let total = (*header).size;
            let block_offset = (*header).block_offset;
            let block_addr = (header as usize) - block_offset;

            let node = block_addr as *mut FreeNode;
            (*node).size = total;
            (*node).next = inner.free_list;
            inner.free_list = node;

            Self::coalesce(inner);

            self.lock.unlock();
        }
    }

    impl Heap {
        unsafe fn coalesce(inner: &mut Inner) {
            let mut a = inner.free_list;
            while !a.is_null() {
                let a_start = a as usize;
                let a_end = a_start + (*a).size;

                let mut prev: *mut FreeNode = ptr::null_mut();
                let mut b = inner.free_list;
                while !b.is_null() {
                    if b == a {
                        prev = b;
                        b = (*b).next;
                        continue;
                    }
                    let b_start = b as usize;
                    let b_end = b_start + (*b).size;

                    if a_end == b_start {
                        (*a).size += (*b).size;
                        if prev.is_null() {
                            inner.free_list = (*b).next;
                        } else {
                            (*prev).next = (*b).next;
                        }
                        b = inner.free_list;
                        prev = ptr::null_mut();
                        continue;
                    } else if b_end == a_start {
                        (*b).size += (*a).size;
                        Self::remove_node(inner, a);
                        a = b;
                        break;
                    }

                    prev = b;
                    b = (*b).next;
                }

                a = (*a).next;
            }
        }

        unsafe fn remove_node(inner: &mut Inner, target: *mut FreeNode) {
            let mut prev: *mut FreeNode = ptr::null_mut();
            let mut cur = inner.free_list;
            while !cur.is_null() {
                if cur == target {
                    if prev.is_null() {
                        inner.free_list = (*cur).next;
                    } else {
                        (*prev).next = (*cur).next;
                    }
                    return;
                }
                prev = cur;
                cur = (*cur).next;
            }
        }
    }
}

#[global_allocator]
static ALLOCATOR: mem::Heap = mem::Heap::new();

#[alloc_error_handler]
fn alloc_error(_layout: core::alloc::Layout) -> ! {
    log("atom_desktop: allocation failure");
    loop {}
}

mod theme {
    use atom_theme::colors as ds;
    use atom_syscall::graphics::Color;

    pub const DESKTOP_BG: Color = ds::ATOM_COLOR_BG;

    pub const PANEL_TEXT: Color = ds::ATOM_COLOR_TEXT_PRIMARY;
    pub const PANEL_TEXT_DIM: Color = ds::ATOM_COLOR_TEXT_SECONDARY;

    pub const ACCENT: Color = ds::ATOM_COLOR_ACCENT;

    pub const WINDOW_BG: Color = ds::ATOM_COLOR_WIN_BG;
    pub const WINDOW_HEADER: Color = ds::ATOM_COLOR_WIN_HDR;
    pub const WINDOW_HEADER_FOCUSED: Color = ds::ATOM_COLOR_WIN_HDR_FOC;
    pub const WINDOW_BORDER: Color = ds::ATOM_COLOR_WIN_BORDER;
    pub const WINDOW_BORDER_FOCUSED: Color = ds::ATOM_COLOR_WIN_BORDER_FOC;

    pub const DOCK_BG: Color = ds::ATOM_COLOR_DOCK_BG;
    pub const DOCK_BORDER: Color = ds::ATOM_COLOR_DOCK_BORDER;

    pub const CURSOR_FILL: Color = Color::WHITE;
    pub const CURSOR_OUTLINE: Color = Color::BLACK;

    pub const SHADOW: Color = ds::ATOM_COLOR_SHADOW;

    pub const BTN_CLOSE: Color = ds::ATOM_COLOR_BTN_CLOSE;
    pub const BTN_MAXIMIZE: Color = ds::ATOM_COLOR_BTN_MAX;
    pub const BTN_MINIMIZE: Color = ds::ATOM_COLOR_BTN_MIN;
    pub const BTN_INACTIVE: Color = ds::ATOM_COLOR_BTN_INACTIVE;

    pub const MENU_BG: Color = ds::ATOM_COLOR_MENU_BG;
    pub const MENU_BORDER: Color = ds::ATOM_COLOR_MENU_BORDER;
    pub const MENU_TEXT: Color = ds::ATOM_COLOR_MENU_TEXT;

    pub const ICON_LABEL: Color = ds::ATOM_COLOR_TEXT_SECONDARY;
}

const WINDOW_HEADER_HEIGHT: u32 = atom_theme::shell::WINDOW_TITLE_HEIGHT;
const WINDOW_BORDER_WIDTH: u32 = 1;
const WINDOW_MIN_WIDTH: u32 = 150;
const WINDOW_MIN_HEIGHT: u32 = 100;
const DOCK_HEIGHT: u32 = atom_theme::shell::DOCK_HEIGHT;
const CLOSE_GRACE_TICKS: u32 = 200;

/// Maximum number of independent damage rectangles tracked per frame before
/// they are collapsed into a single bounding box. Keeping damage as a list of
/// disjoint rects (instead of one union) is what stops a continuously-animating
/// window in one screen corner from forcing a full-screen recomposite — and a
/// re-blit of every other open window — on every frame. The cap bounds the
/// per-frame intersection work when damage is genuinely scattered.
const MAX_DIRTY_RECTS: usize = 16;

/// Minimum number of scheduler ticks between composites (frame pacing). The
/// compositor is single-threaded, so without a cap a client presenting in a
/// tight loop — or several animating windows — drives it to recomposite
/// back-to-back and pegs its CPU core. At the 100Hz scheduler tick (10ms),
/// 2 ticks ≈ 50fps, the closest cap at or below ~60fps the tick granularity
/// allows. Damage accumulates between slots and drains in a single composite.
const MIN_FRAME_TICKS: u64 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    pub fn union(&self, other: &Rect) -> Rect {
        let x1 = self.x.min(other.x);
        let y1 = self.y.min(other.y);
        let x2 = (self.x + self.w as i32).max(other.x + other.w as i32);
        let y2 = (self.y + self.h as i32).max(other.y + other.h as i32);
        Rect { x: x1, y: y1, w: (x2 - x1) as u32, h: (y2 - y1) as u32 }
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.w as i32
            && self.x + self.w as i32 > other.x
            && self.y < other.y + other.h as i32
            && self.y + self.h as i32 > other.y
    }
}

#[inline]
fn isqrt_helper(n: u32) -> u32 {
    if n == 0 {
        return 0;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

#[inline]
fn frac_sqrt_256_helper(n: u32, sq_int: u32) -> u32 {
    if sq_int == 0 {
        return 0;
    }
    let step = 2 * sq_int + 1;
    let remainder = n - sq_int * sq_int;
    (remainder * 256) / step
}

#[inline]
fn rgb32_to_color(pixel: u32) -> Color {
    Color::new(
        ((pixel >> 16) & 0xFF) as u8,
        ((pixel >> 8) & 0xFF) as u8,
        (pixel & 0xFF) as u8,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowState {
    Normal,
    Minimized,
    Maximized,
}

struct Window {
    id: WindowId,
    title: String,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    state: WindowState,
    visible: bool,
    focused: bool,
    event_port: Option<PortId>,
    surface: Option<SharedSurface>,
    surface_region_id: Option<SharedRegionId>,
    content_dirty: bool,
    surface_ready: bool,
    /// Compositor-owned snapshot of the client surface, refreshed only when the
    /// app commits a frame. Compositing always reads from this copy, never from
    /// the live shared surface the client may be half-way through redrawing —
    /// otherwise any recomposite (cursor damage, window drag, ...) that lands
    /// mid-frame shows a torn/incomplete frame (the "blinking ball" bug).
    front_region: Option<SharedRegionId>,
    front_ptr: *mut u32,
    front_width: u32,
    front_height: u32,
    /// Set when the app commits a frame; the actual snapshot copy is deferred
    /// to once per compositor loop pass (`flush_pending_snapshots`). Copying
    /// inside the IPC drain loop lets a client that commits in a tight loop
    /// outpace the compositor, which then never exits the drain loop and the
    /// screen freezes while the rest of the system keeps running.
    pending_snapshot: bool,
    saved_x: i32,
    saved_y: i32,
    saved_width: u32,
    saved_height: u32,
}

impl Window {
    fn new(id: WindowId, title: &str, x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            id,
            title: String::from(title),
            x,
            y,
            width: width.max(WINDOW_MIN_WIDTH),
            height: height.max(WINDOW_MIN_HEIGHT),
            state: WindowState::Normal,
            visible: true,
            focused: false,
            event_port: None,
            surface: None,
            surface_region_id: None,
            content_dirty: false,
            surface_ready: false,
            front_region: None,
            front_ptr: core::ptr::null_mut(),
            front_width: 0,
            front_height: 0,
            pending_snapshot: false,
            saved_x: x,
            saved_y: y,
            saved_width: width,
            saved_height: height,
        }
    }

    fn new_with_process(
        id: WindowId,
        title: &str,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        event_port: PortId,
    ) -> Option<Self> {
        let width = width.max(WINDOW_MIN_WIDTH);
        let height = height.max(WINDOW_MIN_HEIGHT);

        let content_width = width.saturating_sub(WINDOW_BORDER_WIDTH * 2);
        let content_height = height.saturating_sub(WINDOW_HEADER_HEIGHT + WINDOW_BORDER_WIDTH);

        let surface = match SharedSurface::create(content_width, content_height) {
            Ok(s) => s,
            Err(_) => return None,
        };

        let region_id = surface.region_id();

        Some(Self {
            id,
            title: String::from(title),
            x,
            y,
            width,
            height,
            state: WindowState::Normal,
            visible: true,
            focused: false,
            event_port: Some(event_port),
            surface_region_id: Some(region_id),
            surface: Some(surface),
            content_dirty: false,
            surface_ready: false,
            front_region: None,
            front_ptr: core::ptr::null_mut(),
            front_width: 0,
            front_height: 0,
            pending_snapshot: false,
            saved_x: x,
            saved_y: y,
            saved_width: width,
            saved_height: height,
        })
    }

    fn content_x(&self) -> u32 {
        (self.x as u32).wrapping_add(WINDOW_BORDER_WIDTH)
    }
    fn content_y(&self) -> u32 {
        (self.y as u32).wrapping_add(WINDOW_HEADER_HEIGHT)
    }
    fn content_width(&self) -> u32 {
        self.width.saturating_sub(WINDOW_BORDER_WIDTH * 2)
    }
    fn content_height(&self) -> u32 {
        self.height.saturating_sub(WINDOW_HEADER_HEIGHT + WINDOW_BORDER_WIDTH)
    }

    /// Release the compositor-side front buffer (shared region + mapping).
    /// Must be called before the window is dropped, since the region is a
    /// kernel resource not tied to the struct's lifetime.
    fn release_front_buffer(&mut self) {
        if let Some(region) = self.front_region.take() {
            let _ = shared_region_unmap(region);
            let _ = shared_region_destroy(region);
        }
        self.front_ptr = core::ptr::null_mut();
        self.front_width = 0;
        self.front_height = 0;
    }

    /// Copy the client's live shared surface into the compositor-owned front
    /// buffer. Called on frame commit, so the snapshot is taken right after the
    /// app finished drawing (it is typically asleep between frames), while
    /// composites can then read a stable, complete frame at any time.
    /// Returns `false` (leaving any previous snapshot intact) on failure.
    fn snapshot_surface(&mut self) -> bool {
        let (src_addr, w, h, stride) = match self.surface {
            Some(ref s) => match s.address() {
                Some(addr) => (addr as usize, s.width(), s.height(), s.stride()),
                None => return false,
            },
            None => return false,
        };
        if w == 0 || h == 0 {
            return false;
        }

        if self.front_region.is_none() || self.front_width != w || self.front_height != h {
            self.release_front_buffer();
            let size = (w as usize) * (h as usize) * 4;
            let region = match shared_region_create(size) {
                Ok(r) => r,
                Err(_) => return false,
            };
            let ptr = match shared_region_map(region, 0, SharedMemFlags::READ_WRITE) {
                Ok(p) => p as *mut u32,
                Err(_) => {
                    let _ = shared_region_destroy(region);
                    return false;
                }
            };
            self.front_region = Some(region);
            self.front_ptr = ptr;
            self.front_width = w;
            self.front_height = h;
        }

        let row_px = w as usize;
        for row in 0..h as usize {
            let src = (src_addr + row * stride as usize * 4) as *const u32;
            // SAFETY: src rows are within the mapped client surface (stride ×
            // height × 4 bytes); dst is our freshly mapped w × h × 4 region.
            unsafe {
                core::ptr::copy_nonoverlapping(src, self.front_ptr.add(row * row_px), row_px);
            }
        }
        true
    }

    fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x
            && py >= self.y
            && px < self.x + self.width as i32
            && py < self.y + self.height as i32
    }

    fn header_contains(&self, px: i32, py: i32) -> bool {
        px >= self.x
            && py >= self.y
            && px < self.x + self.width as i32
            && py < self.y + WINDOW_HEADER_HEIGHT as i32
    }
}

fn send_wm_window_event(
    port: PortId,
    window_id: WindowId,
    event_type: libipc::messages::WindowEventType,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
) {
    if port == 0 {
        return;
    }
    let msg = libipc::messages::WmWindowEventMsg {
        window_id,
        event_type,
        x,
        y,
        width,
        height,
    };
    let header = MessageHeader::new(
        MessageType::WmEvent,
        libipc::messages::WmWindowEventMsg::SIZE as u32,
    );
    let mut full = [0u8; 64];
    full[..MessageHeader::SIZE].copy_from_slice(&header.to_bytes());
    full[MessageHeader::SIZE..MessageHeader::SIZE + libipc::messages::WmWindowEventMsg::SIZE]
        .copy_from_slice(&msg.to_bytes());
    let _ = send(port, &full[..MessageHeader::SIZE + libipc::messages::WmWindowEventMsg::SIZE]);
}

/// Size (px) of each window control button (close / maximise / minimise).
const WINDOW_BTN_SIZE: i32 = 12;
/// Horizontal stride between consecutive control buttons.
const WINDOW_BTN_STRIDE: i32 = 20;
/// Right inset of the close button from the window's right edge.
const WINDOW_BTN_INSET: i32 = 22;
/// Extra padding around each button that still counts as a click.
const WINDOW_BTN_HIT_PAD: i32 = 4;

/// Single source of truth for the window control-button geometry, shared by the
/// renderer (`draw_window`) and the hit-tester (`handle_click`) so the two can
/// never drift out of alignment.
///
/// Returns `(btn_y, size, close_x, max_x, min_x)` — all in absolute screen
/// coordinates, ordered right-to-left as drawn (close is rightmost).
fn window_button_layout(win_x: i32, win_y: i32, win_w: u32) -> (i32, i32, i32, i32, i32) {
    let btn_y = win_y + (WINDOW_HEADER_HEIGHT as i32 - WINDOW_BTN_SIZE) / 2;
    let close_x = win_x + win_w as i32 - WINDOW_BTN_INSET;
    let max_x = close_x - WINDOW_BTN_STRIDE;
    let min_x = max_x - WINDOW_BTN_STRIDE;
    (btn_y, WINDOW_BTN_SIZE, close_x, max_x, min_x)
}

/// Map a dock entry's executable name to the window title its process registers,
/// so the dock can detect already-running instances and raise them instead of
/// spawning duplicates.
fn window_title_for(executable: &str) -> &str {
    match executable {
        "fileman" => "File Manager",
        "terminal" => "Terminal",
        "system_settings" => "System Settings",
        "tinygl_demo" => "TinyGL Gears",
        other => other,
    }
}

/// Pick a stable tile colour and one-letter glyph for a window's taskbar
/// button. Known apps reuse the colour of their pinned launcher; any other
/// window derives its glyph from the title's first letter and a colour chosen
/// deterministically from the title, so the same window always looks the same.
fn taskbar_glyph_for(title: &str) -> (Color, String) {
    use atom_theme::colors as c;

    let known = match title {
        "File Manager" => Some(c::ATOM_COLOR_ACCENT_GLOW),
        "Terminal" => Some(c::ATOM_GRADIENT_PRIMARY_END),
        "System Settings" => Some(c::ATOM_COLOR_ACCENT),
        "TinyGL Gears" => Some(c::ATOM_COLOR_SUCCESS),
        _ => None,
    };

    let color = known.unwrap_or_else(|| {
        const PALETTE: [Color; 6] = [
            c::ATOM_COLOR_ACCENT,
            c::ATOM_COLOR_ACCENT_GLOW,
            c::ATOM_COLOR_SUCCESS,
            c::ATOM_GRADIENT_PRIMARY_END,
            c::ATOM_COLOR_WARNING,
            c::ATOM_COLOR_ERROR,
        ];
        // FNV-1a over the title keeps the colour stable per window title.
        let mut h: u32 = 0x811c_9dc5;
        for b in title.bytes() {
            h = (h ^ b as u32).wrapping_mul(0x0100_0193);
        }
        PALETTE[(h as usize) % PALETTE.len()]
    });

    let initial = title
        .chars()
        .find(|ch| ch.is_ascii_alphanumeric())
        .unwrap_or('?')
        .to_ascii_uppercase();
    let mut mono = String::new();
    mono.push(initial);
    (color, mono)
}

struct WindowManager {
    windows: Vec<Window>,
    next_id: WindowId,
    focused_id: Option<WindowId>,
}

impl WindowManager {
    fn new() -> Self {
        Self { windows: Vec::new(), next_id: 1, focused_id: None }
    }

    fn create_window_with_process(
        &mut self,
        title: &str,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        event_port: PortId,
    ) -> Option<WindowId> {
        let id = self.next_id;
        self.next_id += 1;
        let window = Window::new_with_process(id, title, x, y, width, height, event_port)?;
        self.windows.push(window);
        self.focus_window(id);
        Some(id)
    }

    fn get_window(&self, id: WindowId) -> Option<&Window> {
        self.windows.iter().find(|w| w.id == id)
    }

    fn get_window_mut(&mut self, id: WindowId) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    fn focus_window(&mut self, id: WindowId) {
        if let Some(prev_id) = self.focused_id {
            if prev_id == id {
                if let Some(pos) = self.windows.iter().position(|w| w.id == id) {
                    let window = self.windows.remove(pos);
                    self.windows.push(window);
                }
                return;
            }
            if let Some(w) = self.windows.iter_mut().find(|w| w.id == prev_id) {
                w.focused = false;
                if let Some(port) = w.event_port {
                    send_wm_window_event(
                        port,
                        prev_id,
                        libipc::messages::WindowEventType::Unfocus,
                        w.x,
                        w.y,
                        w.width,
                        w.height,
                    );
                }
            }
        }

        if let Some(pos) = self.windows.iter().position(|w| w.id == id) {
            let mut window = self.windows.remove(pos);
            window.focused = true;
            window.visible = true;
            if window.state == WindowState::Minimized {
                window.state = WindowState::Normal;
            }

            if let Some(port) = window.event_port {
                send_wm_window_event(
                    port,
                    id,
                    libipc::messages::WindowEventType::Focus,
                    window.x,
                    window.y,
                    window.width,
                    window.height,
                );
            }

            self.windows.push(window);
            self.focused_id = Some(id);
        }
    }

    fn window_at(&self, x: i32, y: i32) -> Option<WindowId> {
        for window in self.windows.iter().rev() {
            if window.visible && window.state != WindowState::Minimized && window.contains(x, y) {
                return Some(window.id);
            }
        }
        None
    }

    fn close_window(&mut self, id: WindowId) {
        if let Some(pos) = self.windows.iter().position(|w| w.id == id) {
            let mut window = self.windows.remove(pos);
            window.release_front_buffer();
        }
        if self.focused_id == Some(id) {
            self.refocus_topmost();
        }
    }

    /// Give focus to the top-most eligible window. Resets `focused_id` first so
    /// `focus_window` performs a real focus transition (sets the `focused` flag
    /// and emits the `Focus` event) instead of short-circuiting on a stale id.
    fn refocus_topmost(&mut self) {
        self.focused_id = None;
        let candidate = self
            .windows
            .iter()
            .filter(|w| w.visible && w.state != WindowState::Minimized)
            .last()
            .map(|w| w.id);
        if let Some(new_focus) = candidate {
            self.focus_window(new_focus);
        }
    }

    fn resize_window(&mut self, id: WindowId, width: u32, height: u32) {
        if let Some(window) = self.get_window_mut(id) {
            if window.state == WindowState::Maximized {
                return;
            }
            let new_width = width.max(WINDOW_MIN_WIDTH);
            let new_height = height.max(WINDOW_MIN_HEIGHT);
            if window.width != new_width || window.height != new_height {
                window.width = new_width;
                window.height = new_height;
                window.content_dirty = true;
            }
        }
    }

    fn toggle_maximize(
        &mut self,
        id: WindowId,
        work_x: i32,
        work_y: i32,
        work_w: u32,
        work_h: u32,
    ) -> bool {
        if let Some(window) = self.get_window_mut(id) {
            match window.state {
                WindowState::Maximized => {
                    window.state = WindowState::Normal;
                    window.x = window.saved_x;
                    window.y = window.saved_y;
                    window.width = window.saved_width;
                    window.height = window.saved_height;
                }
                _ => {
                    window.saved_x = window.x;
                    window.saved_y = window.y;
                    window.saved_width = window.width;
                    window.saved_height = window.height;
                    window.state = WindowState::Maximized;
                    window.x = work_x;
                    window.y = work_y;
                    window.width = work_w;
                    window.height = work_h;
                }
            }
            window.content_dirty = true;
            return true;
        }
        false
    }

    fn minimize_window(&mut self, id: WindowId) {
        if let Some(window) = self.get_window_mut(id) {
            window.state = WindowState::Minimized;
            window.focused = false;
        }
        if self.focused_id == Some(id) {
            self.refocus_topmost();
        }
    }
}

struct CursorState {
    x: i32,
    y: i32,
}

impl CursorState {
    fn new(width: u32, height: u32) -> Self {
        Self { x: (width / 2) as i32, y: (height / 2) as i32 }
    }

    fn apply_delta(&mut self, dx: i32, dy: i32, width: u32, height: u32) {
        self.x = (self.x + dx).clamp(0, (width - 1) as i32);
        self.y = (self.y - dy).clamp(0, (height - 1) as i32);
    }
}

struct PendingWindow {
    window_id: WindowId,
    /// PID of the process launched for this window, learned from the
    /// `AppLaunchReply`. `0` until the reply arrives. The app's registration is
    /// matched to its window by this PID (authenticated by the kernel via the
    /// IPC envelope) rather than by arrival order, so a slow- or out-of-order
    /// registering app no longer binds to the wrong window.
    pid: u64,
}

struct PendingClose {
    window_id: WindowId,
    deadline_tick: u32,
}

struct DesktopIcon {
    label: String,
    executable: String,
    x: i32,
    y: i32,
    color: Color,
}

struct DockApp {
    executable: String,
    color: Color,
    monogram: String,
}

/// What activating a taskbar button does.
enum TaskKind {
    /// A pinned app with no open window — launch it.
    Launch { executable: String },
    /// An open window — raise/restore it, or minimise it if already focused.
    Window {
        window_id: WindowId,
        minimized: bool,
        focused: bool,
    },
}

/// One button on the bottom taskbar. The bar shows a launcher for every pinned
/// app that has no open window, plus a button for every open window (minimised
/// ones included), so any window can always be restored from here.
struct TaskItem {
    color: Color,
    monogram: String,
    kind: TaskKind,
}

struct ContextMenu {
    x: i32,
    y: i32,
    visible: bool,
    items: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragOperation {
    None,
    Move {
        window_id: WindowId,
        start_mouse_x: i32,
        start_mouse_y: i32,
        start_win_x: i32,
        start_win_y: i32,
    },
    Resize {
        window_id: WindowId,
        start_mouse_x: i32,
        start_mouse_y: i32,
        start_win_w: u32,
        start_win_h: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CurrentWallpaperSource {
    SolidColor { rgb: u32 },
    Image { path: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WallpaperSourceType {
    Image,
    SolidColor,
}

impl WallpaperSourceType {
    fn from_ipc(value: libipc::messages::WallpaperSourceType) -> Self {
        match value {
            libipc::messages::WallpaperSourceType::Image => Self::Image,
            libipc::messages::WallpaperSourceType::SolidColor => Self::SolidColor,
        }
    }
    fn to_str(self) -> &'static str {
        match self {
            Self::Image => "Image",
            Self::SolidColor => "SolidColor",
        }
    }
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "Image" => Some(Self::Image),
            "SolidColor" => Some(Self::SolidColor),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScalingMode {
    Fill,
    Fit,
    Stretch,
    Center,
    Tile,
}

impl ScalingMode {
    fn from_ipc(value: libipc::messages::ScalingMode) -> Self {
        match value {
            libipc::messages::ScalingMode::Fill => Self::Fill,
            libipc::messages::ScalingMode::Fit => Self::Fit,
            libipc::messages::ScalingMode::Stretch => Self::Stretch,
            libipc::messages::ScalingMode::Center => Self::Center,
            libipc::messages::ScalingMode::Tile => Self::Tile,
        }
    }
    fn to_str(self) -> &'static str {
        match self {
            Self::Fill => "Fill",
            Self::Fit => "Fit",
            Self::Stretch => "Stretch",
            Self::Center => "Center",
            Self::Tile => "Tile",
        }
    }
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "Fill" => Some(Self::Fill),
            "Fit" => Some(Self::Fit),
            "Stretch" => Some(Self::Stretch),
            "Center" => Some(Self::Center),
            "Tile" => Some(Self::Tile),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WallpaperConfig {
    source_type: WallpaperSourceType,
    image_path: Option<String>,
    color_rgb: Option<u32>,
    scaling_mode: ScalingMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ResolutionConfig {
    width: u32,
    height: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DesktopConfig {
    wallpaper: WallpaperConfig,
    resolution: ResolutionConfig,
}

#[derive(Debug)]
enum ParseError {
    InvalidJson,
    MissingField(&'static str),
    InvalidFieldValue(&'static str),
}

#[derive(Debug)]
enum ValidationError {
    InvalidResolution,
    MissingImagePath,
    EmptyImagePath,
    InvalidImageExtension,
    InvalidImagePath,
    MissingColorRgb,
}

impl DesktopConfig {
    fn default_config(width: u32, height: u32) -> Self {
        Self {
            wallpaper: WallpaperConfig {
                source_type: WallpaperSourceType::SolidColor,
                image_path: None,
                color_rgb: Some(0x12141C),
                scaling_mode: ScalingMode::Fill,
            },
            resolution: ResolutionConfig { width, height },
        }
    }

    fn to_json(&self) -> Result<String, ()> {
        let mut json = String::from("{\n");
        json.push_str("  \"wallpaper\": {\n");
        json.push_str("    \"source_type\": \"");
        json.push_str(self.wallpaper.source_type.to_str());
        json.push_str("\",\n");

        match self.wallpaper.source_type {
            WallpaperSourceType::Image => {
                if let Some(ref path) = self.wallpaper.image_path {
                    json.push_str("    \"image_path\": \"");
                    for ch in path.chars() {
                        match ch {
                            '"' => json.push_str("\\\""),
                            '\\' => json.push_str("\\\\"),
                            '\n' => json.push_str("\\n"),
                            '\r' => json.push_str("\\r"),
                            '\t' => json.push_str("\\t"),
                            _ => json.push(ch),
                        }
                    }
                    json.push_str("\",\n");
                }
            }
            WallpaperSourceType::SolidColor => {
                if let Some(rgb) = self.wallpaper.color_rgb {
                    json.push_str("    \"color_rgb\": ");
                    json.push_str(&format_u32(rgb));
                    json.push_str(",\n");
                }
            }
        }

        json.push_str("    \"scaling_mode\": \"");
        json.push_str(self.wallpaper.scaling_mode.to_str());
        json.push_str("\"\n");
        json.push_str("  },\n");

        json.push_str("  \"resolution\": {\n");
        json.push_str("    \"width\": ");
        json.push_str(&format_u32(self.resolution.width));
        json.push_str(",\n");
        json.push_str("    \"height\": ");
        json.push_str(&format_u32(self.resolution.height));
        json.push_str("\n");
        json.push_str("  }\n");
        json.push_str("}");

        Ok(json)
    }

    fn from_json(json: &str) -> Result<Self, ParseError> {
        let json = json.trim();

        let wallpaper_start = json
            .find("\"wallpaper\"")
            .ok_or(ParseError::MissingField("wallpaper"))?;
        let _wallpaper_obj_start = json[wallpaper_start..]
            .find('{')
            .ok_or(ParseError::InvalidJson)?;

        let source_type_str = extract_string_field(json, "source_type")
            .ok_or(ParseError::MissingField("source_type"))?;
        let source_type = WallpaperSourceType::from_str(&source_type_str)
            .ok_or(ParseError::InvalidFieldValue("source_type"))?;

        let scaling_mode_str = extract_string_field(json, "scaling_mode")
            .ok_or(ParseError::MissingField("scaling_mode"))?;
        let scaling_mode = ScalingMode::from_str(&scaling_mode_str)
            .ok_or(ParseError::InvalidFieldValue("scaling_mode"))?;

        let image_path = match source_type {
            WallpaperSourceType::Image => {
                let path = extract_string_field(json, "image_path")
                    .ok_or(ParseError::MissingField("image_path"))?;
                if path.is_empty() {
                    return Err(ParseError::InvalidFieldValue("image_path"));
                }
                Some(path)
            }
            WallpaperSourceType::SolidColor => None,
        };

        let color_rgb = match source_type {
            WallpaperSourceType::SolidColor => {
                let rgb = extract_number_field(json, "color_rgb")
                    .ok_or(ParseError::MissingField("color_rgb"))?;
                Some(rgb)
            }
            WallpaperSourceType::Image => None,
        };

        let width = extract_number_field(json, "width")
            .ok_or(ParseError::MissingField("width"))?;
        let height = extract_number_field(json, "height")
            .ok_or(ParseError::MissingField("height"))?;

        let config = DesktopConfig {
            wallpaper: WallpaperConfig { source_type, image_path, color_rgb, scaling_mode },
            resolution: ResolutionConfig { width, height },
        };

        config
            .validate()
            .map_err(|_| ParseError::InvalidFieldValue("validation"))?;
        Ok(config)
    }

    fn validate(&self) -> Result<(), ValidationError> {
        if self.resolution.width < 640 || self.resolution.width > 1920 {
            return Err(ValidationError::InvalidResolution);
        }
        if self.resolution.height < 480 || self.resolution.height > 1080 {
            return Err(ValidationError::InvalidResolution);
        }

        match self.wallpaper.source_type {
            WallpaperSourceType::Image => {
                if self.wallpaper.image_path.is_none() {
                    return Err(ValidationError::MissingImagePath);
                }
                let path = self.wallpaper.image_path.as_ref().unwrap();
                if path.is_empty() {
                    return Err(ValidationError::EmptyImagePath);
                }
                if !validate_wallpaper_path(path) {
                    let lower = path.to_lowercase();
                    if !lower.ends_with(".jpg") && !lower.ends_with(".jpeg") {
                        return Err(ValidationError::InvalidImageExtension);
                    }
                    return Err(ValidationError::InvalidImagePath);
                }
            }
            WallpaperSourceType::SolidColor => {
                if self.wallpaper.color_rgb.is_none() {
                    return Err(ValidationError::MissingColorRgb);
                }
            }
        }
        Ok(())
    }
}

fn extract_string_field(json: &str, field_name: &str) -> Option<String> {
    let pattern = alloc::format!("\"{}\"", field_name);
    let field_start = json.find(&pattern)?;
    let after_field = &json[field_start + pattern.len()..];
    let colon_pos = after_field.find(':')?;
    let after_colon = after_field[colon_pos + 1..].trim_start();

    if !after_colon.starts_with('"') {
        return None;
    }

    let value_start = 1;
    let mut value_end = value_start;
    let chars: Vec<char> = after_colon.chars().collect();

    while value_end < chars.len() {
        if chars[value_end] == '\\' && value_end + 1 < chars.len() {
            value_end += 2;
            continue;
        }
        if chars[value_end] == '"' {
            break;
        }
        value_end += 1;
    }

    if value_end >= chars.len() {
        return None;
    }

    let value: String = chars[value_start..value_end].iter().collect();
    Some(value)
}

fn extract_number_field(json: &str, field_name: &str) -> Option<u32> {
    let pattern = alloc::format!("\"{}\"", field_name);
    let field_start = json.find(&pattern)?;
    let after_field = &json[field_start + pattern.len()..];
    let colon_pos = after_field.find(':')?;
    let after_colon = after_field[colon_pos + 1..].trim_start();

    let mut num_str = String::new();
    for ch in after_colon.chars() {
        if ch.is_ascii_digit() {
            num_str.push(ch);
        } else {
            break;
        }
    }
    parse_u32(&num_str)
}

fn format_u32(n: u32) -> String {
    if n == 0 {
        return String::from("0");
    }
    let mut result = String::new();
    let mut num = n;
    let mut digits = Vec::new();
    while num > 0 {
        digits.push((num % 10) as u8 + b'0');
        num /= 10;
    }
    for &digit in digits.iter().rev() {
        result.push(digit as char);
    }
    result
}

fn format_taskbar_clock(
    state: Option<TimeStateReplyMsg>,
    now_tick: u64,
) -> ([u8; 8], usize) {
    let mut out = [0u8; 8];
    let Some(state) = state.filter(|state| state.synced) else {
        out[..5].copy_from_slice(b"--:--");
        return (out, 5);
    };

    let seconds = state.local_unix_seconds(now_tick).rem_euclid(86_400);
    let mut hour = (seconds / 3_600) as u8;
    let minute = ((seconds % 3_600) / 60) as u8;
    if state.format_24h {
        out[0] = b'0' + hour / 10;
        out[1] = b'0' + hour % 10;
        out[2] = b':';
        out[3] = b'0' + minute / 10;
        out[4] = b'0' + minute % 10;
        (out, 5)
    } else {
        let pm = hour >= 12;
        hour %= 12;
        if hour == 0 {
            hour = 12;
        }
        let mut pos = 0;
        if hour >= 10 {
            out[pos] = b'0' + hour / 10;
            pos += 1;
        }
        out[pos] = b'0' + hour % 10;
        pos += 1;
        out[pos] = b':';
        pos += 1;
        out[pos] = b'0' + minute / 10;
        out[pos + 1] = b'0' + minute % 10;
        out[pos + 2] = b' ';
        out[pos + 3] = if pm { b'P' } else { b'A' };
        out[pos + 4] = b'M';
        (out, pos + 5)
    }
}

fn parse_u32(s: &str) -> Option<u32> {
    if s.is_empty() {
        return None;
    }
    let mut result: u32 = 0;
    for ch in s.chars() {
        if !ch.is_ascii_digit() {
            return None;
        }
        let digit = (ch as u8 - b'0') as u32;
        result = result.checked_mul(10)?;
        result = result.checked_add(digit)?;
    }
    Some(result)
}

fn validate_wallpaper_path(path: &str) -> bool {
    if path.is_empty() || path.len() > ApplyWallpaperMsg::MAX_PATH_LEN {
        return false;
    }
    if !path.starts_with("/system/wallpapers/") || path.contains("..") {
        return false;
    }
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".jpg") || lower.ends_with(".jpeg")
}

fn wallpaper_png_sidecar_path(path: &str) -> Option<String> {
    if path.as_bytes().len() < 5 {
        return None;
    }
    let ext_start = path.rfind('.')?;
    let ext = &path[ext_start..];
    if ext.eq_ignore_ascii_case(".jpg") || ext.eq_ignore_ascii_case(".jpeg") {
        let mut png = String::from(&path[..ext_start]);
        png.push_str(".png");
        Some(png)
    } else {
        None
    }
}

struct Compositor {
    fb: Framebuffer,
    backbuffer_fb: Framebuffer,
    backbuffer_region: SharedRegionId,
    backbuffer_ptr: *mut u32,
    wm: WindowManager,
    cursor: CursorState,
    event_port: PortId,
    register_port: PortId,
    pending_windows: Vec<PendingWindow>,
    pending_close: Vec<PendingClose>,
    dirty: bool,
    dirty_rects: Vec<Rect>,
    last_composite_tick: u64,
    mouse_left_down: bool,
    mouse_right_down: bool,
    drag_op: DragOperation,
    captured_window: Option<WindowId>,
    mouse_driver: MouseDriver,
    keyboard_shift: bool,
    desktop_bg: Color,
    icons: Vec<DesktopIcon>,
    dock_apps: Vec<DockApp>,
    ticks: u32,
    click_counter: u32,
    last_click_tick: u32,
    last_click_icon: Option<usize>,
    context_menu: ContextMenu,
    desktop_config: DesktopConfig,
    current_wallpaper_source: CurrentWallpaperSource,
    current_scaling_mode: ScalingMode,
    wallpaper_cache_region: Option<SharedRegionId>,
    wallpaper_cache_ptr: *mut u8,
    wallpaper_cache_valid: bool,
    wallpaper_recompute_pending: bool,
    config_save_pending: bool,
    image_cache: libimage::ImageCache,
    time_service_port: Option<PortId>,
    clock_state: Option<TimeStateReplyMsg>,
    last_clock_query_tick: u64,
    last_clock_minute: i64,
    audio_service_port: Option<PortId>,
    audio_state: Option<AudioStateReplyMsg>,
    last_audio_query_tick: u64,
    startup_chime_requested: bool,
}

impl Compositor {
    fn new(fb: Framebuffer) -> Self {
        let width = fb.width();
        let height = fb.height();

        let event_port = create_port().expect("Failed to create event port");
        let register_port = create_port().expect("Failed to create registration port");

        let mut mouse_driver = MouseDriver::new();
        mouse_driver.init();

        let _ = register_irq_handler(1, event_port);
        let _ = register_irq_handler(12, event_port);

        let icons = Self::build_desktop_icons();
        let dock_apps = Self::build_dock_apps();

        let bb_size = (fb.stride() * fb.height() * 4) as usize;
        let bb_region = shared_region_create(bb_size).expect("Failed to create backbuffer region");
        let bb_ptr = shared_region_map(bb_region, 0, SharedMemFlags::READ_WRITE)
            .expect("Failed to map backbuffer region") as *mut u32;

        let backbuffer_fb = Framebuffer::new_custom(
            bb_ptr as u64,
            fb.width(),
            fb.height(),
            fb.stride(),
            fb.bytes_per_pixel() as u32,
        )
        .expect("Failed to create backbuffer framebuffer");

        Self {
            fb,
            backbuffer_fb,
            backbuffer_region: bb_region,
            backbuffer_ptr: bb_ptr,
            wm: WindowManager::new(),
            cursor: CursorState::new(width, height),
            event_port,
            register_port,
            pending_windows: Vec::new(),
            pending_close: Vec::new(),
            dirty: true,
            dirty_rects: Vec::new(),
            last_composite_tick: 0,
            mouse_left_down: false,
            mouse_right_down: false,
            drag_op: DragOperation::None,
            captured_window: None,
            mouse_driver,
            keyboard_shift: false,
            desktop_bg: theme::DESKTOP_BG,
            icons,
            dock_apps,
            ticks: 0,
            click_counter: 0,
            last_click_tick: 0,
            last_click_icon: None,
            context_menu: ContextMenu {
                x: 0,
                y: 0,
                visible: false,
                items: {
                    let mut v = Vec::new();
                    v.push(String::from("Change Wallpaper"));
                    v
                },
            },
            desktop_config: DesktopConfig::default_config(width, height),
            current_wallpaper_source: CurrentWallpaperSource::SolidColor { rgb: 0x12141C },
            current_scaling_mode: ScalingMode::Fill,
            wallpaper_cache_region: None,
            wallpaper_cache_ptr: core::ptr::null_mut(),
            wallpaper_cache_valid: false,
            wallpaper_recompute_pending: false,
            config_save_pending: false,
            image_cache: libimage::ImageCache::with_capacity(1),
            time_service_port: None,
            clock_state: None,
            last_clock_query_tick: 0,
            last_clock_minute: -1,
            audio_service_port: None,
            audio_state: None,
            last_audio_query_tick: 0,
            startup_chime_requested: false,
        }
    }

    fn build_desktop_icons() -> Vec<DesktopIcon> {
        let mut icons = Vec::new();
        icons.push(DesktopIcon {
            label: String::from("Files"),
            executable: String::from("fileman"),
            x: 28,
            y: 28,
            color: atom_theme::colors::ATOM_COLOR_ACCENT_GLOW,
        });
        icons.push(DesktopIcon {
            label: String::from("Rectangles"),
            executable: String::from("demo_rects"),
            x: 28,
            y: 120,
            color: atom_theme::colors::ATOM_COLOR_ACCENT,
        });
        icons.push(DesktopIcon {
            label: String::from("Text"),
            executable: String::from("demo_text"),
            x: 28,
            y: 212,
            color: atom_theme::colors::ATOM_COLOR_SUCCESS,
        });
        icons
    }

    fn build_dock_apps() -> Vec<DockApp> {
        let candidates: [(&str, &str, Color); 4] = [
            ("fileman", "Files", atom_theme::colors::ATOM_COLOR_ACCENT_GLOW),
            ("system_settings", "Settings", atom_theme::colors::ATOM_COLOR_ACCENT),
            ("tinygl_demo", "TinyGL", atom_theme::colors::ATOM_COLOR_SUCCESS),
            ("terminal", "Terminal", atom_theme::colors::ATOM_GRADIENT_PRIMARY_END),
        ];

        let mut apps = Vec::new();
        for (exec, label, color) in candidates.iter() {
            let sys_path = alloc::format!("/apps/system/{}.atxf", exec);
            let user_path = alloc::format!("/apps/user/{}.atxf", exec);
            let exists = fs::stat(&sys_path).is_ok() || fs::stat(&user_path).is_ok();

            if exists {
                // A single, capitalised initial from the friendly name reads like
                // an app-icon glyph instead of a cryptic 2-letter exec prefix.
                let initial = label.chars().next().unwrap_or('?').to_ascii_uppercase();
                let mut mono = String::new();
                mono.push(initial);
                apps.push(DockApp {
                    executable: String::from(*exec),
                    color: *color,
                    monogram: mono,
                });
            }
        }
        apps
    }

    fn run(&mut self) -> ! {
        self.load_persisted_config();
        self.flush_deferred_desktop_work();
        self.draw_all();

        let mut reg_buffer = [0u8; 64];

        // Per-pass budget on drained IPC messages. The drain loops must not be
        // unbounded: a client that sends in a tight loop can keep the queue
        // non-empty forever, and the compositor would never get back to
        // drawing (frozen screen while the system stays alive).
        const MAX_MSGS_PER_PASS: usize = 64;

        let mut envelope = IpcEnvelope::default();

        loop {
            let ports = [self.register_port, self.event_port];

            for _ in 0..MAX_MSGS_PER_PASS {
                // Receive with the envelope so app registrations can be bound to
                // their window by the kernel-authenticated sender PID instead of
                // by arrival order.
                match recv_envelope(self.register_port, &mut envelope, &mut reg_buffer) {
                    Ok(Some(len)) => {
                        let sender_pid = envelope.sender_process;
                        self.handle_register_message(&reg_buffer[..len], sender_pid);
                    }
                    _ => break,
                }
            }

            let mut event_buffer = [0u8; 64];
            for _ in 0..MAX_MSGS_PER_PASS {
                match try_recv(self.event_port, &mut event_buffer) {
                    Ok(Some(len)) => self.handle_app_event(&event_buffer[..len]),
                    _ => break,
                }
            }

            self.poll_input();

            self.reap_pending_closes();
            self.flush_deferred_desktop_work();
            self.refresh_clock();
            self.refresh_audio();

            if self.dirty {
                // Frame pacing: only composite once per MIN_FRAME_TICKS. A
                // present flood still wakes us (to accumulate damage), but the
                // expensive snapshot+composite work is bounded to the frame
                // rate instead of running back-to-back at 100% CPU. Snapshots
                // are taken here, coalesced with the draw, since a pending
                // snapshot always implies dirty.
                let now = get_ticks();
                if now.wrapping_sub(self.last_composite_tick) >= MIN_FRAME_TICKS {
                    self.flush_pending_snapshots();
                    self.draw_all();
                    self.dirty = false;
                    self.last_composite_tick = now;
                }
            }

            let _ = wait_any(&ports, 10);
            self.ticks = self.ticks.wrapping_add(1);
        }
    }

    fn poll_input(&mut self) {
        while let Some(event) = self.mouse_driver.poll_event() {
            let old_x = self.cursor.x;
            let old_y = self.cursor.y;
            self.cursor
                .apply_delta(event.dx, event.dy, self.fb.width(), self.fb.height());

            let r1 = Rect::new(old_x, old_y, 24, 24);
            let r2 = Rect::new(self.cursor.x, self.cursor.y, 24, 24);
            self.mark_dirty_rect(r1.union(&r2));

            if event.dz != 0 {
                self.dispatch_mouse_scroll(self.cursor.x, self.cursor.y, event.dz);
            }

            if event.left_button {
                if !self.mouse_left_down {
                    self.click_counter = self.click_counter.wrapping_add(1);
                    self.handle_click(self.cursor.x, self.cursor.y);
                } else {
                    self.handle_mouse_drag(self.cursor.x, self.cursor.y);
                }
                if matches!(self.drag_op, DragOperation::None) {
                    self.dispatch_mouse_move(
                        self.cursor.x,
                        self.cursor.y,
                        event.dx as i16,
                        event.dy as i16,
                    );
                }
                self.mouse_left_down = true;
            } else if event.right_button {
                if !self.mouse_right_down {
                    self.handle_right_click(self.cursor.x, self.cursor.y);
                }
                self.mouse_right_down = true;
            } else {
                if self.mouse_left_down {
                    self.handle_mouse_up(self.cursor.x, self.cursor.y);
                } else {
                    self.dispatch_mouse_move(
                        self.cursor.x,
                        self.cursor.y,
                        event.dx as i16,
                        event.dy as i16,
                    );
                }
                self.mouse_left_down = false;
                self.mouse_right_down = false;
            }
        }

        while let Some(scancode) = keyboard_poll() {
            let pressed = (scancode & 0x80) == 0;
            let code = scancode & 0x7F;

            match code {
                0x60 => {}
                scancodes::LEFT_SHIFT | scancodes::RIGHT_SHIFT => {
                    self.keyboard_shift = pressed;
                    self.dispatch_key_event(code, 0, pressed);
                }
                _ => {
                    let ascii = if pressed {
                        scancode_to_ascii(code, self.keyboard_shift)
                            .map(|c| c as u8)
                            .unwrap_or(0)
                    } else {
                        0
                    };
                    self.dispatch_key_event(code, ascii, pressed);
                }
            }
        }
    }

    fn dispatch_mouse_scroll(&mut self, x: i32, y: i32, dz: i32) {
        let target_id = self
            .captured_window
            .or_else(|| self.wm.window_at(x, y))
            .or(self.wm.focused_id);

        if let Some(id) = target_id {
            let target = self
                .wm
                .get_window(id)
                .and_then(|w| w.event_port.map(|port| (port, w.content_x(), w.content_y())));

            if let Some((port, content_x, content_y)) = target {
                let rel_x = x - content_x as i32;
                let rel_y = y - content_y as i32;
                let event = MouseScrollEvent { dz, x: rel_x, y: rel_y };
                self.send_window_event_async(id, port, MessageType::MouseScroll, &event.to_bytes());
            }
        }
    }

    fn dispatch_mouse_move(&mut self, x: i32, y: i32, dx: i16, dy: i16) {
        let target_id = self.captured_window.or_else(|| self.wm.window_at(x, y));

        if let Some(id) = target_id {
            let target = self
                .wm
                .get_window(id)
                .and_then(|w| w.event_port.map(|port| (port, w.content_x(), w.content_y())));

            if let Some((port, content_x, content_y)) = target {
                let rel_x = x - content_x as i32;
                let rel_y = y - content_y as i32;
                let event = MouseMoveEvent { x: rel_x, y: rel_y, dx, dy };
                self.send_window_event_async(id, port, MessageType::MouseMove, &event.to_bytes());
            }
        }
    }

    fn dispatch_key_event(&mut self, scancode: u8, ascii: u8, pressed: bool) {
        let event = KeyEvent {
            scancode,
            character: ascii,
            modifiers: KeyModifiers {
                shift: self.keyboard_shift,
                ctrl: false,
                alt: false,
                caps_lock: false,
            },
        };

        let target = self.wm.focused_id.and_then(|id| {
            self.wm
                .get_window(id)
                .and_then(|w| w.event_port.map(|port| (id, port)))
        });

        if let Some((window_id, port)) = target {
            let primary_type = if pressed { MessageType::KeyDown } else { MessageType::KeyUp };
            self.send_window_event_async(window_id, port, primary_type, &event.to_bytes());

            if pressed && ascii != 0 {
                self.send_window_event_async(
                    window_id,
                    port,
                    MessageType::KeyPress,
                    &event.to_bytes(),
                );
            }
        }
    }

    fn send_window_event_async(
        &mut self,
        window_id: WindowId,
        port: PortId,
        msg_type: MessageType,
        payload: &[u8],
    ) {
        if port == 0 {
            return;
        }
        if let Err(err) = send_message_async(port, msg_type, payload) {
            if matches!(err, SyscallError::NotFound | SyscallError::InvalidArgument) {
                self.drop_dead_window(window_id);
            }
        }
    }

    fn drop_dead_window(&mut self, window_id: WindowId) {
        self.pending_close.retain(|pc| pc.window_id != window_id);

        if self.captured_window == Some(window_id) {
            self.captured_window = None;
        }

        match self.drag_op {
            DragOperation::Move { window_id: id, .. }
            | DragOperation::Resize { window_id: id, .. }
                if id == window_id =>
            {
                self.drag_op = DragOperation::None;
            }
            _ => {}
        }

        let rect = self
            .wm
            .get_window(window_id)
            .map(|w| Rect::new(w.x - 10, w.y - 10, w.width + 20, w.height + 25));

        self.wm.close_window(window_id);

        if let Some(r) = rect {
            self.mark_dirty_rect(r);
        }
        self.dirty = true;
    }

    fn handle_register_message(&mut self, data: &[u8], sender_pid: u64) {
        if data.len() < MessageHeader::SIZE {
            return;
        }
        let header = match MessageHeader::from_bytes(data) {
            Some(h) => h,
            None => return,
        };

        match header.msg_type {
            MessageType::AppRegister => self.handle_app_registration(data, sender_pid),
            MessageType::WmRequest => self.handle_wm_request(&data[MessageHeader::SIZE..]),
            MessageType::WmCommitFrame => {
                if let Some(msg) =
                    libipc::messages::WmCommitFrameMsg::from_bytes(&data[MessageHeader::SIZE..])
                {
                    self.present_window(msg.window_id);
                }
            }
            MessageType::SurfacePresent => {
                let payload_start = MessageHeader::SIZE;
                if data.len() >= payload_start + SurfacePresentMsg::SIZE {
                    if let Some(msg) = SurfacePresentMsg::from_bytes(&data[payload_start..]) {
                        self.present_window(msg.window_id);
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_app_registration(&mut self, data: &[u8], sender_pid: u64) {
        if data.len() < MessageHeader::SIZE {
            return;
        }
        let header = match MessageHeader::from_bytes(data) {
            Some(h) => h,
            None => return,
        };
        if header.msg_type != MessageType::AppRegister {
            return;
        }

        let payload_start = MessageHeader::SIZE;
        if data.len() < payload_start + AppRegisterMsg::SIZE {
            return;
        }

        let reg_msg = match AppRegisterMsg::from_bytes(&data[payload_start..]) {
            Some(m) => m,
            None => return,
        };

        // Bind the registering app to the window launched for its PID. The PID
        // comes from the kernel-authenticated IPC envelope, so it cannot be
        // forged and does not depend on registrations arriving in launch order
        // — the bug where File Manager (slow to register) had its content land
        // in a later app's window. Fall back to the oldest pending window only
        // when no PID match exists yet (e.g. the launch reply has not been
        // processed), preserving correct behaviour for a lone in-flight launch.
        let index = self
            .pending_windows
            .iter()
            .position(|p| p.pid != 0 && p.pid == sender_pid)
            .unwrap_or(0);

        if index < self.pending_windows.len() {
            let pending = self.pending_windows.remove(index);

            let result = if let Some(window) = self.wm.get_window_mut(pending.window_id) {
                window.event_port = Some(reg_msg.app_port);
                window
                    .surface_region_id
                    .map(|rid| (rid, window.content_width(), window.content_height()))
            } else {
                None
            };

            if let Some((region_id, cw, ch)) = result {
                self.send_surface_assignment(
                    pending.window_id,
                    reg_msg.app_port,
                    region_id,
                    cw,
                    ch,
                    None,
                );
            }
        }
    }

    /// Process an `AppLaunchReply` from the app launcher. Replies arrive in
    /// launch order from the (sequential) launcher, so each maps to the oldest
    /// pending window that has not yet learned its PID.
    ///
    /// On success the reply's PID is stamped onto that window, so the app's
    /// later registration can be matched to it by authenticated sender PID. On
    /// failure the placeholder window is dropped: the app will never register,
    /// and leaving its entry in the queue is exactly what used to shift every
    /// subsequent app's content into the wrong window.
    fn handle_launch_reply(&mut self, reply: &AppLaunchReplyMsg) {
        let index = match self.pending_windows.iter().position(|p| p.pid == 0) {
            Some(i) => i,
            None => return,
        };

        if reply.status == launch_status::LAUNCH_OK && reply.pid != 0 {
            self.pending_windows[index].pid = reply.pid;
        } else {
            let window_id = self.pending_windows.remove(index).window_id;
            self.handle_close_window(window_id);
        }
    }

    fn handle_app_event(&mut self, data: &[u8]) {
        if data.len() < MessageHeader::SIZE {
            return;
        }
        let header = match MessageHeader::from_bytes(data) {
            Some(h) => h,
            None => return,
        };

        match header.msg_type {
            MessageType::IrqNotification => {
                self.poll_input();
            }
            MessageType::WmRequest => {
                self.handle_wm_request(&data[MessageHeader::SIZE..]);
            }
            MessageType::WmCommitFrame => {
                if let Some(msg) =
                    libipc::messages::WmCommitFrameMsg::from_bytes(&data[MessageHeader::SIZE..])
                {
                    self.present_window(msg.window_id);
                }
            }
            MessageType::MouseMove => {
                let payload_start = MessageHeader::SIZE;
                if let Some(event) = MouseMoveEvent::from_bytes(&data[payload_start..]) {
                    let old_x = self.cursor.x;
                    let old_y = self.cursor.y;
                    self.cursor.apply_delta(
                        event.dx as i32,
                        event.dy as i32,
                        self.fb.width(),
                        self.fb.height(),
                    );
                    let r1 = Rect::new(old_x, old_y, 24, 24);
                    let r2 = Rect::new(self.cursor.x, self.cursor.y, 24, 24);
                    self.mark_dirty_rect(r1.union(&r2));
                }
            }
            MessageType::MouseButtonDown => {
                let payload_start = MessageHeader::SIZE;
                if let Some(event) = MouseButtonEvent::from_bytes(&data[payload_start..]) {
                    if event.button == MouseButton::Left {
                        if !self.mouse_left_down {
                            self.click_counter = self.click_counter.wrapping_add(1);
                            self.handle_click(self.cursor.x, self.cursor.y);
                        }
                        self.mouse_left_down = true;
                    }
                }
            }
            MessageType::MouseButtonUp => {
                let payload_start = MessageHeader::SIZE;
                if let Some(event) = MouseButtonEvent::from_bytes(&data[payload_start..]) {
                    if event.button == MouseButton::Left {
                        self.mouse_left_down = false;
                    }
                }
            }
            MessageType::AppLaunchReply => {
                if data.len() >= MessageHeader::SIZE + AppLaunchReplyMsg::SIZE {
                    if let Some(reply) = AppLaunchReplyMsg::from_bytes(&data[MessageHeader::SIZE..])
                    {
                        self.handle_launch_reply(&reply);
                    }
                }
            }
            MessageType::VideoModeChanged => {
                self.handle_video_mode_changed();
            }
            MessageType::ApplyWallpaper => {
                if let Some(msg) = ApplyWallpaperMsg::from_bytes(&data[MessageHeader::SIZE..]) {
                    self.handle_apply_wallpaper_msg(&msg);
                } else {
                    self.send_wallpaper_failed("Invalid wallpaper request");
                }
            }
            MessageType::TimeStateReply => {
                if let Some(state) =
                    TimeStateReplyMsg::from_bytes(&data[MessageHeader::SIZE..])
                {
                    self.clock_state = Some(state);
                    self.last_clock_minute = -1;
                    self.mark_taskbar_dirty();
                }
            }
            MessageType::AudioStateReply => {
                if let Some(state) =
                    AudioStateReplyMsg::from_bytes(&data[MessageHeader::SIZE..])
                {
                    self.audio_state = Some(state);
                    self.mark_taskbar_dirty();
                }
            }
            MessageType::SurfacePresent => {
                let payload_start = MessageHeader::SIZE;
                if data.len() >= payload_start + SurfacePresentMsg::SIZE {
                    if let Some(msg) = SurfacePresentMsg::from_bytes(&data[payload_start..]) {
                        self.present_window(msg.window_id);
                    }
                }
            }
            MessageType::KeyPress => {
                let payload_start = MessageHeader::SIZE;
                if data.len() >= payload_start + 3 {
                    if let Some(key_event) = KeyEvent::from_bytes(&data[payload_start..]) {
                        let event_port = self
                            .wm
                            .focused_id
                            .and_then(|id| self.wm.get_window(id).and_then(|w| w.event_port));

                        if let Some(focused_id) = self.wm.focused_id {
                            if let Some(port) = event_port {
                                self.send_window_event_async(
                                    focused_id,
                                    port,
                                    MessageType::KeyPress,
                                    &key_event.to_bytes(),
                                );
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_wm_request(&mut self, payload: &[u8]) {
        if payload.len() < 4 {
            return;
        }
        let req_type_raw =
            u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
        let req_type = match libipc::messages::WmRequestType::from_u32(req_type_raw) {
            Some(t) => t,
            None => return,
        };

        match req_type {
            libipc::messages::WmRequestType::CreateWindow => {
                if let Some(req) = libipc::messages::WmCreateWindowRequest::from_bytes(payload) {
                    let win_x = 150 + (self.wm.windows.len() as i32 * 30);
                    let win_y = 120 + (self.wm.windows.len() as i32 * 30);

                    let id = self.wm.next_id;
                    self.wm.next_id += 1;

                    let outer_w = req.width + WINDOW_BORDER_WIDTH * 2;
                    let outer_h = req.height + WINDOW_HEADER_HEIGHT + WINDOW_BORDER_WIDTH;

                    let window = match Window::new_with_process(
                        id,
                        &req.title,
                        win_x,
                        win_y,
                        outer_w,
                        outer_h,
                        req.reply_port as PortId,
                    ) {
                        Some(w) => w,
                        None => return,
                    };

                    let resp = libipc::messages::WmCreateWindowResponse {
                        window_id: id,
                        region_id: window.surface_region_id.unwrap_or(0),
                        width: window.content_width(),
                        height: window.content_height(),
                        stride: window.content_width(),
                    };

                    self.wm.windows.push(window);
                    self.wm.focus_window(id);
                    self.mark_window_dirty(id);
                    self.mark_taskbar_dirty();

                    let header = MessageHeader::new(
                        MessageType::WmResponse,
                        libipc::messages::WmCreateWindowResponse::SIZE as u32,
                    );
                    let mut full_msg = [0u8; 64];
                    full_msg[..MessageHeader::SIZE].copy_from_slice(&header.to_bytes());
                    full_msg[MessageHeader::SIZE
                        ..MessageHeader::SIZE
                            + libipc::messages::WmCreateWindowResponse::SIZE]
                        .copy_from_slice(&resp.to_bytes());
                    if req.reply_port != 0 {
                        let _ = send(
                            req.reply_port as PortId,
                            &full_msg[..MessageHeader::SIZE
                                + libipc::messages::WmCreateWindowResponse::SIZE],
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn send_surface_assignment(
        &self,
        window_id: WindowId,
        port: PortId,
        region_id: SharedRegionId,
        width: u32,
        height: u32,
        resize_pos: Option<(i32, i32)>,
    ) {
        if port == 0 {
            return;
        }

        let assign = SurfaceAssignMsg {
            window_id,
            region_id,
            width,
            height,
            stride: width,
            bytes_per_pixel: 4,
            compositor_port: self.event_port,
            scale_factor: 1000,
        };

        let header = MessageHeader::new(MessageType::SurfaceAssign, SurfaceAssignMsg::SIZE as u32);
        let mut full_msg = [0u8; 64];
        full_msg[..MessageHeader::SIZE].copy_from_slice(&header.to_bytes());
        full_msg[MessageHeader::SIZE..MessageHeader::SIZE + SurfaceAssignMsg::SIZE]
            .copy_from_slice(&assign.to_bytes());
        let _ = send(port, &full_msg[..MessageHeader::SIZE + SurfaceAssignMsg::SIZE]);

        if let Some((win_x, win_y)) = resize_pos {
            send_wm_window_event(
                port,
                window_id,
                libipc::messages::WindowEventType::Resize,
                win_x,
                win_y,
                width,
                height,
            );
        }
    }

    fn reallocate_window_surface(&mut self, window_id: WindowId) {
        let (cw, ch, already_correct) = if let Some(window) = self.wm.get_window(window_id) {
            let cw = window.content_width();
            let ch = window.content_height();
            let already_correct = if let Some(ref s) = window.surface {
                s.width() == cw && s.height() == ch
            } else {
                false
            };
            (cw, ch, already_correct)
        } else {
            return;
        };

        if already_correct {
            return;
        }

        if let Ok(new_surface) = SharedSurface::create(cw, ch) {
            let region_id = new_surface.region_id();

            let result = if let Some(window) = self.wm.get_window_mut(window_id) {
                window.surface = Some(new_surface);
                window.surface_region_id = Some(region_id);
                window.content_dirty = false;
                window.surface_ready = false;
                window.pending_snapshot = false;
                window.event_port.map(|port| (port, window.x, window.y))
            } else {
                None
            };

            if let Some((port, x, y)) = result {
                self.send_surface_assignment(window_id, port, region_id, cw, ch, Some((x, y)));
            }
        }
    }

    /// On-screen footprint of the context menu (frame + AA edge + drop shadow),
    /// matching the geometry painted by `draw_context_menu`.
    fn context_menu_rect(&self) -> Rect {
        let item_h = atom_theme::spacing::XXXL as u32;
        let padding_v = atom_theme::spacing::SM as u32 - 2;
        let menu_h = self.context_menu.items.len() as u32 * item_h + padding_v * 2;
        Rect::new(self.context_menu.x - 1, self.context_menu.y - 1, 200 + 4, menu_h + 6)
    }

    fn handle_click(&mut self, x: i32, y: i32) {
        if self.context_menu.visible {
            let menu_rect = self.context_menu_rect();
            if self.handle_context_menu_click(x, y) {
                self.context_menu.visible = false;
                self.mark_dirty_rect(menu_rect);
                return;
            }
            self.context_menu.visible = false;
            self.mark_dirty_rect(menu_rect);
        }

        if self.is_on_dock(x, y) {
            if self.sound_control_at(x, y) {
                self.toggle_audio_mute();
                return;
            }
            if let Some(icon_index) = self.dock_icon_at(x, y) {
                self.handle_dock_click(icon_index);
            }
            return;
        }

        if let Some(id) = self.wm.window_at(x, y) {
            if self.wm.focused_id != Some(id) {
                let prev = self.wm.focused_id;
                self.wm.focus_window(id);
                // Repaint both the newly focused window (it may have been behind
                // others) and the one that lost focus (its frame tint changes).
                if let Some(p) = prev {
                    self.mark_window_dirty(p);
                }
                self.mark_window_dirty(id);
                // The taskbar's focus highlight tracks the active window.
                self.mark_taskbar_dirty();
            }

            self.captured_window = Some(id);

            if let Some(w) = self.wm.get_window(id) {
                let (btn_y, btn_size, close_x, max_x, min_x) =
                    window_button_layout(w.x, w.y, w.width);
                let pad = WINDOW_BTN_HIT_PAD;
                let hit = |bx: i32| {
                    x >= bx - pad
                        && x < bx + btn_size + pad
                        && y >= btn_y - pad
                        && y < btn_y + btn_size + pad
                };

                if w.state != WindowState::Minimized {
                    if hit(close_x) {
                        self.handle_close_window(id);
                        return;
                    } else if hit(max_x) {
                        let (wx, wy, ww, wh) = self.get_work_area();
                        let old_rect =
                            Rect::new(w.x - 10, w.y - 10, w.width + 20, w.height + 25);
                        if self.wm.toggle_maximize(id, wx, wy, ww, wh) {
                            self.mark_dirty_rect(old_rect);
                            self.mark_dirty_rect(Rect::new(wx - 10, wy - 10, ww + 20, wh + 25));
                            self.reallocate_window_surface(id);
                            self.dirty = true;
                        }
                        return;
                    } else if hit(min_x) {
                        let rect = Rect::new(w.x - 10, w.y - 10, w.width + 20, w.height + 25);
                        self.wm.minimize_window(id);
                        self.mark_dirty_rect(rect);
                        self.mark_taskbar_dirty();
                        self.dirty = true;
                        return;
                    }
                }

                if w.header_contains(x, y) {
                    self.drag_op = DragOperation::Move {
                        window_id: id,
                        start_mouse_x: x,
                        start_mouse_y: y,
                        start_win_x: w.x,
                        start_win_y: w.y,
                    };
                    return;
                }

                if x >= w.x + w.width as i32 - 16 && y >= w.y + w.height as i32 - 16 {
                    self.drag_op = DragOperation::Resize {
                        window_id: id,
                        start_mouse_x: x,
                        start_mouse_y: y,
                        start_win_w: w.width,
                        start_win_h: w.height,
                    };
                    return;
                }

                if let Some(port) = w.event_port {
                    let rel_x = x - w.content_x() as i32;
                    let rel_y = y - w.content_y() as i32;
                    if rel_x >= 0
                        && rel_y >= 0
                        && rel_x < w.content_width() as i32
                        && rel_y < w.content_height() as i32
                    {
                        let event = MouseButtonEvent {
                            button: MouseButton::Left,
                            x: rel_x,
                            y: rel_y,
                        };
                        self.send_window_event_async(
                            id,
                            port,
                            MessageType::MouseButtonDown,
                            &event.to_bytes(),
                        );
                    }
                }
            }
            return;
        }

        if let Some(icon_idx) = self.icon_at(x, y) {
            let current = self.click_counter;
            if self.last_click_icon == Some(icon_idx)
                && current.wrapping_sub(self.last_click_tick) < 2
            {
                let exe = self.icons[icon_idx].executable.clone();
                self.spawn_app(&exe);
                self.last_click_icon = None;
            } else {
                self.last_click_icon = Some(icon_idx);
                self.last_click_tick = current;
            }
            return;
        }
    }

    fn handle_mouse_drag(&mut self, x: i32, y: i32) {
        match self.drag_op {
            DragOperation::Move {
                window_id,
                start_mouse_x,
                start_mouse_y,
                start_win_x,
                start_win_y,
            } => {
                let dx = x - start_mouse_x;
                let dy = y - start_mouse_y;

                let screen_w = self.fb.width() as i32;
                let (old_rect, new_rect, clamped_x, clamped_y) =
                    if let Some(w) = self.wm.get_window(window_id) {
                        // Footprint matches mark_window_dirty / draw_all (h + 12
                        // from y - 4): the drop shadow extends a few rows below
                        // the frame, and missing them leaves trails when dragging.
                        let old = Rect::new(w.x - 4, w.y - 4, w.width + 8, w.height + 12);
                        // Keep the title bar reachable. The framebuffer primitives
                        // are u32-based and only clip on the high edge, so a
                        // negative x would wrap and render nothing (the "transparent"
                        // bug on the left). Pin the left edge at 0; the window may
                        // still slide partly off the right, where clipping is sound.
                        let min_visible = 80i32;
                        let max_x = (screen_w - min_visible).max(0);
                        let nx = (start_win_x + dx).clamp(0, max_x);
                        let ny = (start_win_y + dy).max(0);
                        let new = Rect::new(nx - 4, ny - 4, w.width + 8, w.height + 12);
                        (old, new, nx, ny)
                    } else {
                        return;
                    };

                if let Some(w) = self.wm.get_window_mut(window_id) {
                    w.x = clamped_x;
                    w.y = clamped_y;
                }

                self.mark_dirty_rect(old_rect);
                self.mark_dirty_rect(new_rect);
            }
            DragOperation::Resize {
                window_id,
                start_mouse_x,
                start_mouse_y,
                start_win_w,
                start_win_h,
            } => {
                let dx = x - start_mouse_x;
                let dy = y - start_mouse_y;
                let new_w = (start_win_w as i32 + dx).max(WINDOW_MIN_WIDTH as i32) as u32;
                let new_h = (start_win_h as i32 + dy).max(WINDOW_MIN_HEIGHT as i32) as u32;

                let (old_rect, new_rect) = if let Some(w) = self.wm.get_window(window_id) {
                    let old = Rect::new(w.x - 4, w.y - 4, w.width + 8, w.height + 12);
                    let new = Rect::new(w.x - 4, w.y - 4, new_w + 8, new_h + 12);
                    (old, new)
                } else {
                    return;
                };

                self.wm.resize_window(window_id, new_w, new_h);
                self.mark_dirty_rect(old_rect);
                self.mark_dirty_rect(new_rect);
            }
            DragOperation::None => {}
        }
    }

    fn handle_mouse_up(&mut self, x: i32, y: i32) {
        let captured = self.captured_window.take();

        match self.drag_op {
            DragOperation::Resize { window_id, .. } => {
                self.reallocate_window_surface(window_id);
                self.drag_op = DragOperation::None;
                return;
            }
            DragOperation::Move { .. } => {
                self.drag_op = DragOperation::None;
                return;
            }
            DragOperation::None => {}
        }

        let target_id = captured.or_else(|| self.wm.window_at(x, y));

        if let Some(id) = target_id {
            if let Some(w) = self.wm.get_window(id) {
                if let Some(port) = w.event_port {
                    let rel_x = x - w.content_x() as i32;
                    let rel_y = y - w.content_y() as i32;
                    if rel_x >= 0
                        && rel_y >= 0
                        && rel_x < w.content_width() as i32
                        && rel_y < w.content_height() as i32
                    {
                        let event = MouseButtonEvent {
                            button: MouseButton::Left,
                            x: rel_x,
                            y: rel_y,
                        };
                        self.send_window_event_async(
                            id,
                            port,
                            MessageType::MouseButtonUp,
                            &event.to_bytes(),
                        );
                    }
                }
            }
        }

        self.drag_op = DragOperation::None;
    }

    fn handle_right_click(&mut self, x: i32, y: i32) {
        if self.context_menu.visible {
            let old_rect = self.context_menu_rect();
            self.mark_dirty_rect(old_rect);
        }
        self.context_menu.visible = false;

        if self.wm.window_at(x, y).is_some() {
            return;
        }
        if self.is_on_dock(x, y) {
            return;
        }
        if self.icon_at(x, y).is_some() {
            return;
        }

        self.context_menu.x = x;
        self.context_menu.y = y;
        self.context_menu.visible = true;
        let menu_rect = self.context_menu_rect();
        self.mark_dirty_rect(menu_rect);
    }

    fn handle_context_menu_click(&mut self, x: i32, y: i32) -> bool {
        let menu_w = 180u32;
        let item_h = 32u32;
        let menu_h = self.context_menu.items.len() as u32 * item_h;

        if x >= self.context_menu.x
            && x < self.context_menu.x + menu_w as i32
            && y >= self.context_menu.y
            && y < self.context_menu.y + menu_h as i32
        {
            let item_idx = (y - self.context_menu.y) as u32 / item_h;
            if (item_idx as usize) < self.context_menu.items.len() {
                let action = &self.context_menu.items[item_idx as usize];
                if action == "Change Wallpaper" {
                    self.open_display_settings_wallpaper_tab();
                }
                return true;
            }
        }
        false
    }

    fn handle_close_window(&mut self, id: WindowId) {
        let mut event_port = None;
        if let Some(w) = self.wm.get_window(id) {
            event_port = w.event_port;
        }

        if let Some(port) = event_port {
            if port != 0 {
                let msg = TerminateRequestMsg { window_id: id, reason: 0 };
                let header = MessageHeader::new(
                    MessageType::TerminateRequest,
                    TerminateRequestMsg::SIZE as u32,
                );
                let mut full_msg = [0u8; 64];
                full_msg[..MessageHeader::SIZE].copy_from_slice(&header.to_bytes());
                full_msg[MessageHeader::SIZE..MessageHeader::SIZE + TerminateRequestMsg::SIZE]
                    .copy_from_slice(&msg.to_bytes());
                let _ = send(port, &full_msg[..MessageHeader::SIZE + TerminateRequestMsg::SIZE]);
            }
        }

        let rect = self
            .wm
            .get_window(id)
            .map(|w| Rect::new(w.x - 10, w.y - 10, w.width + 20, w.height + 25));

        if let Some(w) = self.wm.get_window_mut(id) {
            w.visible = false;
        }

        if let Some(r) = rect {
            self.mark_dirty_rect(r);
        }
        // The closing window drops out of the taskbar (and a pinned launcher may
        // take its place) — repaint the bar.
        self.mark_taskbar_dirty();

        if self.wm.focused_id == Some(id) {
            // The window is no longer visible (set above), so refocus_topmost
            // will skip it and hand focus to the remaining top-most window.
            self.wm.refocus_topmost();
        }

        self.pending_close.push(PendingClose {
            window_id: id,
            deadline_tick: self.ticks.wrapping_add(CLOSE_GRACE_TICKS),
        });

        self.dirty = true;
    }

    fn reap_pending_closes(&mut self) {
        let current = self.ticks;
        let mut reaped = false;
        let mut to_close: Vec<WindowId> = Vec::new();
        for pc in self.pending_close.iter() {
            if current.wrapping_sub(pc.deadline_tick) < 0x8000_0000 {
                to_close.push(pc.window_id);
            }
        }
        for id in to_close.iter() {
            self.wm.close_window(*id);
            reaped = true;
        }
        self.pending_close
            .retain(|pc| current.wrapping_sub(pc.deadline_tick) >= 0x8000_0000);
        if reaped {
            self.mark_taskbar_dirty();
            self.dirty = true;
        }
    }

    /// Mark a window's full on-screen footprint (frame + drop shadow) dirty so
    /// the next `draw_all` actually repaints it. Without an explicit footprint,
    /// a freshly presented or freshly raised window could stay invisible until
    /// unrelated damage happened to cross it.
    fn mark_window_dirty(&mut self, window_id: WindowId) {
        if let Some(w) = self.wm.get_window(window_id) {
            let rect = Rect::new(w.x - 4, w.y - 4, w.width + 8, w.height + 12);
            self.mark_dirty_rect(rect);
        }
    }

    /// Record that an app committed/presented a frame and mark its footprint
    /// dirty. The snapshot copy itself is deferred to `flush_pending_snapshots`
    /// so that any number of commits received in one loop pass cost one copy:
    /// this handler must stay cheap because it runs inside the IPC drain loop,
    /// where a client presenting in a tight loop can otherwise feed messages
    /// faster than full-surface copies can drain them (compositor livelock).
    fn present_window(&mut self, window_id: WindowId) {
        let rect = match self.wm.get_window_mut(window_id) {
            Some(window) => {
                window.content_dirty = true;
                window.pending_snapshot = true;
                Rect::new(window.x - 4, window.y - 4, window.width + 8, window.height + 12)
            }
            None => return,
        };
        self.mark_dirty_rect(rect);
    }

    /// Perform the deferred front-buffer snapshots for every window that
    /// committed a frame since the last pass. Runs once per compositor loop,
    /// right before drawing, so each window is copied at most once per frame
    /// regardless of how many commits it sent.
    fn flush_pending_snapshots(&mut self) {
        for window in self.wm.windows.iter_mut() {
            if window.pending_snapshot {
                window.pending_snapshot = false;
                if window.snapshot_surface() {
                    window.surface_ready = true;
                }
            }
        }
    }

    /// Mark the entire screen dirty so the next `draw_all` repaints everything.
    /// Clears any accumulated damage rects first: a stale cursor-sized rect left
    /// in the list would otherwise clip the intended full repaint down to a few
    /// pixels (black screen / trails that only heal where the mouse moves).
    fn mark_dirty_all(&mut self) {
        self.dirty_rects.clear();
        self.dirty_rects
            .push(Rect::new(0, 0, self.fb.width(), self.fb.height()));
        self.dirty = true;
    }

    fn mark_dirty_rect(&mut self, rect: Rect) {
        let screen = Rect::new(0, 0, self.fb.width(), self.fb.height());
        let x1 = rect.x.max(0);
        let y1 = rect.y.max(0);
        let x2 = (rect.x + rect.w as i32).min(screen.w as i32);
        let y2 = (rect.y + rect.h as i32).min(screen.h as i32);

        if x2 <= x1 || y2 <= y1 {
            return;
        }
        let clamped = Rect::new(x1, y1, (x2 - x1) as u32, (y2 - y1) as u32);

        // Coalesce only with rects we already overlap. A window that presents
        // the same footprint every frame (an animation) keeps merging into its
        // own existing entry, so the list stays tiny and disjoint instead of
        // unioning unrelated corners of the screen into one near-full-screen
        // box. Non-overlapping damage stays a separate region and is composited
        // independently in `draw_all`.
        for existing in self.dirty_rects.iter_mut() {
            if existing.intersects(&clamped) {
                *existing = existing.union(&clamped);
                self.dirty = true;
                return;
            }
        }

        if self.dirty_rects.len() >= MAX_DIRTY_RECTS {
            // Too much scattered damage to track individually: fall back to a
            // single bounding box (the previous behaviour) to bound per-frame
            // bookkeeping. Correctness is preserved; only locality is lost.
            let mut acc = clamped;
            for existing in self.dirty_rects.drain(..) {
                acc = acc.union(&existing);
            }
            self.dirty_rects.push(acc);
        } else {
            self.dirty_rects.push(clamped);
        }
        self.dirty = true;
    }

    /// Re-read the active framebuffer from the kernel and reallocate the
    /// backbuffer to match. Used after any video-mode change (runtime or the
    /// one re-applied from persisted config at boot). Returns `false` — leaving
    /// the previous framebuffer untouched — if the new one could not be built,
    /// so callers never run against a half-torn-down framebuffer.
    fn rebuild_framebuffer(&mut self) -> bool {
        let new_fb = match Framebuffer::new() {
            Some(fb) => fb,
            None => return false,
        };

        // Allocate and map the new backbuffer *before* releasing the old one,
        // so an allocation failure leaves the current framebuffer intact.
        let bb_size = (new_fb.stride() * new_fb.height() * 4) as usize;
        let bb_region = match shared_region_create(bb_size) {
            Ok(region) => region,
            Err(_) => return false,
        };
        let bb_ptr = match shared_region_map(bb_region, 0, SharedMemFlags::READ_WRITE) {
            Ok(ptr) => ptr as *mut u32,
            Err(_) => {
                let _ = shared_region_destroy(bb_region);
                return false;
            }
        };
        let new_backbuffer_fb = match Framebuffer::new_custom(
            bb_ptr as u64,
            new_fb.width(),
            new_fb.height(),
            new_fb.stride(),
            new_fb.bytes_per_pixel() as u32,
        ) {
            Some(fb) => fb,
            None => {
                let _ = shared_region_unmap(bb_region);
                let _ = shared_region_destroy(bb_region);
                return false;
            }
        };

        let old_region = self.backbuffer_region;
        self.fb = new_fb;
        self.backbuffer_fb = new_backbuffer_fb;
        self.backbuffer_region = bb_region;
        self.backbuffer_ptr = bb_ptr;
        let _ = shared_region_unmap(old_region);
        let _ = shared_region_destroy(old_region);

        let w = self.fb.width() as i32;
        let h = self.fb.height() as i32;
        if self.cursor.x >= w {
            self.cursor.x = w - 1;
        }
        if self.cursor.y >= h {
            self.cursor.y = h - 1;
        }
        self.desktop_config.resolution =
            ResolutionConfig { width: self.fb.width(), height: self.fb.height() };

        self.wallpaper_cache_valid = false;

        // The fresh backbuffer is uninitialised and any pending damage is in the
        // old resolution's coordinates — repaint the whole new screen, or it
        // stays black except where later damage (e.g. the cursor) happens to land.
        self.dirty_rects.clear();
        self.mark_dirty_all();
        true
    }

    /// Re-apply a persisted resolution by programming the video mode and
    /// rebuilding the framebuffer. No-op when it already matches or the mode is
    /// unavailable on this hardware (in which case the current mode is kept).
    fn apply_resolution(&mut self, width: u32, height: u32) {
        if width == self.fb.width() && height == self.fb.height() {
            return;
        }
        if width == 0
            || height == 0
            || width > u16::MAX as u32
            || height > u16::MAX as u32
        {
            return;
        }
        if atom_syscall::graphics::set_video_mode(width as u16, height as u16, 32).is_err() {
            return;
        }
        if !self.rebuild_framebuffer() {
            log("ui_shell: framebuffer rebuild failed after resolution change");
        }
    }

    fn handle_video_mode_changed(&mut self) {
        if !self.rebuild_framebuffer() {
            return;
        }
        if matches!(self.current_wallpaper_source, CurrentWallpaperSource::Image { .. }) {
            self.wallpaper_recompute_pending = true;
        }
        self.config_save_pending = true;
        self.dirty = true;
    }

    fn is_on_dock(&self, x: i32, y: i32) -> bool {
        if let Some((dock_x, dock_y, dock_w, dock_h, _, _, _, _)) = self.dock_layout() {
            x >= dock_x
                && x < dock_x + dock_w as i32
                && y >= dock_y
                && y < dock_y + dock_h as i32
        } else {
            false
        }
    }

    fn get_work_area(&self) -> (i32, i32, u32, u32) {
        let sw = self.fb.width();
        let sh = self.fb.height();
        let x = 0;
        let y = 0;
        let w = sw;
        let h = sh.saturating_sub(DOCK_HEIGHT + 12);
        (x, y, w, h)
    }

    fn open_display_settings_wallpaper_tab(&mut self) {
        let port = match libipc::protocol::lookup_service("system_settings") {
            Ok(port) => port,
            Err(_) => {
                self.spawn_display_settings();
                let mut found = None;
                for _ in 0..80 {
                    if let Ok(port) = libipc::protocol::lookup_service("system_settings") {
                        found = Some(port);
                        break;
                    }
                    yield_now();
                }
                match found {
                    Some(port) => port,
                    None => return,
                }
            }
        };

        if let Some(id) = self
            .wm
            .windows
            .iter()
            .find(|w| w.title == "System Settings")
            .map(|w| w.id)
        {
            self.wm.focus_window(id);
            self.mark_window_dirty(id);
            self.mark_taskbar_dirty();
        }

        let msg = OpenInTabMsg {
            target_app: String::from("system_settings"),
            tab_name: String::from("Desktop Background"),
        };
        let payload = msg.to_bytes();
        let header = MessageHeader::new(MessageType::OpenInTab, payload.len() as u32);
        let mut buf = Vec::new();
        buf.extend_from_slice(&header.to_bytes());
        buf.extend_from_slice(&payload);
        let _ = send(port, &buf);
        self.dirty = true;
    }

    fn send_wallpaper_applied(&mut self) {
        if let Ok(port) = libipc::protocol::lookup_service("system_settings") {
            let msg = WallpaperAppliedMsg {};
            let header =
                MessageHeader::new(MessageType::WallpaperApplied, WallpaperAppliedMsg::SIZE as u32);
            let mut buf = Vec::new();
            buf.extend_from_slice(&header.to_bytes());
            buf.extend_from_slice(&msg.to_bytes());
            let _ = send(port, &buf);
        }
    }

    fn send_wallpaper_failed(&mut self, error: &str) {
        if let Ok(port) = libipc::protocol::lookup_service("system_settings") {
            let msg = WallpaperFailedMsg { error_message: String::from(error) };
            let payload = msg.to_bytes();
            let header = MessageHeader::new(MessageType::WallpaperFailed, payload.len() as u32);
            let mut buf = Vec::new();
            buf.extend_from_slice(&header.to_bytes());
            buf.extend_from_slice(&payload);
            let _ = send(port, &buf);
        }
    }

    fn handle_apply_wallpaper_msg(&mut self, msg: &ApplyWallpaperMsg) {
        let source_type = WallpaperSourceType::from_ipc(msg.source_type);
        let scaling_mode = ScalingMode::from_ipc(msg.scaling_mode);
        let config = match source_type {
            WallpaperSourceType::SolidColor => DesktopConfig {
                wallpaper: WallpaperConfig {
                    source_type: WallpaperSourceType::SolidColor,
                    image_path: None,
                    color_rgb: msg.color_rgb,
                    scaling_mode,
                },
                resolution: ResolutionConfig {
                    width: self.fb.width(),
                    height: self.fb.height(),
                },
            },
            WallpaperSourceType::Image => DesktopConfig {
                wallpaper: WallpaperConfig {
                    source_type: WallpaperSourceType::Image,
                    image_path: msg.image_path.clone(),
                    color_rgb: None,
                    scaling_mode,
                },
                resolution: ResolutionConfig {
                    width: self.fb.width(),
                    height: self.fb.height(),
                },
            },
        };

        match self.apply_config(config, true) {
            Ok(()) => self.send_wallpaper_applied(),
            Err(error) => {
                self.revert_to_fallback_wallpaper();
                self.send_wallpaper_failed(error);
            }
        }
    }

    fn load_persisted_config(&mut self) {
        let mut config = DesktopConfig::default_config(self.fb.width(), self.fb.height());
        if let Ok(bytes) = fs::read_file("/user/config/desktop.cfg") {
            if let Ok(text) = core::str::from_utf8(&bytes) {
                let trimmed = text.trim();
                if let Ok(parsed) = DesktopConfig::from_json(trimmed) {
                    config = parsed;
                } else if validate_wallpaper_path(trimmed) {
                    config.wallpaper.source_type = WallpaperSourceType::Image;
                    config.wallpaper.image_path = Some(String::from(trimmed));
                    config.wallpaper.color_rgb = None;
                    config.wallpaper.scaling_mode = ScalingMode::Fill;
                }
            }
        }
        if matches!(config.wallpaper.source_type, WallpaperSourceType::SolidColor) {
            if let Ok(bytes) = fs::read_file("/user/config/desktop.json") {
                if let Ok(text) = core::str::from_utf8(&bytes) {
                    let trimmed = text.trim();
                    if let Ok(parsed) = DesktopConfig::from_json(trimmed) {
                        config = parsed;
                    }
                }
            }
        }

        // Re-apply the persisted resolution first, so the wallpaper is scaled to
        // the final display size. set_video_mode is a runtime change, so the saved
        // mode must be programmed again on every boot. Then keep the in-memory
        // config in sync with the resolution actually in effect (the saved mode
        // may be unavailable on this hardware, in which case we keep the current).
        self.apply_resolution(config.resolution.width, config.resolution.height);
        config.resolution =
            ResolutionConfig { width: self.fb.width(), height: self.fb.height() };

        let _ = self.apply_config(config, false);
    }

    fn load_and_decode_image(&mut self, path: &str) -> Result<DecodedImage, &'static str> {
        if !validate_wallpaper_path(path) {
            return Err("Invalid wallpaper path");
        }
        let img = if let Some(sidecar_path) = wallpaper_png_sidecar_path(path) {
            match fs::read_file(&sidecar_path) {
                Ok(sidecar_data) => {
                    if sidecar_data.len() > 16 * 1024 * 1024 {
                        return Err("Image file too large");
                    }
                    match PngDecoder::decode(&sidecar_data) {
                        Ok(image) => image,
                        Err(_) => {
                            drop(sidecar_data);
                            let data =
                                fs::read_file(path).map_err(|_| "Image file not found")?;
                            if data.len() > 16 * 1024 * 1024 {
                                return Err("Image file too large");
                            }
                            JpgDecoder::decode(&data).map_err(|_| "Failed to decode image")?
                        }
                    }
                }
                Err(_) => {
                    let data = fs::read_file(path).map_err(|_| "Image file not found")?;
                    if data.len() > 16 * 1024 * 1024 {
                        return Err("Image file too large");
                    }
                    JpgDecoder::decode(&data).map_err(|_| "Failed to decode image")?
                }
            }
        } else {
            let data = fs::read_file(path).map_err(|_| "Image file not found")?;
            if data.len() > 16 * 1024 * 1024 {
                return Err("Image file too large");
            }
            JpgDecoder::decode(&data).map_err(|_| "Failed to decode image")?
        };
        if img.width == 0 || img.height == 0 || img.width > 4096 || img.height > 4096 {
            return Err("Image dimensions unsupported");
        }
        Ok(img)
    }

    fn apply_config(&mut self, config: DesktopConfig, persist: bool) -> Result<(), &'static str> {
        config.validate().map_err(|_| "Invalid desktop configuration")?;

        self.desktop_config = config.clone();
        self.current_scaling_mode = config.wallpaper.scaling_mode;
        match config.wallpaper.source_type {
            WallpaperSourceType::SolidColor => {
                let rgb = config.wallpaper.color_rgb.unwrap_or(0x12141C);
                self.current_wallpaper_source = CurrentWallpaperSource::SolidColor { rgb };
                self.wallpaper_cache_valid = false;
                self.wallpaper_recompute_pending = false;
                self.desktop_bg = Color::new(
                    ((rgb >> 16) & 0xFF) as u8,
                    ((rgb >> 8) & 0xFF) as u8,
                    (rgb & 0xFF) as u8,
                );
            }
            WallpaperSourceType::Image => {
                let path = config
                    .wallpaper
                    .image_path
                    .as_ref()
                    .ok_or("Missing image path")?
                    .clone();
                self.current_wallpaper_source = CurrentWallpaperSource::Image { path };
                self.wallpaper_cache_valid = false;
                self.wallpaper_recompute_pending = true;
            }
        }

        if persist {
            self.config_save_pending = true;
        }
        self.mark_dirty_all();
        Ok(())
    }

    fn save_desktop_config(&mut self) -> Result<(), &'static str> {
        self.desktop_config.validate().map_err(|_| "Invalid config")?;
        let json = self.desktop_config.to_json().map_err(|_| "Serialize failed")?;
        let tmp_path = "/user/config/desktop.tmp";
        let final_path = "/user/config/desktop.cfg";

        if fs::write_file(tmp_path, json.as_bytes()).is_ok() {
            match fs::rename(tmp_path, final_path) {
                Ok(()) => return Ok(()),
                Err(_) => {
                }
            }
        }
        fs::write_file(final_path, json.as_bytes()).map_err(|_| "Failed to write config")
    }

    fn revert_to_fallback_wallpaper(&mut self) {
        let fallback_rgb = match self.current_wallpaper_source {
            CurrentWallpaperSource::SolidColor { rgb } => rgb,
            CurrentWallpaperSource::Image { .. } => 0x12141C,
        };
        self.desktop_bg = Color::new(
            ((fallback_rgb >> 16) & 0xFF) as u8,
            ((fallback_rgb >> 8) & 0xFF) as u8,
            (fallback_rgb & 0xFF) as u8,
        );
        self.current_wallpaper_source = CurrentWallpaperSource::SolidColor { rgb: fallback_rgb };
        self.current_scaling_mode = ScalingMode::Fill;
        self.wallpaper_cache_valid = false;
        self.wallpaper_recompute_pending = false;
        self.desktop_config.wallpaper = WallpaperConfig {
            source_type: WallpaperSourceType::SolidColor,
            image_path: None,
            color_rgb: Some(fallback_rgb),
            scaling_mode: ScalingMode::Fill,
        };
        self.config_save_pending = true;
        self.mark_dirty_all();
    }

    fn flush_deferred_desktop_work(&mut self) {
        if self.wallpaper_recompute_pending {
            let image_path = match &self.current_wallpaper_source {
                CurrentWallpaperSource::Image { path } => Some(path.clone()),
                CurrentWallpaperSource::SolidColor { .. } => None,
            };

            if let Some(path) = image_path {
                const WALLPAPER_SCRATCH_SIZE: usize = 48 * 1024 * 1024;
                let result = match shared_region_create(WALLPAPER_SCRATCH_SIZE) {
                    Ok(scratch_region) => {
                        match shared_region_map(
                            scratch_region,
                            0,
                            SharedMemFlags::READ_WRITE,
                        ) {
                            Ok(scratch_start) => {
                                ALLOCATOR.push_scratch(
                                    scratch_start as usize,
                                    WALLPAPER_SCRATCH_SIZE,
                                );
                                let result = self.load_and_decode_image(&path).and_then(
                                    |wallpaper| self.cache_scaled_wallpaper(&wallpaper),
                                );
                                ALLOCATOR.pop_scratch();
                                let _ = shared_region_unmap(scratch_region);
                                let _ = shared_region_destroy(scratch_region);
                                result
                            }
                            Err(_) => {
                                let _ = shared_region_destroy(scratch_region);
                                Err("Failed to map wallpaper scratch memory")
                            }
                        }
                    }
                    Err(_) => Err("Failed to create wallpaper scratch memory"),
                };

                match result {
                    Ok(()) => log("ui_shell: wallpaper cache updated"),
                    Err(_) => {
                        log("ui_shell: wallpaper update failed; using fallback");
                        self.revert_to_fallback_wallpaper();
                    }
                }
            } else {
                self.wallpaper_cache_valid = false;
            }
            self.wallpaper_recompute_pending = false;
            self.mark_dirty_all();
        }

        if self.config_save_pending {
            let _ = self.save_desktop_config();
            self.config_save_pending = false;
        }
    }

    fn cache_scaled_wallpaper(&mut self, wallpaper: &DecodedImage) -> Result<(), &'static str> {
        let sw = self.fb.width();
        let sh = self.fb.height();
        let size = (sw as usize)
            .checked_mul(sh as usize)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or("Wallpaper cache size overflow")?;
        let region =
            shared_region_create(size).map_err(|_| "Failed to create wallpaper cache")?;
        let ptr = match shared_region_map(region, 0, SharedMemFlags::READ_WRITE) {
            Ok(ptr) => ptr as *mut u8,
            Err(_) => {
                let _ = shared_region_destroy(region);
                return Err("Failed to map wallpaper cache");
            }
        };

        let buf = unsafe { core::slice::from_raw_parts_mut(ptr, size) };
        self.scale_wallpaper_into(
            buf,
            sw,
            sh,
            wallpaper,
            self.current_scaling_mode,
        );

        if let Some(old_region) = self.wallpaper_cache_region {
            let _ = shared_region_unmap(old_region);
            let _ = shared_region_destroy(old_region);
        }
        self.wallpaper_cache_region = Some(region);
        self.wallpaper_cache_ptr = ptr;
        self.wallpaper_cache_valid = true;
        Ok(())
    }

    fn fill_rgba(pixels: &mut [u8], color: Color) {
        for chunk in pixels.chunks_exact_mut(4) {
            chunk[0] = color.r;
            chunk[1] = color.g;
            chunk[2] = color.b;
            chunk[3] = 255;
        }
    }

    fn sample_bilinear(img: &DecodedImage, src_x_fp: i32, src_y_fp: i32) -> Option<(u8, u8, u8, u8)> {
        if img.width == 0 || img.height == 0 {
            return None;
        }

        let max_x_fp = ((img.width - 1) as i32) << 16;
        let max_y_fp = ((img.height - 1) as i32) << 16;
        let x_fp = src_x_fp.clamp(0, max_x_fp);
        let y_fp = src_y_fp.clamp(0, max_y_fp);

        let x0 = (x_fp >> 16) as u32;
        let y0 = (y_fp >> 16) as u32;
        let x1 = (x0 + 1).min(img.width - 1);
        let y1 = (y0 + 1).min(img.height - 1);

        let tx = (x_fp & 0xFFFF) as u32;
        let ty = (y_fp & 0xFFFF) as u32;

        let (r00, g00, b00, a00) = img.get_pixel(x0, y0)?;
        let (r10, g10, b10, a10) = img.get_pixel(x1, y0)?;
        let (r01, g01, b01, a01) = img.get_pixel(x0, y1)?;
        let (r11, g11, b11, a11) = img.get_pixel(x1, y1)?;

        let lerp = |a: u32, b: u32, t: u32| -> u32 { (((a * (65536 - t)) + (b * t)) + 32768) >> 16 };

        let top_r = lerp(r00 as u32, r10 as u32, tx);
        let top_g = lerp(g00 as u32, g10 as u32, tx);
        let top_b = lerp(b00 as u32, b10 as u32, tx);
        let top_a = lerp(a00 as u32, a10 as u32, tx);
        let bot_r = lerp(r01 as u32, r11 as u32, tx);
        let bot_g = lerp(g01 as u32, g11 as u32, tx);
        let bot_b = lerp(b01 as u32, b11 as u32, tx);
        let bot_a = lerp(a01 as u32, a11 as u32, tx);

        let r = lerp(top_r, bot_r, ty).min(255) as u8;
        let g = lerp(top_g, bot_g, ty).min(255) as u8;
        let b = lerp(top_b, bot_b, ty).min(255) as u8;
        let a = lerp(top_a, bot_a, ty).min(255) as u8;

        Some((r, g, b, a))
    }

    fn blit_scaled(
        dest: &mut [u8],
        dest_width: u32,
        dest_height: u32,
        img: &DecodedImage,
        dst_x: i32,
        dst_y: i32,
        dst_w: u32,
        dst_h: u32,
    ) {
        if dst_w == 0 || dst_h == 0 || img.width == 0 || img.height == 0 {
            return;
        }
        for y in 0..dst_h {
            for x in 0..dst_w {
                let src_x = ((((x as u64) * 2 + 1) * img.width as u64) << 15) / dst_w as u64;
                let src_y = ((((y as u64) * 2 + 1) * img.height as u64) << 15) / dst_h as u64;
                let src_x_fp = src_x as i64 - (1 << 15);
                let src_y_fp = src_y as i64 - (1 << 15);
                let dx = dst_x + x as i32;
                let dy = dst_y + y as i32;
                if dx < 0 || dy < 0 || dx >= dest_width as i32 || dy >= dest_height as i32 {
                    continue;
                }
                if let Some((r, g, b, a)) =
                    Self::sample_bilinear(img, src_x_fp as i32, src_y_fp as i32)
                {
                    let off = (((dy as u32) * dest_width + dx as u32) * 4) as usize;
                    dest[off] = r;
                    dest[off + 1] = g;
                    dest[off + 2] = b;
                    dest[off + 3] = a;
                }
            }
        }
    }

    fn scale_fill(&self, dest: &mut [u8], img: &DecodedImage, sw: u32, sh: u32) {
        let scale_w = (sw as u64 * 1024) / img.width as u64;
        let scale_h = (sh as u64 * 1024) / img.height as u64;
        let scale = scale_w.max(scale_h).max(1);
        let target_w = ((img.width as u64 * scale) / 1024) as u32;
        let target_h = ((img.height as u64 * scale) / 1024) as u32;
        let dx = (sw as i32 - target_w as i32) / 2;
        let dy = (sh as i32 - target_h as i32) / 2;
        Self::blit_scaled(dest, sw, sh, img, dx, dy, target_w.max(1), target_h.max(1));
    }

    fn scale_fit(&self, dest: &mut [u8], img: &DecodedImage, sw: u32, sh: u32) {
        let scale_w = (sw as u64 * 1024) / img.width as u64;
        let scale_h = (sh as u64 * 1024) / img.height as u64;
        let scale = scale_w.min(scale_h).max(1);
        let target_w = ((img.width as u64 * scale) / 1024) as u32;
        let target_h = ((img.height as u64 * scale) / 1024) as u32;
        let dx = (sw as i32 - target_w as i32) / 2;
        let dy = (sh as i32 - target_h as i32) / 2;
        Self::blit_scaled(dest, sw, sh, img, dx, dy, target_w.max(1), target_h.max(1));
    }

    fn scale_stretch(&self, dest: &mut [u8], img: &DecodedImage, sw: u32, sh: u32) {
        Self::blit_scaled(dest, sw, sh, img, 0, 0, sw, sh);
    }

    fn scale_center(&self, dest: &mut [u8], img: &DecodedImage, sw: u32, sh: u32) {
        let dx = (sw as i32 - img.width as i32) / 2;
        let dy = (sh as i32 - img.height as i32) / 2;
        Self::blit_scaled(dest, sw, sh, img, dx, dy, img.width, img.height);
    }

    fn scale_tile(&self, dest: &mut [u8], img: &DecodedImage, sw: u32, sh: u32) {
        for y in 0..sh {
            for x in 0..sw {
                if let Some((r, g, b, a)) = img.get_pixel(x % img.width, y % img.height) {
                    let off = ((y * sw + x) * 4) as usize;
                    dest[off] = r;
                    dest[off + 1] = g;
                    dest[off + 2] = b;
                    dest[off + 3] = a;
                }
            }
        }
    }

    fn scale_wallpaper_into(
        &self,
        dest: &mut [u8],
        sw: u32,
        sh: u32,
        img: &DecodedImage,
        mode: ScalingMode,
    ) {
        Self::fill_rgba(dest, self.desktop_bg);
        match mode {
            ScalingMode::Fill => self.scale_fill(dest, img, sw, sh),
            ScalingMode::Fit => self.scale_fit(dest, img, sw, sh),
            ScalingMode::Stretch => self.scale_stretch(dest, img, sw, sh),
            ScalingMode::Center => self.scale_center(dest, img, sw, sh),
            ScalingMode::Tile => self.scale_tile(dest, img, sw, sh),
        }
    }

    /// Build the current taskbar buttons. Each pinned app keeps a stable slot,
    /// shown as its open window when running or as a launcher otherwise; every
    /// other open window (minimised ones included — the whole point, so they can
    /// always be restored) is appended after, ordered by id for a stable place.
    fn taskbar_items(&self) -> Vec<TaskItem> {
        let mut items = Vec::new();
        let mut shown: Vec<WindowId> = Vec::new();

        let window_item = |w: &Window, color: Color, monogram: String| TaskItem {
            color,
            monogram,
            kind: TaskKind::Window {
                window_id: w.id,
                minimized: w.state == WindowState::Minimized,
                focused: w.focused,
            },
        };

        for app in &self.dock_apps {
            let title = window_title_for(&app.executable);
            if let Some(w) = self.wm.windows.iter().find(|w| w.visible && w.title == title) {
                shown.push(w.id);
                items.push(window_item(w, app.color, app.monogram.clone()));
            } else {
                items.push(TaskItem {
                    color: app.color,
                    monogram: app.monogram.clone(),
                    kind: TaskKind::Launch {
                        executable: app.executable.clone(),
                    },
                });
            }
        }

        let mut win_ids: Vec<WindowId> = self
            .wm
            .windows
            .iter()
            .filter(|w| w.visible && !shown.contains(&w.id))
            .map(|w| w.id)
            .collect();
        win_ids.sort_unstable();
        for id in win_ids {
            if let Some(w) = self.wm.windows.iter().find(|w| w.id == id) {
                let (color, monogram) = taskbar_glyph_for(&w.title);
                items.push(window_item(w, color, monogram));
            }
        }

        items
    }

    /// Mark the taskbar strip dirty so it repaints after the open-window set,
    /// focus or minimise state changes.
    fn mark_taskbar_dirty(&mut self) {
        if let Some((dx, dy, dw, dh, _, _, _, _)) = self.dock_layout() {
            self.mark_dirty_rect(Rect::new(dx, dy, dw, dh));
        }
    }

    fn refresh_clock(&mut self) {
        let now = get_ticks();
        let query_interval = if self.time_service_port.is_some() {
            500
        } else {
            3_000
        };
        if now.wrapping_sub(self.last_clock_query_tick) >= query_interval
            || (self.time_service_port.is_none() && self.last_clock_query_tick == 0)
        {
            self.last_clock_query_tick = now;
            if self.time_service_port.is_none() {
                self.time_service_port = libipc::protocol::lookup_service("timesync").ok();
            }
            if let Some(port) = self.time_service_port {
                let request = TimeGetStateMsg {
                    reply_port: self.event_port,
                };
                if send_message_async(port, MessageType::TimeGetState, &request.to_bytes()).is_err()
                {
                    self.time_service_port = None;
                }
            }
        }

        let minute = self
            .clock_state
            .filter(|state| state.synced)
            .map(|state| state.local_unix_seconds(now).div_euclid(60))
            .unwrap_or(-1);
        if minute != self.last_clock_minute {
            self.last_clock_minute = minute;
            self.mark_taskbar_dirty();
        }
    }

    fn refresh_audio(&mut self) {
        let now = get_ticks();
        let query_interval = if self.audio_service_port.is_some() { 500 } else { 300 };
        if now.wrapping_sub(self.last_audio_query_tick) < query_interval
            && self.last_audio_query_tick != 0
        {
            return;
        }
        self.last_audio_query_tick = now;
        if self.audio_service_port.is_none() {
            self.audio_service_port = libipc::protocol::lookup_service("audiod").ok();
        }
        let Some(port) = self.audio_service_port else { return };
        let request = AudioGetStateMsg { reply_port: self.event_port };
        if send_message_async(port, MessageType::AudioGetState, &request.to_bytes()).is_err() {
            self.audio_service_port = None;
            return;
        }
        if !self.startup_chime_requested {
            let play = AudioPlayFileMsg {
                path: String::from("/system/sounds/startup.wav"),
            };
            if send_message_async(port, MessageType::AudioPlayFile, &play.to_bytes()).is_ok() {
                self.startup_chime_requested = true;
                log("ui_shell: startup chime requested");
            }
        }
    }

    fn toggle_audio_mute(&mut self) {
        let Some(port) = self.audio_service_port
            .or_else(|| libipc::protocol::lookup_service("audiod").ok())
        else { return };
        self.audio_service_port = Some(port);
        let current = self.audio_state.unwrap_or(AudioStateReplyMsg {
            volume: 70, muted: false, available: false, playing: false,
        });
        let muted = current.muted || current.volume == 0;
        let request = AudioSetStateMsg {
            reply_port: self.event_port,
            volume: if muted && current.volume == 0 { 70 } else { current.volume },
            muted: !muted,
        };
        if send_message_async(port, MessageType::AudioSetState, &request.to_bytes()).is_ok() {
            self.audio_state = Some(AudioStateReplyMsg {
                volume: request.volume,
                muted: request.muted,
                ..current
            });
            self.mark_taskbar_dirty();
        } else {
            self.audio_service_port = None;
        }
    }

    fn sound_control_rect(&self) -> Option<(u32, u32, u32, u32)> {
        let (_, bar_y, bar_w, bar_h, _, _, _, _) = self.dock_layout()?;
        let (_, clock_len) = format_taskbar_clock(self.clock_state, get_ticks());
        let clock_w = clock_len as u32 * 8;
        let clock_x = bar_w.saturating_sub(atom_theme::spacing::LG as u32 + clock_w);
        let separator_x = clock_x.saturating_sub(atom_theme::spacing::LG as u32);
        let w = 28u32;
        let x = separator_x.saturating_sub(atom_theme::spacing::LG as u32 + w);
        Some((x, bar_y as u32, w, bar_h))
    }

    fn sound_control_at(&self, x: i32, y: i32) -> bool {
        let Some((sx, sy, sw, sh)) = self.sound_control_rect() else { return false };
        x >= sx as i32 && x < (sx + sw) as i32
            && y >= sy as i32 && y < (sy + sh) as i32
    }

    fn dock_icon_at(&self, x: i32, y: i32) -> Option<usize> {
        let (_, bar_y, _, bar_h, start_x, _, item_size, spacing) = self.dock_layout()?;
        if y < bar_y || y >= bar_y + bar_h as i32 {
            return None;
        }
        let count = self.taskbar_items().len();
        for i in 0..count {
            let ix = start_x + (i as i32 * (item_size + spacing));
            if x >= ix && x < ix + item_size {
                return Some(i);
            }
        }
        None
    }

    fn handle_dock_click(&mut self, index: usize) {
        let items = self.taskbar_items();
        let kind = match items.get(index) {
            Some(item) => &item.kind,
            None => return,
        };

        match kind {
            TaskKind::Launch { executable } => {
                let exe = executable.clone();
                self.spawn_app(&exe);
            }
            TaskKind::Window {
                window_id,
                minimized,
                focused,
            } => {
                let id = *window_id;
                if *focused && !*minimized {
                    // Already the active window: a click on its taskbar button
                    // minimises it, matching the Windows/Linux toggle.
                    let rect = self
                        .wm
                        .get_window(id)
                        .map(|w| Rect::new(w.x - 10, w.y - 10, w.width + 20, w.height + 25));
                    self.wm.minimize_window(id);
                    if let Some(r) = rect {
                        self.mark_dirty_rect(r);
                    }
                } else {
                    // Raise and un-minimise the window (focus_window does both).
                    let prev = self.wm.focused_id;
                    self.wm.focus_window(id);
                    if let Some(p) = prev {
                        if p != id {
                            self.mark_window_dirty(p);
                        }
                    }
                    self.mark_window_dirty(id);
                }
                self.mark_taskbar_dirty();
                self.dirty = true;
            }
        }
    }

    /// Full-width bottom taskbar. Returns
    /// `(bar_x, bar_y, bar_w, bar_h, first_item_x, item_y, item_size, spacing)`.
    /// Always present, so it never disappears when no apps are open.
    fn dock_layout(&self) -> Option<(i32, i32, u32, u32, i32, i32, i32, i32)> {
        let width = self.fb.width();
        let height = self.fb.height();
        if width == 0 || height <= DOCK_HEIGHT {
            return None;
        }

        let item_size = atom_theme::shell::DOCK_ITEM_SIZE as i32;
        let spacing = atom_theme::spacing::SM as i32;
        let side_padding = atom_theme::spacing::MD as i32;

        let bar_x = 0;
        let bar_y = height.saturating_sub(DOCK_HEIGHT) as i32;
        let start_x = bar_x + side_padding;
        let item_y = bar_y + (DOCK_HEIGHT as i32 - item_size) / 2;

        Some((bar_x, bar_y, width, DOCK_HEIGHT, start_x, item_y, item_size, spacing))
    }

    /// Ask the `app_launcher` service to start an application by path.
    ///
    /// ui_shell holds no spawn capability of its own — process creation is the
    /// app_launcher's responsibility (it is the sole holder of `SpawnFromPath`).
    /// We send an `AppLaunchRequest` and continue; the reply, if any, arrives on
    /// `event_port` and is ignored by the message dispatch. Returns `true` if
    /// the request was dispatched.
    fn request_app_launch(&self, path: &str) -> bool {
        let launcher_port = match libipc::protocol::lookup_service("app_launcher") {
            Ok(p) => p,
            Err(_) => {
                log("ui_shell: app_launcher service not available; cannot launch app");
                return false;
            }
        };
        let req = match AppLaunchRequestMsg::new(self.event_port as u64, path) {
            Some(r) => r,
            None => {
                log("ui_shell: launch path too long");
                return false;
            }
        };
        if send_message_async(launcher_port, MessageType::AppLaunchRequest, &req.to_bytes())
            .is_err()
        {
            log("ui_shell: failed to send AppLaunchRequest to app_launcher");
            return false;
        }
        true
    }

    fn spawn_app(&mut self, name: &str) {
        match name {
            "terminal" => return self.spawn_terminal(),
            "fileman" => return self.spawn_fileman(),
            "system_settings" => return self.spawn_display_settings(),
            _ => {}
        }

        let user_path = alloc::format!("/apps/user/{}.atxf", name);
        if fs::stat(&user_path).is_ok() {
            let _ = self.request_app_launch(&user_path);
            return;
        }
        let system_path = alloc::format!("/apps/system/{}.atxf", name);
        if fs::stat(&system_path).is_ok() {
            let _ = self.request_app_launch(&system_path);
            return;
        }
        log("ui_shell: requested app not found under /apps/user or /apps/system");
    }

    fn spawn_hosted_window(
        &mut self,
        title: &str,
        base_x: i32,
        base_y: i32,
        step: i32,
        width: u32,
        height: u32,
    ) {
        let offset = (self.wm.windows.len() as i32) * step;
        let window_id = match self.wm.create_window_with_process(
            title,
            base_x + offset,
            base_y + offset,
            width,
            height,
            0,
        ) {
            Some(id) => id,
            None => return,
        };
        self.pending_windows.push(PendingWindow { window_id, pid: 0 });
        self.mark_window_dirty(window_id);
        // A new window means a new taskbar button — repaint the bar.
        self.mark_taskbar_dirty();
    }

    fn spawn_fileman(&mut self) {
        if !self.request_app_launch("/apps/user/fileman.atxf") {
            return;
        }
        self.spawn_hosted_window("File Manager", 100, 80, 30, 720, 480);
    }

    fn spawn_terminal(&mut self) {
        if !self.request_app_launch("/apps/system/terminal.atxf") {
            return;
        }
        self.spawn_hosted_window("Terminal", 150, 120, 30, 640, 420);
    }

    fn spawn_display_settings(&mut self) {
        if !self.request_app_launch("/apps/system/system_settings.atxf") {
            return;
        }
        self.spawn_hosted_window("System Settings", 160, 80, 20, 700, 520);
    }

    fn draw_all(&mut self) {
        let rects = core::mem::take(&mut self.dirty_rects);
        let rects = if rects.is_empty() {
            // `dirty` was set without an explicit rect: repaint everything.
            [Rect::new(0, 0, self.fb.width(), self.fb.height())].to_vec()
        } else {
            rects
        };

        // Footprints of the always-on-top overlays, so each region can stamp
        // them before it is published.
        let menu_rect = if self.context_menu.visible {
            Some(self.context_menu_rect())
        } else {
            None
        };
        // Cursor glyph is 11x17 with a 1px drop shadow; pad to 13x19.
        let cursor_rect = Rect::new(self.cursor.x, self.cursor.y, 13, 19);

        // Composite and publish each damaged region one at a time. The drawing
        // primitives are NOT clipped to the region, so compositing a window
        // over-paints its whole footprint into the backbuffer — including any
        // area belonging to another damage rect. If every region were
        // composited first and blitted afterwards, a later region's over-draw
        // would clobber an earlier region's backbuffer pixels (wallpaper not
        // restored, windows above it not redrawn) and that corruption would be
        // published, making windows pop up where they are not. Publishing each
        // region immediately after compositing it pushes the correct pixels to
        // the visible framebuffer before any later region can touch them.
        for rect in &rects {
            self.composite_region(*rect);

            // Stamp the overlays that overlap this region on top, so they are
            // never clipped away by a region that excludes them.
            if let Some(menu_rect) = menu_rect {
                if menu_rect.intersects(rect) {
                    self.draw_context_menu();
                }
            }
            if cursor_rect.intersects(rect) {
                self.draw_cursor();
            }

            self.backbuffer_fb.blit_rect(
                &self.fb,
                rect.x as u32,
                rect.y as u32,
                rect.w,
                rect.h,
            );
        }
    }

    /// Composite a single damage rectangle into the backbuffer: wallpaper (or
    /// background), desktop icons, windows, and the taskbar. Only the wallpaper
    /// copy is clipped to `draw_rect`; windows and the taskbar draw their full
    /// footprint, so `draw_all` composites and blits one region at a
    /// time to keep that over-draw from corrupting other regions. Overlays
    /// (context menu, cursor) are stamped per region by `draw_all`.
    fn composite_region(&mut self, draw_rect: Rect) {
        if self.wallpaper_cache_valid && !self.wallpaper_cache_ptr.is_null() {
            let sw = self.fb.width();
            let stride = self.backbuffer_fb.stride();
            let src_ptr = self.wallpaper_cache_ptr;
            let dst_ptr = self.backbuffer_ptr as *mut u8;

            let y_start = draw_rect.y as u32;
            let y_end = (draw_rect.y + draw_rect.h as i32) as u32;
            let x_start = draw_rect.x as u32;
            let copy_len = draw_rect.w * 4;

            for row in y_start..y_end {
                let src_off = (row * sw * 4 + x_start * 4) as usize;
                let dst_off = (row * stride * 4 + x_start * 4) as usize;
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        src_ptr.add(src_off),
                        dst_ptr.add(dst_off),
                        copy_len as usize,
                    );
                }
            }
        } else {
            self.backbuffer_fb.fill_rect(
                draw_rect.x as u32,
                draw_rect.y as u32,
                draw_rect.w,
                draw_rect.h,
                self.desktop_bg,
            );
        }

        for icon in &self.icons {
            let icon_rect = Rect::new(icon.x, icon.y, 60, 80);
            if icon_rect.intersects(&draw_rect) {
                self.draw_desktop_icon(icon);
            }
        }

        for window in self.wm.windows.iter() {
            if window.visible {
                let win_rect =
                    Rect::new(window.x - 4, window.y - 4, window.width + 8, window.height + 12);
                if win_rect.intersects(&draw_rect) {
                    self.draw_window(window);
                }
            }
        }

        if let Some((dx, dy, dw, dh, _, _, _, _)) = self.dock_layout() {
            let dock_rect = Rect::new(dx, dy, dw, dh);
            if dock_rect.intersects(&draw_rect) {
                self.draw_dock();
            }
        }
    }

    fn draw_desktop_icon(&self, icon: &DesktopIcon) {
        let ix = icon.x as u32;
        let iy = icon.y as u32;
        let size = 60u32;
        let icon_size = 48u32;
        let icon_x = ix + (size - icon_size) / 2;
        let icon_y = iy + (size - icon_size) / 2 - 2;

        self.backbuffer_fb
            .fill_rect_rounded_aa(icon_x, icon_y, icon_size, icon_size, 8, icon.color);

        let label_len = icon.label.len() as u32 * 8;
        let lx = (ix as i32 + (size as i32 - label_len as i32) / 2).max(0) as u32;
        let label_y = iy + size + 6;
        self.backbuffer_fb
            .draw_string(lx, label_y, &icon.label, theme::ICON_LABEL, self.desktop_bg);
    }

    /// Blit a window's committed front buffer into the backbuffer, rounding
    /// the bottom two corners. Reads exclusively from the compositor-owned
    /// snapshot so a client mid-redraw can never tear the composited output.
    fn blit_front_buffer_bottom_rounded(
        &self,
        window: &Window,
        dest_x: u32,
        dest_y: u32,
        radius: u32,
    ) {
        let src_addr = window.front_ptr as usize;
        if src_addr == 0 {
            return;
        }
        let src_width = window.front_width;
        let src_height = window.front_height;
        let src_stride = src_width;

        let fb_bpp = self.backbuffer_fb.bytes_per_pixel();
        if fb_bpp != 4 {
            // Cold path: backbuffer is not 32-bit; fall back to per-pixel writes.
            for sy in 0..src_height {
                for sx in 0..src_width {
                    let off = ((sy * src_stride + sx) as usize) * 4;
                    let pixel = unsafe { ((src_addr + off) as *const u32).read() };
                    self.backbuffer_fb
                        .draw_pixel(dest_x + sx, dest_y + sy, rgb32_to_color(pixel));
                }
            }
            return;
        }

        let copy_width = src_width.min(self.backbuffer_fb.width().saturating_sub(dest_x));
        let copy_height = src_height.min(self.backbuffer_fb.height().saturating_sub(dest_y));
        if copy_width == 0 || copy_height == 0 {
            return;
        }

        let r = radius.min(copy_width / 2).min(copy_height);

        let fb_addr = self.backbuffer_fb.address();
        let fb_stride = self.backbuffer_fb.stride();
        let straight_height = copy_height.saturating_sub(r);

        for sy in 0..straight_height {
            let src_offset = (sy * src_stride) as usize * fb_bpp;
            let dst_offset = ((dest_y + sy) * fb_stride + dest_x) as usize * fb_bpp;
            let src_ptr = (src_addr + src_offset) as *const u8;
            let dst_ptr = (fb_addr + dst_offset) as *mut u8;
            unsafe {
                core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, copy_width as usize * fb_bpp);
            }
        }

        for sy in straight_height..copy_height {
            let dy = copy_height - 1 - sy;
            let fy = r - dy;
            let n = r * r - fy * fy;
            let sq = isqrt_helper(n);
            let frac = frac_sqrt_256_helper(n, sq);
            let int_offset = r - sq;
            let row_y = dest_y + sy;
            let row_width = copy_width.saturating_sub(int_offset * 2);

            if row_width > 0 {
                let src_offset = (sy * src_stride + int_offset) as usize * fb_bpp;
                let dst_offset = (row_y * fb_stride + dest_x + int_offset) as usize * fb_bpp;
                let src_ptr = (src_addr + src_offset) as *const u8;
                let dst_ptr = (fb_addr + dst_offset) as *mut u8;
                unsafe {
                    core::ptr::copy_nonoverlapping(src_ptr, dst_ptr, row_width as usize * fb_bpp);
                }
            }

            if frac > 0 && int_offset > 0 {
                let alpha = frac as u8;

                let left_src_x = int_offset - 1;
                let left_src_offset = (sy * src_stride + left_src_x) as usize * fb_bpp;
                let left_pixel = unsafe {
                    ((src_addr + left_src_offset) as *const u32).read_volatile()
                };
                self.backbuffer_fb.fill_rect_alpha(
                    dest_x + left_src_x,
                    row_y,
                    1,
                    1,
                    rgb32_to_color(left_pixel),
                    alpha,
                );

                let right_src_x = copy_width - int_offset;
                let right_src_offset = (sy * src_stride + right_src_x) as usize * fb_bpp;
                let right_pixel = unsafe {
                    ((src_addr + right_src_offset) as *const u32).read_volatile()
                };
                self.backbuffer_fb.fill_rect_alpha(
                    dest_x + right_src_x,
                    row_y,
                    1,
                    1,
                    rgb32_to_color(right_pixel),
                    alpha,
                );
            }
        }
    }

    fn draw_window(&self, window: &Window) {
        if !window.visible || window.state == WindowState::Minimized {
            return;
        }

        let x = window.x as u32;
        let y = window.y as u32;
        let w = window.width;
        let h = window.height;

        // A maximised window fills the work area edge-to-edge, so rounded corners
        // would only carve odd notches against the wallpaper — square it off and
        // drop the shadow.
        let maximized = window.state == WindowState::Maximized;
        let radius = if maximized {
            0
        } else {
            atom_theme::shell::WINDOW_RADIUS as u32
        };

        if !maximized {
            // Soft, even drop shadow whose corner radius tracks the window's, so
            // the halo hugs the frame instead of poking out at the corners.
            let r = radius;
            self.backbuffer_fb.fill_rect_rounded_alpha(
                x.saturating_sub(3), y.saturating_sub(1), w + 6, h + 8, r + 3, theme::SHADOW, 28,
            );
            self.backbuffer_fb.fill_rect_rounded_alpha(
                x.saturating_sub(2), y, w + 4, h + 6, r + 2, theme::SHADOW, 44,
            );
            self.backbuffer_fb.fill_rect_rounded_alpha(
                x.saturating_sub(1), y + 1, w + 2, h + 4, r + 1, theme::SHADOW, 62,
            );
        }

        let border_color = if window.focused {
            theme::WINDOW_BORDER_FOCUSED
        } else {
            theme::WINDOW_BORDER
        };

        self.backbuffer_fb.fill_rect_rounded_aa(x, y, w, h, radius, border_color);

        let inner_r = radius.saturating_sub(WINDOW_BORDER_WIDTH);
        let header_color = if window.focused {
            theme::WINDOW_HEADER_FOCUSED
        } else {
            theme::WINDOW_HEADER
        };
        self.backbuffer_fb.fill_rect_top_rounded_aa(
            x + WINDOW_BORDER_WIDTH,
            y + WINDOW_BORDER_WIDTH,
            w - WINDOW_BORDER_WIDTH * 2,
            WINDOW_HEADER_HEIGHT - WINDOW_BORDER_WIDTH,
            inner_r,
            header_color,
        );
        self.backbuffer_fb.fill_rect(
            x + WINDOW_BORDER_WIDTH,
            y + WINDOW_HEADER_HEIGHT - 1,
            w - WINDOW_BORDER_WIDTH * 2,
            1,
            border_color,
        );

        let title_y = y + (WINDOW_HEADER_HEIGHT - 8) / 2;
        let title_color = if window.focused {
            theme::PANEL_TEXT
        } else {
            theme::PANEL_TEXT_DIM
        };
        self.backbuffer_fb
            .draw_string(x + 16, title_y, &window.title, title_color, header_color);

        let (btn_y_i, btn_size_i, close_xi, max_xi, min_xi) =
            window_button_layout(window.x, window.y, w);
        let btn_y = btn_y_i as u32;
        let btn_size = btn_size_i as u32;
        let btn_radius = btn_size / 2;
        let close_x = close_xi as u32;
        let max_x = max_xi as u32;
        let min_x = min_xi as u32;

        if window.focused {
            self.backbuffer_fb
                .fill_rect_rounded_aa(close_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_CLOSE);
            self.backbuffer_fb.fill_rect_rounded_aa(
                max_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_MAXIMIZE,
            );
            self.backbuffer_fb.fill_rect_rounded_aa(
                min_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_MINIMIZE,
            );
        } else {
            self.backbuffer_fb.fill_rect_rounded_aa(
                close_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_INACTIVE,
            );
            self.backbuffer_fb.fill_rect_rounded_aa(
                max_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_INACTIVE,
            );
            self.backbuffer_fb.fill_rect_rounded_aa(
                min_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_INACTIVE,
            );
        }

        self.backbuffer_fb.fill_rect_bottom_rounded_aa(
            window.content_x(),
            window.content_y(),
            window.content_width(),
            window.content_height(),
            inner_r,
            theme::WINDOW_BG,
        );

        if window.surface_ready {
            self.blit_front_buffer_bottom_rounded(
                window,
                window.content_x(),
                window.content_y(),
                inner_r,
            );
        }
    }

    fn draw_dock(&self) {
        let (bar_x_i32, bar_y_i32, bar_w, bar_h, start_x_i32, item_y_i32, item_size_i32, spacing_i32) =
            match self.dock_layout() {
                Some(v) => v,
                None => return,
            };

        let bar_x = bar_x_i32 as u32;
        let bar_y = bar_y_i32 as u32;
        let start_x = start_x_i32 as u32;
        let item_y = item_y_i32 as u32;
        let item_size = item_size_i32 as u32;
        let spacing = spacing_i32 as u32;

        // Full-width bar flush with the bottom edge (Windows/Linux taskbar),
        // with a thin top border separating it from the desktop.
        self.backbuffer_fb.fill_rect(bar_x, bar_y, bar_w, bar_h, theme::DOCK_BG);
        self.backbuffer_fb.fill_rect(bar_x, bar_y, bar_w, 1, theme::DOCK_BORDER);

        let tile_r = atom_theme::shell::DOCK_ITEM_RADIUS as u32;
        let items = self.taskbar_items();
        for (i, item) in items.iter().enumerate() {
            let ix = start_x + (i as u32 * (item_size + spacing));

            let (minimized, focused, is_window) = match item.kind {
                TaskKind::Window { minimized, focused, .. } => (minimized, focused, true),
                TaskKind::Launch { .. } => (false, false, false),
            };

            // Subtle highlight panel behind the active window's tile.
            if focused {
                self.backbuffer_fb.fill_rect_rounded_alpha(
                    ix.saturating_sub(4),
                    item_y.saturating_sub(3),
                    item_size + 8,
                    item_size + 6,
                    tile_r + 2,
                    theme::DOCK_BORDER,
                    150,
                );
            }

            // The app tile. Minimised windows are dimmed so they read as hidden
            // but still clearly restorable.
            if minimized {
                self.backbuffer_fb
                    .fill_rect_rounded_alpha(ix, item_y, item_size, item_size, tile_r, item.color, 120);
            } else {
                self.backbuffer_fb
                    .fill_rect_rounded_aa(ix, item_y, item_size, item_size, tile_r, item.color);
            }

            // Pick a glyph colour that stays legible on the tile's fill.
            let lum = (item.color.r as u32 * 299
                + item.color.g as u32 * 587
                + item.color.b as u32 * 114)
                / 1000;
            let glyph = if lum > 150 {
                atom_theme::colors::ATOM_COLOR_BG
            } else {
                Color::WHITE
            };

            let label_len = item.monogram.len() as u32 * 8;
            let lx = ix + (item_size - label_len) / 2;
            let ly = item_y + (item_size - 8) / 2;
            self.backbuffer_fb
                .draw_string(lx, ly, &item.monogram, glyph, item.color);

            // Running indicator under window tiles: a wide accent underline for
            // the focused window, a small dot for the others. Launchers (apps
            // that are not running) get nothing.
            if is_window {
                let ind_y = item_y + item_size + 3;
                if focused {
                    self.backbuffer_fb
                        .fill_rect_rounded_aa(ix, ind_y, item_size, 3, 1, theme::ACCENT);
                } else {
                    let dot_x = ix + item_size / 2 - 2;
                    self.backbuffer_fb
                        .fill_rect_rounded_aa(dot_x, ind_y, 4, 4, 2, theme::ACCENT);
                }
            }
        }

        // Internet-synchronized clock, right-aligned and vertically centered
        // in the taskbar. The shell advances the last service sample with the
        // monotonic kernel tick, so drawing the clock never requires network IO.
        let (clock_buf, clock_len) = format_taskbar_clock(self.clock_state, get_ticks());
        let clock = core::str::from_utf8(&clock_buf[..clock_len]).unwrap_or("--:--");
        let clock_w = clock_len as u32 * 8;
        let clock_x = bar_w.saturating_sub(atom_theme::spacing::LG as u32 + clock_w);
        let clock_y = bar_y + (bar_h.saturating_sub(8)) / 2;
        let separator_x = clock_x.saturating_sub(atom_theme::spacing::LG as u32);
        self.backbuffer_fb.fill_rect(
            separator_x,
            bar_y + atom_theme::spacing::MD as u32,
            1,
            bar_h.saturating_sub(atom_theme::spacing::MD as u32 * 2),
            theme::DOCK_BORDER,
        );
        if let Some((sound_x, _, _, _)) = self.sound_control_rect() {
            let state = self.audio_state.unwrap_or(AudioStateReplyMsg {
                volume: 70, muted: false, available: false, playing: false,
            });
            let icon_y = bar_y + (bar_h.saturating_sub(12)) / 2;
            let color = if state.available { theme::PANEL_TEXT } else { theme::PANEL_TEXT_DIM };
            self.backbuffer_fb.fill_rect(sound_x + 2, icon_y + 4, 4, 5, color);
            self.backbuffer_fb.fill_rect(sound_x + 6, icon_y + 2, 3, 9, color);
            if state.muted || state.volume == 0 {
                self.backbuffer_fb.draw_string(
                    sound_x + 13, icon_y + 2, "x", theme::BTN_CLOSE, theme::DOCK_BG,
                );
            } else {
                self.backbuffer_fb.draw_string(
                    sound_x + 12, icon_y + 2, "))", color, theme::DOCK_BG,
                );
            }
        }
        self.backbuffer_fb
            .draw_string(clock_x, clock_y, clock, theme::PANEL_TEXT, theme::DOCK_BG);
    }

    fn draw_context_menu(&self) {
        let menu_w = 200u32;
        let item_h = atom_theme::spacing::XXXL as u32; 
        let padding_v = atom_theme::spacing::SM as u32 - 2; 
        let menu_r = atom_theme::radius::SM as u32;
        let menu_h = self.context_menu.items.len() as u32 * item_h + padding_v * 2;
        let mx = self.context_menu.x as u32;
        let my = self.context_menu.y as u32;

        self.backbuffer_fb
            .fill_rect_rounded_alpha(mx + 2, my + 3, menu_w, menu_h, menu_r, theme::SHADOW, 100);
        self.backbuffer_fb
            .fill_rect_rounded_aa(mx, my, menu_w, menu_h, menu_r, theme::MENU_BG);
        self.backbuffer_fb
            .draw_rect_rounded_aa(mx, my, menu_w, menu_h, menu_r, theme::MENU_BORDER);

        for (i, item) in self.context_menu.items.iter().enumerate() {
            let iy = my + padding_v + (i as u32 * item_h);
            let text_y = iy + (item_h - 8) / 2;
            self.backbuffer_fb.draw_string(
                mx + atom_theme::spacing::LG as u32,
                text_y,
                item,
                theme::MENU_TEXT,
                theme::MENU_BG,
            );
        }
    }

    fn icon_at(&self, x: i32, y: i32) -> Option<usize> {
        for (i, icon) in self.icons.iter().enumerate() {
            if x >= icon.x && x < icon.x + 60 && y >= icon.y && y < icon.y + 60 {
                return Some(i);
            }
        }
        None
    }

    fn draw_cursor(&self) {
        // Classic arrow pointer. 0 = transparent, 1 = outline, 2 = fill.
        // The hotspot is the tip at the top-left pixel (0,0), matching the
        // coordinate used for click hit-testing.
        const CURSOR_W: usize = 11;
        let cursor_shape: [[u8; CURSOR_W]; 17] = [
            [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            [1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            [1, 2, 1, 0, 0, 0, 0, 0, 0, 0, 0],
            [1, 2, 2, 1, 0, 0, 0, 0, 0, 0, 0],
            [1, 2, 2, 2, 1, 0, 0, 0, 0, 0, 0],
            [1, 2, 2, 2, 2, 1, 0, 0, 0, 0, 0],
            [1, 2, 2, 2, 2, 2, 1, 0, 0, 0, 0],
            [1, 2, 2, 2, 2, 2, 2, 1, 0, 0, 0],
            [1, 2, 2, 2, 2, 2, 2, 2, 1, 0, 0],
            [1, 2, 2, 2, 2, 2, 2, 2, 2, 1, 0],
            [1, 2, 2, 2, 2, 2, 1, 1, 1, 1, 1],
            [1, 2, 2, 1, 2, 2, 1, 0, 0, 0, 0],
            [1, 2, 1, 0, 1, 2, 2, 1, 0, 0, 0],
            [1, 1, 0, 0, 1, 2, 2, 1, 0, 0, 0],
            [1, 0, 0, 0, 0, 1, 2, 2, 1, 0, 0],
            [0, 0, 0, 0, 0, 1, 2, 2, 1, 0, 0],
            [0, 0, 0, 0, 0, 0, 1, 1, 1, 0, 0],
        ];

        let cx = self.cursor.x as u32;
        let cy = self.cursor.y as u32;
        let fb_w = self.backbuffer_fb.width();
        let fb_h = self.backbuffer_fb.height();

        // Soft drop shadow (offset by 1px) for depth and to keep the pointer
        // legible over both dark wallpaper and light app surfaces.
        for (row, cols) in cursor_shape.iter().enumerate() {
            for (col, &pixel) in cols.iter().enumerate() {
                if pixel == 0 {
                    continue;
                }
                let px = cx + col as u32 + 1;
                let py = cy + row as u32 + 1;
                if px < fb_w && py < fb_h {
                    self.backbuffer_fb.fill_rect_alpha(px, py, 1, 1, Color::BLACK, 70);
                }
            }
        }

        for (row, cols) in cursor_shape.iter().enumerate() {
            for (col, &pixel) in cols.iter().enumerate() {
                let px = cx + col as u32;
                let py = cy + row as u32;
                if px < fb_w && py < fb_h {
                    match pixel {
                        1 => self.backbuffer_fb.draw_pixel(px, py, theme::CURSOR_OUTLINE),
                        2 => self.backbuffer_fb.draw_pixel(px, py, theme::CURSOR_FILL),
                        _ => {}
                    }
                }
            }
        }
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

#[no_mangle]
pub extern "efiapi" fn efi_main(
    _image_handle: *const core::ffi::c_void,
    _system_table: *const core::ffi::c_void,
) -> usize {
    main()
}

fn main() -> ! {
    log("Atom Desktop Environment v1.0");
    log("ui_shell: startup begin");

    let _fb_info = match get_framebuffer() {
        Some(info) => info,
        None => {
            log("ui_shell: failed to get framebuffer info");
            exit(1)
        }
    };

    let heap_size = 8 * 1024 * 1024;

    let region_id = match shared_region_create(heap_size) {
        Ok(id) => id,
        Err(_) => {
            log("ui_shell: failed to create shared heap region");
            exit(1)
        }
    };

    let heap_start = match shared_region_map(region_id, 0, SharedMemFlags::READ_WRITE) {
        Ok(addr) => addr,
        Err(_) => {
            log("ui_shell: failed to map shared heap region");
            exit(1)
        }
    };

    ALLOCATOR.init(heap_start as usize, heap_size);

    let fb = match Framebuffer::new() {
        Some(fb) => fb,
        None => {
            log("ui_shell: failed to create framebuffer handle");
            exit(1)
        }
    };

    let mut compositor = Compositor::new(fb);

    log("ui_shell: registering compositor services");
    if libipc::protocol::register_service("compositor", compositor.event_port).is_err() {
        log("ui_shell: failed to register service 'compositor'");
        exit(1);
    }
    if libipc::protocol::register_service("compositor.register", compositor.register_port).is_err() {
        log("ui_shell: failed to register service 'compositor.register'");
        exit(1);
    }
    if libipc::protocol::register_service("compositor.wm", compositor.register_port).is_err() {
        log("ui_shell: failed to register service 'compositor.wm'");
        exit(1);
    }
    log("ui_shell: compositor services registered");

    compositor.run()
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("Desktop: PANIC!");
    exit(0xFF);
}
