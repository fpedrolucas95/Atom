//! Atom Desktop Environment alpha

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

struct BumpAllocator {
    start: AtomicUsize,
    size: AtomicUsize,
    next: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

impl BumpAllocator {
    const fn new() -> Self {
        Self {
            start: AtomicUsize::new(0),
            size: AtomicUsize::new(0),
            next: AtomicUsize::new(0),
        }
    }

    fn init(&self, start: usize, size: usize) {
        self.start.store(start, Ordering::SeqCst);
        self.size.store(size, Ordering::SeqCst);
        self.next.store(0, Ordering::SeqCst);
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align().max(16);

        let heap_start = self.start.load(Ordering::Relaxed);
        let heap_size = self.size.load(Ordering::Relaxed);

        if heap_start == 0 || heap_size == 0 {
            return core::ptr::null_mut();
        }

        loop {
            let current = self.next.load(Ordering::Relaxed);
            let aligned = (current + align - 1) & !(align - 1);
            let new_next = aligned + size;

            if new_next > heap_size {
                return core::ptr::null_mut();
            }

            if self.next.compare_exchange_weak(
                current, new_next, Ordering::SeqCst, Ordering::Relaxed
            ).is_ok() {
                return (heap_start as *mut u8).add(aligned);
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    loop {}
}

use alloc::string::String;
use alloc::vec::Vec;
use core::panic::PanicInfo;

use atom_syscall::graphics::{Color, Framebuffer, SharedSurface, SharedRegionId, SharedMemFlags, shared_region_create, shared_region_map, get_framebuffer};
use atom_syscall::ipc::{create_port, send, try_recv, wait_any, PortId};
use atom_syscall::interrupts::register_irq_handler;
use atom_syscall::thread::{yield_now, exit};
use atom_syscall::debug::log;
use atom_syscall::process::{spawn_process, ProcessId};
use atom_syscall::input::{MouseDriver, keyboard_poll, scancode_to_ascii, scancodes};

use libipc::messages::{MessageType, MessageHeader, WindowId, SurfaceAssignMsg, TerminateRequestMsg, AppRegisterMsg, SurfacePresentMsg, KeyEvent, KeyModifiers, MouseMoveEvent, MouseButtonEvent, MouseButton};
use libipc::protocol::send_message_async;
use libipc::well_known;

mod theme {
    use atom_syscall::graphics::Color;

    pub const DESKTOP_BG: Color = Color::new(30, 33, 40);
    pub const PANEL_BG: Color = Color::new(20, 22, 28);
    pub const PANEL_TEXT: Color = Color::new(220, 223, 228);
    pub const ACCENT: Color = Color::new(136, 192, 208);
    pub const WINDOW_BG: Color = Color::new(40, 44, 52);
    pub const WINDOW_HEADER: Color = Color::new(35, 39, 46);
    pub const WINDOW_HEADER_FOCUSED: Color = Color::new(55, 61, 73);
    pub const WINDOW_BORDER: Color = Color::new(60, 66, 82);
    pub const DOCK_BG: Color = Color::new(20, 22, 28);
    pub const CURSOR_FILL: Color = Color::WHITE;
    pub const CURSOR_OUTLINE: Color = Color::BLACK;
    pub const SHADOW: Color = Color::new(8, 9, 12);
    pub const BTN_CLOSE: Color = Color::new(191, 97, 106);
    pub const BTN_MAXIMIZE: Color = Color::new(163, 190, 140);
    pub const BTN_MINIMIZE: Color = Color::new(235, 203, 139);
}

const WINDOW_HEADER_HEIGHT: u32 = 32;
const WINDOW_BORDER_WIDTH: u32 = 1;
const WINDOW_MIN_WIDTH: u32 = 120;
const WINDOW_MIN_HEIGHT: u32 = 80;
const PANEL_HEIGHT: u32 = 32;
const DOCK_HEIGHT: u32 = 60;
const DOCK_WIDTH: u32 = 340;

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
    process_id: Option<ProcessId>,
    surface: Option<SharedSurface>,
    surface_region_id: Option<SharedRegionId>,
    content_dirty: bool,
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
            process_id: None,
            surface: None,
            surface_region_id: None,
            content_dirty: false,
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
        process_id: ProcessId,
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
            process_id: Some(process_id),
            surface_region_id: Some(region_id),
            surface: Some(surface),
            content_dirty: true,
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

    fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y
            && px < self.x + self.width as i32
            && py < self.y + self.height as i32
    }

    fn header_contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y
            && px < self.x + self.width as i32
            && py < self.y + WINDOW_HEADER_HEIGHT as i32
    }
}

struct WindowManager {
    windows: Vec<Window>,
    next_id: WindowId,
    focused_id: Option<WindowId>,
}

impl WindowManager {
    fn new() -> Self {
        Self {
            windows: Vec::new(),
            next_id: 1,
            focused_id: None,
        }
    }

    fn create_window(&mut self, title: &str, x: i32, y: i32, width: u32, height: u32) -> WindowId {
        let id = self.next_id;
        self.next_id += 1;

        let window = Window::new(id, title, x, y, width, height);
        self.windows.push(window);
        self.focus_window(id);
        id
    }

    fn create_window_with_process(
        &mut self,
        title: &str,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
        process_id: ProcessId,
        event_port: PortId,
    ) -> Option<WindowId> {
        let id = self.next_id;
        self.next_id += 1;

        let window = Window::new_with_process(id, title, x, y, width, height, process_id, event_port)?;
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
                    let msg = libipc::messages::WmWindowEventMsg {
                        window_id: prev_id,
                        event_type: libipc::messages::WindowEventType::Unfocus,
                        x: w.x, y: w.y, width: w.width, height: w.height,
                    };
                    let header = MessageHeader::new(MessageType::WmEvent, libipc::messages::WmWindowEventMsg::SIZE as u32);
                    let mut full_msg = [0u8; 64];
                    full_msg[..MessageHeader::SIZE].copy_from_slice(&header.to_bytes());
                    full_msg[MessageHeader::SIZE..MessageHeader::SIZE + libipc::messages::WmWindowEventMsg::SIZE].copy_from_slice(&msg.to_bytes());
                    let _ = send(port, &full_msg[..MessageHeader::SIZE + libipc::messages::WmWindowEventMsg::SIZE]);
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
                let msg = libipc::messages::WmWindowEventMsg {
                    window_id: id,
                    event_type: libipc::messages::WindowEventType::Focus,
                    x: window.x, y: window.y, width: window.width, height: window.height,
                };
                let header = MessageHeader::new(MessageType::WmEvent, libipc::messages::WmWindowEventMsg::SIZE as u32);
                let mut full_msg = [0u8; 64];
                full_msg[..MessageHeader::SIZE].copy_from_slice(&header.to_bytes());
                full_msg[MessageHeader::SIZE..MessageHeader::SIZE + libipc::messages::WmWindowEventMsg::SIZE].copy_from_slice(&msg.to_bytes());
                let _ = send(port, &full_msg[..MessageHeader::SIZE + libipc::messages::WmWindowEventMsg::SIZE]);
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
        self.windows.retain(|w| w.id != id);
        if self.focused_id == Some(id) {
            self.focused_id = self.windows.iter().filter(|w| w.visible && w.state != WindowState::Minimized).last().map(|w| w.id);
            if let Some(new_focus) = self.focused_id {
                self.focus_window(new_focus);
            }
        }
    }

    fn move_window(&mut self, id: WindowId, dx: i32, dy: i32) {
        if let Some(window) = self.get_window_mut(id) {
            if window.state == WindowState::Maximized {
                return;
            }
            window.x += dx;
            window.y += dy;
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

    fn toggle_maximize(&mut self, id: WindowId, screen_width: u32, screen_height: u32) {
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
                    window.x = 0;
                    window.y = PANEL_HEIGHT as i32;
                    window.width = screen_width;
                    window.height = screen_height - PANEL_HEIGHT;
                }
            }
            window.content_dirty = true;
        }
    }

    fn minimize_window(&mut self, id: WindowId) {
        if let Some(window) = self.get_window_mut(id) {
            window.state = WindowState::Minimized;
            window.focused = false;
        }
        if self.focused_id == Some(id) {
            self.focused_id = self.windows.iter().filter(|w| w.visible && w.state != WindowState::Minimized).last().map(|w| w.id);
            if let Some(new_focus) = self.focused_id {
                self.focus_window(new_focus);
            }
        }
    }
}

struct CursorState {
    x: i32,
    y: i32,
    visible: bool,
}

impl CursorState {
    fn new(width: u32, height: u32) -> Self {
        Self {
            x: (width / 2) as i32,
            y: (height / 2) as i32,
            visible: true,
        }
    }

    fn apply_delta(&mut self, dx: i32, dy: i32, width: u32, height: u32) {
        self.x = (self.x + dx).clamp(0, (width - 1) as i32);
        self.y = (self.y - dy).clamp(0, (height - 1) as i32);
    }
}

struct PendingWindow {
    pid: ProcessId,
    window_id: WindowId,
}

struct DesktopIcon {
    label: String,
    executable: String,
    x: i32,
    y: i32,
    color: Color,
}

struct ContextMenu {
    x: i32,
    y: i32,
    visible: bool,
    items: Vec<String>,
}

struct WallpaperPicker {
    visible: bool,
    x: i32,
    y: i32,
    width: u32,
    height: u32,
    colors: Vec<Color>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragOperation {
    None,
    Move { window_id: WindowId, start_mouse_x: i32, start_mouse_y: i32, start_win_x: i32, start_win_y: i32 },
    Resize { window_id: WindowId, start_mouse_x: i32, start_mouse_y: i32, start_win_w: u32, start_win_h: u32 },
}

struct Compositor {
    fb: Framebuffer,
    backbuffer_fb: Framebuffer,
    _backbuffer: Vec<u32>,
    wm: WindowManager,
    cursor: CursorState,
    event_port: PortId,
    register_port: PortId,
    pending_windows: Vec<PendingWindow>,
    dirty: bool,
    mouse_left_down: bool,
    mouse_right_down: bool,
    drag_op: DragOperation,
    captured_window: Option<WindowId>,
    mouse_driver: MouseDriver,
    keyboard_shift: bool,
    desktop_bg: Color,
    icons: Vec<DesktopIcon>,
    ticks: u32,
    click_counter: u32,
    last_click_tick: u32,
    last_click_icon: Option<usize>,
    context_menu: ContextMenu,
    wallpaper_picker: WallpaperPicker,
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

        let mut icons = Vec::new();
        icons.push(DesktopIcon {
            label: String::from("Rect Demo"),
            executable: String::from("demo_rects"),
            x: 24,
            y: PANEL_HEIGHT as i32 + 24,
            color: Color::new(136, 192, 208),
        });
        icons.push(DesktopIcon {
            label: String::from("Text Demo"),
            executable: String::from("demo_text"),
            x: 24,
            y: PANEL_HEIGHT as i32 + 120,
            color: Color::new(163, 190, 140),
        });

        let fb_size_pixels = (fb.stride() * fb.height()) as usize;
        let mut backbuffer = alloc::vec![0u32; fb_size_pixels];
        let backbuffer_fb = Framebuffer::new_custom(
            backbuffer.as_mut_ptr() as usize,
            fb.width(),
            fb.height(),
            fb.stride(),
            fb.bytes_per_pixel() as u32,
        ).expect("Failed to create backbuffer framebuffer");

        Self {
            fb,
            backbuffer_fb,
            _backbuffer: backbuffer,
            wm: WindowManager::new(),
            cursor: CursorState::new(width, height),
            event_port,
            register_port,
            pending_windows: Vec::new(),
            dirty: true,
            mouse_left_down: false,
            mouse_right_down: false,
            drag_op: DragOperation::None,
            captured_window: None,
            mouse_driver,
            keyboard_shift: false,
            desktop_bg: theme::DESKTOP_BG,
            icons,
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
            wallpaper_picker: WallpaperPicker {
                visible: false,
                x: (width as i32 - 340) / 2,
                y: (height as i32 - 240) / 2,
                width: 340,
                height: 240,
                colors: {
                    let mut v = Vec::new();
                    v.push(Color::new(30, 33, 40));
                    v.push(Color::new(46, 52, 64));
                    v.push(Color::new(59, 66, 82));
                    v.push(Color::new(76, 86, 106));
                    v.push(Color::new(136, 192, 208));
                    v.push(Color::new(143, 188, 187));
                    v.push(Color::new(163, 190, 140));
                    v.push(Color::new(191, 97, 106));
                    v
                },
            },
        }
    }

    fn run(&mut self) -> ! {
        self.wm.create_window("Welcome to Atom OS", 120, 100, 480, 320);
        self.draw_all();

        let mut reg_buffer = [0u8; 64];

        loop {
            let ports = [self.register_port, self.event_port];

            while let Ok(Some(len)) = try_recv(self.register_port, &mut reg_buffer) {
                self.handle_register_message(&reg_buffer[..len]);
            }

            let mut event_buffer = [0u8; 64];
            while let Ok(Some(len)) = try_recv(self.event_port, &mut event_buffer) {
                self.handle_app_event(&event_buffer[..len]);
            }

            self.poll_input();

            if self.dirty {
                self.draw_all();
                self.dirty = false;
            }

            let _ = wait_any(&ports, 10);
            self.ticks = self.ticks.wrapping_add(1);
        }
    }

    fn poll_input(&mut self) {
        while let Some(event) = self.mouse_driver.poll_event() {
            self.cursor.apply_delta(event.dx, event.dy, self.fb.width(), self.fb.height());
            self.dirty = true;

            if event.left_button {
                if !self.mouse_left_down {
                    self.click_counter = self.click_counter.wrapping_add(1);
                    self.handle_click(self.cursor.x, self.cursor.y);
                } else {
                    self.handle_mouse_drag(self.cursor.x, self.cursor.y);
                }

                if matches!(self.drag_op, DragOperation::None) {
                    self.dispatch_mouse_move(self.cursor.x, self.cursor.y, event.dx as i16, event.dy as i16);
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
                    self.dispatch_mouse_move(self.cursor.x, self.cursor.y, event.dx as i16, event.dy as i16);
                }
                self.mouse_left_down = false;
                self.mouse_right_down = false;
            }
        }

        while let Some(scancode) = keyboard_poll() {
            let pressed = (scancode & 0x80) == 0;
            let code = scancode & 0x7F;

            match code {
                scancodes::LEFT_SHIFT | scancodes::RIGHT_SHIFT => {
                    self.keyboard_shift = pressed;
                }
                scancodes::ESCAPE if pressed => {
                    exit(0);
                }
                _ if pressed => {
                    if let Some(ascii) = scancode_to_ascii(code, self.keyboard_shift) {
                        self.dispatch_key_event(scancode, ascii as u8);
                    }
                }
                _ => {}
            }
        }
    }

    fn dispatch_mouse_move(&mut self, x: i32, y: i32, dx: i16, dy: i16) {
        let target_id = self.captured_window.or_else(|| self.wm.window_at(x, y));

        if let Some(id) = target_id {
            if let Some(w) = self.wm.get_window(id) {
                if let Some(port) = w.event_port {
                    if port != 0 {
                        let rel_x = x - w.content_x() as i32;
                        let rel_y = y - w.content_y() as i32;

                        let event = MouseMoveEvent {
                            x: rel_x,
                            y: rel_y,
                            dx,
                            dy,
                        };
                        let _ = send_message_async(port, MessageType::MouseMove, &event.to_bytes());
                    }
                }
            }
        }
    }

    fn dispatch_key_event(&mut self, scancode: u8, ascii: u8) {
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

        let event_port = self.wm.focused_id
            .and_then(|id| self.wm.get_window(id))
            .and_then(|w| w.event_port);

        if let Some(port) = event_port {
            if port != 0 {
                let _ = send_message_async(port, MessageType::KeyPress, &event.to_bytes());
            }
        }
    }

    fn handle_register_message(&mut self, data: &[u8]) {
        if data.len() < MessageHeader::SIZE {
            return;
        }

        let header = match MessageHeader::from_bytes(data) {
            Some(h) => h,
            None => return,
        };

        match header.msg_type {
            MessageType::AppRegister => self.handle_app_registration(data),
            MessageType::WmRequest => self.handle_wm_request(&data[MessageHeader::SIZE..]),
            MessageType::WmCommitFrame => {
                if let Some(msg) = libipc::messages::WmCommitFrameMsg::from_bytes(&data[MessageHeader::SIZE..]) {
                    if let Some(window) = self.wm.get_window_mut(msg.window_id) {
                        window.content_dirty = true;
                        self.dirty = true;
                    }
                }
            }
            MessageType::SurfacePresent => {
                let payload_start = MessageHeader::SIZE;
                if data.len() >= payload_start + libipc::messages::SurfacePresentMsg::SIZE {
                    if let Some(msg) = libipc::messages::SurfacePresentMsg::from_bytes(&data[payload_start..]) {
                        if let Some(window) = self.wm.get_window_mut(msg.window_id) {
                            window.content_dirty = true;
                            self.dirty = true;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_app_registration(&mut self, data: &[u8]) {
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

        if !self.pending_windows.is_empty() {
            let pending = self.pending_windows.remove(0);

            if let Some(window) = self.wm.get_window_mut(pending.window_id) {
                window.event_port = Some(reg_msg.app_port);

                if let Some(region_id) = window.surface_region_id {
                    let msg = SurfaceAssignMsg {
                        window_id: pending.window_id,
                        region_id,
                        width: window.content_width(),
                        height: window.content_height(),
                        stride: window.content_width(),
                        bytes_per_pixel: 4,
                        compositor_port: self.event_port,
                    };

                    let header = MessageHeader::new(MessageType::SurfaceAssign, SurfaceAssignMsg::SIZE as u32);
                    let header_bytes = header.to_bytes();
                    let payload_bytes = msg.to_bytes();

                    let mut full_msg = [0u8; 64];
                    full_msg[..MessageHeader::SIZE].copy_from_slice(&header_bytes);
                    full_msg[MessageHeader::SIZE..MessageHeader::SIZE + SurfaceAssignMsg::SIZE]
                        .copy_from_slice(&payload_bytes);

                    let _ = send(reg_msg.app_port, &full_msg[..MessageHeader::SIZE + SurfaceAssignMsg::SIZE]);
                }
            }
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
                if let Some(msg) = libipc::messages::WmCommitFrameMsg::from_bytes(&data[MessageHeader::SIZE..]) {
                    if let Some(window) = self.wm.get_window_mut(msg.window_id) {
                        window.content_dirty = true;
                        self.dirty = true;
                    }
                }
            }
            MessageType::MouseMove => {
                let payload_start = MessageHeader::SIZE;
                if let Some(event) = MouseMoveEvent::from_bytes(&data[payload_start..]) {
                    self.cursor.apply_delta(event.dx as i32, event.dy as i32, self.fb.width(), self.fb.height());
                    self.dirty = true;
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
            MessageType::SurfacePresent => {
                let payload_start = MessageHeader::SIZE;
                if data.len() >= payload_start + SurfacePresentMsg::SIZE {
                    if let Some(msg) = SurfacePresentMsg::from_bytes(&data[payload_start..]) {
                        if let Some(window) = self.wm.get_window_mut(msg.window_id) {
                            window.content_dirty = true;
                        }
                        self.dirty = true;
                    }
                }
            }
            MessageType::KeyPress => {
                let payload_start = MessageHeader::SIZE;
                if data.len() >= payload_start + 3 {
                    if let Some(key_event) = KeyEvent::from_bytes(&data[payload_start..]) {
                        let scancode = key_event.scancode & 0x7F;
                        if scancode == 0x01 {
                            exit(0);
                        }

                        let event_port = if let Some(focused_id) = self.wm.focused_id {
                            if let Some(window) = self.wm.get_window(focused_id) {
                                window.event_port
                            } else {
                                None
                            }
                        } else {
                            None
                        };

                        if let Some(port) = event_port {
                            if port != 0 {
                                let _ = send_message_async(port, MessageType::KeyPress, &key_event.to_bytes());
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn handle_click(&mut self, x: i32, y: i32) {
        if self.wallpaper_picker.visible {
            if self.handle_wallpaper_picker_click(x, y) {
                self.wallpaper_picker.visible = false;
                self.dirty = true;
                return;
            }
            if x < self.wallpaper_picker.x || x >= self.wallpaper_picker.x + self.wallpaper_picker.width as i32 ||
               y < self.wallpaper_picker.y || y >= self.wallpaper_picker.y + self.wallpaper_picker.height as i32 {
                self.wallpaper_picker.visible = false;
                self.dirty = true;
                return;
            }
            return;
        }

        if self.context_menu.visible {
            if self.handle_context_menu_click(x, y) {
                self.context_menu.visible = false;
                self.dirty = true;
                return;
            }
            self.context_menu.visible = false;
            self.dirty = true;
        }

        if let Some(id) = self.wm.window_at(x, y) {
            if self.wm.focused_id != Some(id) {
                self.wm.focus_window(id);
                self.dirty = true;
            }

            self.captured_window = Some(id);

            if let Some(w) = self.wm.get_window(id) {
                let btn_y = w.y + 10;
                let btn_size = 14;
                let close_x = w.x + w.width as i32 - 26;
                let max_x = close_x - 24;
                let min_x = max_x - 24;

                if y >= btn_y && y < btn_y + btn_size {
                    if x >= close_x && x < close_x + btn_size {
                        self.handle_close_window(id);
                        return;
                    } else if x >= max_x && x < max_x + btn_size {
                        let sw = self.fb.width();
                        let sh = self.fb.height();
                        self.wm.toggle_maximize(id, sw, sh);
                        self.dirty = true;
                        return;
                    } else if x >= min_x && x < min_x + btn_size {
                        self.wm.minimize_window(id);
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
                    if port != 0 {
                        let rel_x = x - w.content_x() as i32;
                        let rel_y = y - w.content_y() as i32;
                        if rel_x >= 0 && rel_y >= 0 && rel_x < w.content_width() as i32 && rel_y < w.content_height() as i32 {
                            let event = MouseButtonEvent {
                                button: MouseButton::Left,
                                x: rel_x,
                                y: rel_y,
                            };
                            let _ = send_message_async(port, MessageType::MouseButtonDown, &event.to_bytes());
                        }
                    }
                }
            }
            return;
        }

        if let Some(icon_index) = self.dock_icon_at(x, y) {
            self.handle_dock_click(icon_index);
            return;
        }

        if let Some(icon_idx) = self.icon_at(x, y) {
            let current = self.click_counter;
            if self.last_click_icon == Some(icon_idx) && current.wrapping_sub(self.last_click_tick) < 2 {
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
            DragOperation::Move { window_id, start_mouse_x, start_mouse_y, start_win_x, start_win_y } => {
                let dx = x - start_mouse_x;
                let dy = y - start_mouse_y;
                if let Some(w) = self.wm.get_window_mut(window_id) {
                    w.x = start_win_x + dx;
                    w.y = start_win_y + dy;
                    self.dirty = true;
                }
            }
            DragOperation::Resize { window_id, start_mouse_x, start_mouse_y, start_win_w, start_win_h } => {
                let dx = x - start_mouse_x;
                let dy = y - start_mouse_y;
                let new_w = (start_win_w as i32 + dx).max(WINDOW_MIN_WIDTH as i32) as u32;
                let new_h = (start_win_h as i32 + dy).max(WINDOW_MIN_HEIGHT as i32) as u32;
                self.wm.resize_window(window_id, new_w, new_h);
                self.dirty = true;
            }
            DragOperation::None => {}
        }
    }

    fn is_on_panel(&self, y: i32) -> bool {
        y >= 0 && y < PANEL_HEIGHT as i32
    }

    fn is_on_dock(&self, x: i32, y: i32) -> bool {
        self.dock_icon_at(x, y).is_some()
            || {
                let height = self.fb.height();
                y >= (height as i32 - DOCK_HEIGHT as i32 - 16)
            }
    }

    fn handle_right_click(&mut self, x: i32, y: i32) {
        self.context_menu.visible = false;

        if self.wm.window_at(x, y).is_some() {
            return;
        }

        if self.is_on_panel(y) {
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
        self.dirty = true;
    }

    fn handle_context_menu_click(&mut self, x: i32, y: i32) -> bool {
        let menu_w = 180u32;
        let item_h = 32u32;
        let menu_h = self.context_menu.items.len() as u32 * item_h;

        if x >= self.context_menu.x && x < self.context_menu.x + menu_w as i32 &&
           y >= self.context_menu.y && y < self.context_menu.y + menu_h as i32 {
            let item_idx = (y - self.context_menu.y) as u32 / item_h;
            if (item_idx as usize) < self.context_menu.items.len() {
                let action = &self.context_menu.items[item_idx as usize];
                if action == "Change Wallpaper" {
                    self.show_wallpaper_picker();
                }
                return true;
            }
        }
        false
    }

    fn show_wallpaper_picker(&mut self) {
        self.wallpaper_picker.visible = true;
        self.dirty = true;
    }

    fn handle_wallpaper_picker_click(&mut self, x: i32, y: i32) -> bool {
        let px = self.wallpaper_picker.x;
        let py = self.wallpaper_picker.y;

        if x >= px + self.wallpaper_picker.width as i32 - 32 && x < px + self.wallpaper_picker.width as i32 - 12 &&
           y >= py + 12 && y < py + 32 {
            return true;
        }

        let start_x = px + 30;
        let start_y = py + 60;
        let tile_size = 48u32;
        let spacing = 20u32;

        for (i, color) in self.wallpaper_picker.colors.iter().enumerate() {
            let tx = start_x + (i as i32 % 4) * (tile_size as i32 + spacing as i32);
            let ty = start_y + (i as i32 / 4) * (tile_size as i32 + spacing as i32);

            if x >= tx && x < tx + tile_size as i32 && y >= ty && y < ty + tile_size as i32 {
                self.desktop_bg = *color;
                self.dirty = true;
                return true;
            }
        }
        false
    }

    fn handle_mouse_up(&mut self, x: i32, y: i32) {
        let captured = self.captured_window.take();

        if !matches!(self.drag_op, DragOperation::None) {
            self.drag_op = DragOperation::None;
            return;
        }

        let target_id = captured.or_else(|| self.wm.window_at(x, y));

        if let Some(id) = target_id {
            if let Some(w) = self.wm.get_window(id) {
                if let Some(port) = w.event_port {
                    if port != 0 {
                        let rel_x = x - w.content_x() as i32;
                        let rel_y = y - w.content_y() as i32;
                        if rel_x >= 0 && rel_y >= 0 && rel_x < w.content_width() as i32 && rel_y < w.content_height() as i32 {
                            let event = MouseButtonEvent {
                                button: MouseButton::Left,
                                x: rel_x,
                                y: rel_y,
                            };
                            let _ = send_message_async(port, MessageType::MouseButtonUp, &event.to_bytes());
                        }
                    }
                }
            }
        }

        self.drag_op = DragOperation::None;
    }

    fn handle_wm_request(&mut self, payload: &[u8]) {
        if payload.len() < 4 { return; }
        let req_type_raw = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
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

                    let window = match Window::new_with_process(
                        id, &req.title, win_x, win_y, req.width, req.height,
                        0 as ProcessId, req.reply_port as PortId
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
                    self.dirty = true;

                    let header = MessageHeader::new(MessageType::WmResponse, libipc::messages::WmCreateWindowResponse::SIZE as u32);
                    let mut full_msg = [0u8; 64];
                    full_msg[..MessageHeader::SIZE].copy_from_slice(&header.to_bytes());
                    full_msg[MessageHeader::SIZE..MessageHeader::SIZE + libipc::messages::WmCreateWindowResponse::SIZE].copy_from_slice(&resp.to_bytes());
                    let _ = send(req.reply_port as PortId, &full_msg[..MessageHeader::SIZE + libipc::messages::WmCreateWindowResponse::SIZE]);
                }
            }
            _ => {}
        }
    }

    fn handle_close_window(&mut self, id: WindowId) {
        let mut event_port = None;
        if let Some(w) = self.wm.get_window(id) {
            event_port = w.event_port;
        }

        if let Some(port) = event_port {
            let msg = TerminateRequestMsg {
                window_id: id,
                reason: 0,
            };
            let header = MessageHeader::new(MessageType::TerminateRequest, TerminateRequestMsg::SIZE as u32);
            let mut full_msg = [0u8; 64];
            full_msg[..MessageHeader::SIZE].copy_from_slice(&header.to_bytes());
            full_msg[MessageHeader::SIZE..MessageHeader::SIZE + TerminateRequestMsg::SIZE].copy_from_slice(&msg.to_bytes());
            let _ = send(port, &full_msg[..MessageHeader::SIZE + TerminateRequestMsg::SIZE]);
        }

        self.wm.close_window(id);
        self.dirty = true;
    }

    fn dock_icon_at(&self, x: i32, y: i32) -> Option<usize> {
        let width = self.fb.width();
        let height = self.fb.height();

        let dock_x = (width / 2).saturating_sub(DOCK_WIDTH / 2) as i32;
        let dock_y = height.saturating_sub(DOCK_HEIGHT + 16) as i32;

        let icon_size = 44i32;
        let spacing = 24i32;
        let num_icons = 4;
        let total_icons_width = num_icons * icon_size + (num_icons - 1) * spacing;
        let start_x = dock_x + (DOCK_WIDTH as i32 - total_icons_width) / 2;
        let icon_y = dock_y + (DOCK_HEIGHT as i32 - icon_size) / 2;

        if y < icon_y || y >= icon_y + icon_size {
            return None;
        }

        for i in 0..4 {
            let ix = start_x + (i as i32 * (icon_size + spacing));
            if x >= ix && x < ix + icon_size {
                return Some(i);
            }
        }

        None
    }

    fn handle_dock_click(&mut self, icon_index: usize) {
        match icon_index {
            3 => {
                self.spawn_app("terminal");
            }
            _ => {}
        }
    }

    fn spawn_app(&mut self, name: &str) {
        if name == "terminal" {
            self.spawn_terminal();
            return;
        }

        let _ = spawn_process(name);
    }

    fn spawn_terminal(&mut self) {
        let pid = match spawn_process("terminal") {
            Ok(pid) => pid,
            Err(_) => return,
        };

        let offset = (self.wm.windows.len() as i32) * 30;
        let win_x = 150 + offset;
        let win_y = 120 + offset;
        let win_width = 640u32;
        let win_height = 420u32;

        let window_id = match self.wm.create_window_with_process(
            "Terminal",
            win_x,
            win_y,
            win_width,
            win_height,
            pid,
            0,
        ) {
            Some(id) => id,
            None => return,
        };

        self.pending_windows.push(PendingWindow {
            pid,
            window_id,
        });

        self.dirty = true;
    }

    fn draw_all(&mut self) {
        self.backbuffer_fb.fill_rect(0, 0, self.backbuffer_fb.width(), self.backbuffer_fb.height(), self.desktop_bg);

        self.draw_desktop_icons();
        self.draw_panel();

        for window in self.wm.windows.iter() {
            if window.visible {
                self.draw_window(window);
            }
        }

        self.draw_dock();

        if self.context_menu.visible {
            self.draw_context_menu();
        }

        if self.wallpaper_picker.visible {
            self.draw_wallpaper_picker();
        }

        self.draw_cursor();
        self.backbuffer_fb.blit(&self.fb);
    }

    fn draw_wallpaper_picker(&self) {
        let px = self.wallpaper_picker.x as u32;
        let py = self.wallpaper_picker.y as u32;
        let pw = self.wallpaper_picker.width;
        let ph = self.wallpaper_picker.height;

        self.backbuffer_fb.fill_rect(px + 4, py + 4, pw, ph, theme::SHADOW);
        self.backbuffer_fb.fill_rect(px, py, pw, ph, theme::WINDOW_BG);
        self.backbuffer_fb.draw_rect(px, py, pw, ph, theme::WINDOW_BORDER);

        self.backbuffer_fb.fill_rect(px + 1, py + 1, pw - 2, 40, theme::WINDOW_HEADER_FOCUSED);
        self.backbuffer_fb.draw_string(px + 16, py + 12, "Wallpaper", theme::PANEL_TEXT, theme::WINDOW_HEADER_FOCUSED);

        self.backbuffer_fb.fill_rect(px + pw - 32, py + 12, 20, 20, theme::BTN_CLOSE);
        self.backbuffer_fb.draw_string(px + pw - 28, py + 12, "X", Color::WHITE, theme::BTN_CLOSE);

        let start_x = px + 30;
        let start_y = py + 60;
        let tile_size = 48u32;
        let spacing = 20u32;

        for (i, color) in self.wallpaper_picker.colors.iter().enumerate() {
            let tx = start_x + (i as u32 % 4) * (tile_size + spacing);
            let ty = start_y + (i as u32 / 4) * (tile_size + spacing);

            self.backbuffer_fb.fill_rect(tx, ty, tile_size, tile_size, *color);
            self.backbuffer_fb.draw_rect(tx, ty, tile_size, tile_size, theme::WINDOW_BORDER);
        }
    }

    fn draw_context_menu(&self) {
        let menu_w = 180u32;
        let item_h = 32u32;
        let menu_h = self.context_menu.items.len() as u32 * item_h;

        self.backbuffer_fb.fill_rect(self.context_menu.x as u32 + 2, self.context_menu.y as u32 + 2, menu_w, menu_h, theme::SHADOW);
        self.backbuffer_fb.fill_rect(self.context_menu.x as u32, self.context_menu.y as u32, menu_w, menu_h, theme::PANEL_BG);
        self.backbuffer_fb.draw_rect(self.context_menu.x as u32, self.context_menu.y as u32, menu_w, menu_h, theme::WINDOW_BORDER);

        for (i, item) in self.context_menu.items.iter().enumerate() {
            let iy = self.context_menu.y as u32 + (i as u32 * item_h);
            self.backbuffer_fb.draw_string(self.context_menu.x as u32 + 12, iy + 8, item, theme::PANEL_TEXT, theme::PANEL_BG);
        }
    }

    fn icon_at(&self, x: i32, y: i32) -> Option<usize> {
        for (i, icon) in self.icons.iter().enumerate() {
            if x >= icon.x && x < icon.x + 56 && y >= icon.y && y < icon.y + 56 {
                return Some(i);
            }
        }
        None
    }

    fn draw_desktop_icons(&self) {
        for icon in &self.icons {
            let ix = icon.x as u32;
            let iy = icon.y as u32;
            let size = 56u32;

            self.backbuffer_fb.fill_rect(ix + 2, iy + 2, size, size, theme::SHADOW);
            self.backbuffer_fb.fill_rect(ix, iy, size, size, icon.color);
            self.backbuffer_fb.draw_rect(ix, iy, size, size, theme::WINDOW_BORDER);

            self.backbuffer_fb.draw_rect(ix + 14, iy + 14, 28, 28, Color::WHITE);

            let label_len = icon.label.len() as u32 * 8;
            let lx = (ix as i32 + (size as i32 - label_len as i32) / 2).max(0) as u32;
            self.backbuffer_fb.draw_string(lx, iy + size + 8, &icon.label, theme::PANEL_TEXT, self.desktop_bg);
        }
    }

    fn draw_panel(&self) {
        let width = self.backbuffer_fb.width();

        self.backbuffer_fb.fill_rect(0, 0, width, PANEL_HEIGHT, theme::PANEL_BG);
        self.backbuffer_fb.fill_rect(0, PANEL_HEIGHT - 1, width, 1, theme::WINDOW_BORDER);

        self.backbuffer_fb.draw_string(20, 8, "ATOM OS", theme::ACCENT, theme::PANEL_BG);

        let clock_x = width.saturating_sub(80);
        self.backbuffer_fb.draw_string(clock_x, 8, "12:00", theme::PANEL_TEXT, theme::PANEL_BG);
    }

    fn draw_window(&self, window: &Window) {
        if !window.visible || window.state == WindowState::Minimized {
            return;
        }

        let x = window.x as u32;
        let y = window.y as u32;
        let w = window.width;
        let h = window.height;

        if window.state != WindowState::Maximized {
            self.backbuffer_fb.fill_rect(x + 3, y + 3, w + 2, h + 2, theme::SHADOW);
        }

        self.backbuffer_fb.fill_rect(x, y, w, h, theme::WINDOW_BORDER);

        let header_color = if window.focused {
            theme::WINDOW_HEADER_FOCUSED
        } else {
            theme::WINDOW_HEADER
        };
        self.backbuffer_fb.fill_rect(x + WINDOW_BORDER_WIDTH, y + WINDOW_BORDER_WIDTH,
                         w - WINDOW_BORDER_WIDTH * 2, WINDOW_HEADER_HEIGHT - WINDOW_BORDER_WIDTH,
                         header_color);

        self.backbuffer_fb.draw_string(x + 16, y + 10, &window.title, theme::PANEL_TEXT, header_color);

        let btn_y = y + 10;
        let btn_size = 14;
        let close_x = x + w - 26;
        let max_x = close_x - 24;
        let min_x = max_x - 24;

        self.backbuffer_fb.fill_rect(close_x, btn_y, btn_size, btn_size, theme::BTN_CLOSE);
        self.backbuffer_fb.fill_rect(max_x, btn_y, btn_size, btn_size, theme::BTN_MAXIMIZE);
        self.backbuffer_fb.fill_rect(min_x, btn_y, btn_size, btn_size, theme::BTN_MINIMIZE);

        if window.surface.is_none() {
            self.backbuffer_fb.fill_rect(window.content_x(), window.content_y(),
                             window.content_width(), window.content_height(),
                             theme::WINDOW_BG);
        }

        if let Some(ref surface) = window.surface {
            surface.blit_to_framebuffer(&self.backbuffer_fb, window.content_x(), window.content_y());
        }

        self.backbuffer_fb.draw_rect(window.content_x() - 1, window.content_y() - 1,
                         window.content_width() + 2, window.content_height() + 2,
                         theme::WINDOW_BORDER);
    }

    fn draw_dock(&self) {
        let width = self.backbuffer_fb.width();
        let height = self.backbuffer_fb.height();

        let dock_x = (width / 2).saturating_sub(DOCK_WIDTH / 2);
        let dock_y = height.saturating_sub(DOCK_HEIGHT + 16);

        self.backbuffer_fb.fill_rect(dock_x + 2, dock_y + 2, DOCK_WIDTH, DOCK_HEIGHT, theme::SHADOW);
        self.backbuffer_fb.fill_rect(dock_x, dock_y, DOCK_WIDTH, DOCK_HEIGHT, theme::DOCK_BG);
        self.backbuffer_fb.draw_rect(dock_x, dock_y, DOCK_WIDTH, DOCK_HEIGHT, theme::WINDOW_BORDER);

        let icons = [
            (Color::new(136, 192, 208), "FL"),
            (Color::new(129, 161, 193), "ST"),
            (Color::new(94, 129, 172), "BR"),
            (Color::new(76, 86, 106), ">_"),
        ];

        let icon_size = 44u32;
        let spacing = 24u32;
        let total_icons_width = icons.len() as u32 * icon_size + (icons.len() as u32 - 1) * spacing;
        let start_x = dock_x + (DOCK_WIDTH - total_icons_width) / 2;
        let icon_y = dock_y + (DOCK_HEIGHT - icon_size) / 2;

        for (i, (color, label)) in icons.iter().enumerate() {
            let ix = start_x + (i as u32 * (icon_size + spacing));

            self.backbuffer_fb.fill_rect(ix, icon_y, icon_size, icon_size, *color);

            let label_len = label.len() as u32 * 8;
            let lx = ix + (icon_size - label_len) / 2;
            let ly = icon_y + (icon_size - 16) / 2;
            self.backbuffer_fb.draw_string(lx, ly, label, Color::WHITE, *color);
        }
    }

    fn draw_cursor(&self) {
        let cursor_shape = [
            [1,0,0,0,0,0,0,0,0,0],
            [1,1,0,0,0,0,0,0,0,0],
            [1,2,1,0,0,0,0,0,0,0],
            [1,2,2,1,0,0,0,0,0,0],
            [1,2,2,2,1,0,0,0,0,0],
            [1,2,2,2,2,1,0,0,0,0],
            [1,2,2,2,2,2,1,0,0,0],
            [1,2,2,2,2,2,2,1,0,0],
            [1,2,2,2,2,2,2,2,1,0],
            [1,2,2,2,2,2,2,2,2,1],
            [1,2,2,2,2,1,1,1,1,1],
            [1,2,1,2,1,0,0,0,0,0],
            [1,1,0,1,2,1,0,0,0,0],
            [0,0,0,1,2,1,0,0,0,0],
            [0,0,0,0,1,2,1,0,0,0],
            [0,0,0,0,1,1,0,0,0,0],
        ];

        for (row, cols) in cursor_shape.iter().enumerate() {
            for (col, &pixel) in cols.iter().enumerate() {
                let px = self.cursor.x as u32 + col as u32;
                let py = self.cursor.y as u32 + row as u32;
                if px < self.backbuffer_fb.width() && py < self.backbuffer_fb.height() {
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

    let fb_info = match get_framebuffer() {
        Some(info) => info,
        None => exit(1),
    };

    let fb_size = fb_info.stride as usize * fb_info.height as usize * fb_info.bytes_per_pixel as usize;
    let heap_size = fb_size + 8 * 1024 * 1024;

    let region_id = match shared_region_create(heap_size) {
        Ok(id) => id,
        Err(_) => exit(1),
    };

    let heap_start = match shared_region_map(region_id, 0, SharedMemFlags::READ_WRITE) {
        Ok(addr) => addr,
        Err(_) => exit(1),
    };

    ALLOCATOR.init(heap_start, heap_size);

    let fb = match Framebuffer::new() {
        Some(fb) => fb,
        None => exit(1),
    };

    let mut compositor = Compositor::new(fb);

    let _ = libipc::protocol::register_service("compositor", compositor.event_port);
    let _ = libipc::protocol::register_service("compositor.register", compositor.register_port);
    let _ = libipc::protocol::register_service("compositor.wm", compositor.register_port);

    compositor.run()
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("Desktop: PANIC!");
    exit(0xFF);
}