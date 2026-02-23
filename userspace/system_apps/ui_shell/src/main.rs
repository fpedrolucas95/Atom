//! Atom Desktop Environment alpha

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
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
use atom_syscall::thread::exit;
use atom_syscall::debug::log;
use atom_syscall::process::{spawn_process, ProcessId};
use atom_syscall::input::{MouseDriver, keyboard_poll, scancode_to_ascii, scancodes};

use libipc::messages::{MessageType, MessageHeader, WindowId, SurfaceAssignMsg, TerminateRequestMsg, AppRegisterMsg, SurfacePresentMsg, KeyEvent, KeyModifiers, MouseMoveEvent, MouseButtonEvent, MouseButton};
use libipc::protocol::send_message_async;

mod theme {
    use atom_syscall::graphics::Color;

    // Desktop
    pub const DESKTOP_BG: Color = Color::new(18, 20, 28);

    // Panel (top bar)
    pub const PANEL_BG: Color = Color::new(14, 16, 22);
    pub const PANEL_BG_ACCENT: Color = Color::new(20, 23, 32);
    pub const PANEL_TEXT: Color = Color::new(210, 215, 225);
    pub const PANEL_TEXT_DIM: Color = Color::new(130, 138, 158);
    pub const PANEL_BORDER: Color = Color::new(38, 42, 56);

    // Accent
    pub const ACCENT: Color = Color::new(99, 143, 255);
    pub const ACCENT_DIM: Color = Color::new(70, 105, 200);

    // Windows
    pub const WINDOW_BG: Color = Color::new(26, 29, 38);
    pub const WINDOW_HEADER: Color = Color::new(22, 25, 34);
    pub const WINDOW_HEADER_FOCUSED: Color = Color::new(30, 34, 46);
    pub const WINDOW_BORDER: Color = Color::new(48, 54, 72);
    pub const WINDOW_BORDER_FOCUSED: Color = Color::new(65, 75, 100);

    // Dock
    pub const DOCK_BG: Color = Color::new(16, 18, 26);
    pub const DOCK_BORDER: Color = Color::new(42, 48, 64);
    pub const DOCK_ICON_BG: Color = Color::new(32, 36, 48);
    pub const DOCK_ICON_HOVER: Color = Color::new(44, 50, 66);

    // Cursor
    pub const CURSOR_FILL: Color = Color::WHITE;
    pub const CURSOR_OUTLINE: Color = Color::BLACK;

    // Shadows
    pub const SHADOW: Color = Color::new(4, 5, 8);
    pub const SHADOW_LIGHT: Color = Color::new(10, 12, 16);

    // Window buttons (traffic lights)
    pub const BTN_CLOSE: Color = Color::new(237, 78, 83);
    pub const BTN_MAXIMIZE: Color = Color::new(72, 199, 142);
    pub const BTN_MINIMIZE: Color = Color::new(245, 189, 65);
    pub const BTN_INACTIVE: Color = Color::new(56, 62, 78);

    // Context menu
    pub const MENU_BG: Color = Color::new(22, 25, 34);
    pub const MENU_HOVER: Color = Color::new(36, 42, 58);
    pub const MENU_BORDER: Color = Color::new(48, 54, 72);
    pub const MENU_TEXT: Color = Color::new(210, 215, 225);

    // Desktop icons
    pub const ICON_BG: Color = Color::new(28, 32, 44);
    pub const ICON_BORDER: Color = Color::new(48, 54, 72);
    pub const ICON_LABEL: Color = Color::new(200, 206, 218);
}

const WINDOW_HEADER_HEIGHT: u32 = 36;
const WINDOW_BORDER_WIDTH: u32 = 1;
const WINDOW_MIN_WIDTH: u32 = 150;
const WINDOW_MIN_HEIGHT: u32 = 100;
const PANEL_HEIGHT: u32 = 34;
const DOCK_HEIGHT: u32 = 64;
const DOCK_WIDTH: u32 = 380;

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
    /// True once the app has presented at least one frame into the current surface.
    /// Prevents blitting a blank/black surface after resize until the app redraws.
    surface_ready: bool,
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
            surface_ready: false,
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
            content_dirty: false,
            surface_ready: false,
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

    fn toggle_maximize(&mut self, id: WindowId, work_x: i32, work_y: i32, work_w: u32, work_h: u32) -> bool {
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

fn isqrt_helper(n: u32) -> u32 {
    if n == 0 { return 0; }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
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
            label: String::from("Files"),
            executable: String::from("fileman"),
            x: 28,
            y: PANEL_HEIGHT as i32 + 28,
            color: Color::new(99, 143, 255),
        });
        icons.push(DesktopIcon {
            label: String::from("Rectangles"),
            executable: String::from("demo_rects"),
            x: 28,
            y: PANEL_HEIGHT as i32 + 120,
            color: Color::new(86, 182, 245),
        });
        icons.push(DesktopIcon {
            label: String::from("Text"),
            executable: String::from("demo_text"),
            x: 28,
            y: PANEL_HEIGHT as i32 + 212,
            color: Color::new(72, 199, 142),
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
                    v.push(Color::new(18, 20, 28));   // Deep dark
                    v.push(Color::new(12, 14, 22));   // Darker
                    v.push(Color::new(24, 28, 42));   // Navy dark
                    v.push(Color::new(16, 24, 40));   // Deep blue
                    v.push(Color::new(22, 36, 52));   // Dark teal
                    v.push(Color::new(28, 18, 36));   // Dark purple
                    v.push(Color::new(20, 30, 24));   // Dark green
                    v.push(Color::new(34, 20, 20));   // Dark red
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
                        window.surface_ready = true;
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
                            window.surface_ready = true;
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

            let result = if let Some(window) = self.wm.get_window_mut(pending.window_id) {
                window.event_port = Some(reg_msg.app_port);
                window.surface_region_id.map(|rid| (rid, window.content_width(), window.content_height()))
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
                    None
                );
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
                        window.surface_ready = true;
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
                            window.surface_ready = true;
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
                let btn_y = w.y + (WINDOW_HEADER_HEIGHT as i32 - 12) / 2;
                let btn_size = 12;
                let close_x = w.x + w.width as i32 - 22;
                let max_x = close_x - 20;
                let min_x = max_x - 20;

                if y >= btn_y && y < btn_y + btn_size {
                    if x >= close_x && x < close_x + btn_size {
                        self.handle_close_window(id);
                        return;
                    } else if x >= max_x && x < max_x + btn_size {
                        let (wx, wy, ww, wh) = self.get_work_area();
                        if self.wm.toggle_maximize(id, wx, wy, ww, wh) {
                            self.reallocate_window_surface(id);
                            self.dirty = true;
                        }
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
                y >= (height as i32 - DOCK_HEIGHT as i32 - 12)
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

    fn get_work_area(&self) -> (i32, i32, u32, u32) {
        let sw = self.fb.width();
        let sh = self.fb.height();

        let x = 0;
        let y = PANEL_HEIGHT as i32;
        let w = sw;
        // Subtract panel height and dock area (bottom margin)
        let h = sh.saturating_sub(PANEL_HEIGHT + DOCK_HEIGHT + 24);

        (x, y, w, h)
    }

    fn send_surface_assignment(&self, window_id: WindowId, port: PortId, region_id: SharedRegionId, width: u32, height: u32, resize_pos: Option<(i32, i32)>) {
        // 1. Notify client about surface
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
        full_msg[MessageHeader::SIZE..MessageHeader::SIZE + SurfaceAssignMsg::SIZE].copy_from_slice(&assign.to_bytes());
        let _ = send(port, &full_msg[..MessageHeader::SIZE + SurfaceAssignMsg::SIZE]);

        if let Some((win_x, win_y)) = resize_pos {
            // 2. Notify client about resize event
            let resize_event = libipc::messages::WmWindowEventMsg {
                window_id,
                event_type: libipc::messages::WindowEventType::Resize,
                x: win_x,
                y: win_y,
                width,
                height,
            };

            let header = MessageHeader::new(MessageType::WmEvent, libipc::messages::WmWindowEventMsg::SIZE as u32);
            let mut full_msg = [0u8; 64];
            full_msg[..MessageHeader::SIZE].copy_from_slice(&header.to_bytes());
            full_msg[MessageHeader::SIZE..MessageHeader::SIZE + libipc::messages::WmWindowEventMsg::SIZE].copy_from_slice(&resize_event.to_bytes());
            let _ = send(port, &full_msg[..MessageHeader::SIZE + libipc::messages::WmWindowEventMsg::SIZE]);
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

        // Create new shared surface for the window
        if let Ok(new_surface) = SharedSurface::create(cw, ch) {
            let region_id = new_surface.region_id();

            let result = if let Some(window) = self.wm.get_window_mut(window_id) {
                window.surface = Some(new_surface);
                window.surface_region_id = Some(region_id);
                window.content_dirty = false;
                window.surface_ready = false;
                window.event_port.map(|port| (port, window.x, window.y))
            } else {
                None
            };

            if let Some((port, x, y)) = result {
                self.send_surface_assignment(window_id, port, region_id, cw, ch, Some((x, y)));
            }
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
        let dock_y = height.saturating_sub(DOCK_HEIGHT + 12) as i32;

        let icon_size = 42i32;
        let spacing = 20i32;
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
        if name == "fileman" {
            self.spawn_fileman();
            return;
        }

        let _ = spawn_process(name);
    }

    fn spawn_fileman(&mut self) {
        let pid = match spawn_process("fileman") {
            Ok(pid) => pid,
            Err(_) => return,
        };

        let offset = (self.wm.windows.len() as i32) * 30;
        let win_x = 100 + offset;
        let win_y = 80 + offset;
        let win_width = 720u32;
        let win_height = 480u32;

        let window_id = match self.wm.create_window_with_process(
            "File Manager",
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

        // Shadow
        self.backbuffer_fb.fill_rect_rounded_alpha(px + 2, py + 4, pw + 2, ph + 2, 10, theme::SHADOW, 100);
        // Background
        self.backbuffer_fb.fill_rect_rounded(px, py, pw, ph, 10, theme::WINDOW_BG);
        // Border
        self.backbuffer_fb.draw_rect_rounded(px, py, pw, ph, 10, theme::WINDOW_BORDER);

        // Header
        self.backbuffer_fb.fill_rect(px + 1, py + 1, pw - 2, 40, theme::WINDOW_HEADER_FOCUSED);
        let title_y = py + (40 - 8) / 2;
        self.backbuffer_fb.draw_string(px + 16, title_y, "Wallpaper", theme::PANEL_TEXT, theme::WINDOW_HEADER_FOCUSED);

        // Close button (rounded)
        let close_x = px + pw - 30;
        let close_y = py + 14;
        self.backbuffer_fb.fill_rect_rounded(close_x, close_y, 16, 16, 8, theme::BTN_CLOSE);
        self.backbuffer_fb.draw_string(close_x + 4, close_y + 4, "X", Color::WHITE, theme::BTN_CLOSE);

        // Color tiles (rounded)
        let start_x = px + 30;
        let start_y = py + 60;
        let tile_size = 48u32;
        let spacing = 20u32;

        for (i, color) in self.wallpaper_picker.colors.iter().enumerate() {
            let tx = start_x + (i as u32 % 4) * (tile_size + spacing);
            let ty = start_y + (i as u32 / 4) * (tile_size + spacing);

            self.backbuffer_fb.fill_rect_rounded(tx, ty, tile_size, tile_size, 8, *color);
            self.backbuffer_fb.draw_rect_rounded(tx, ty, tile_size, tile_size, 8, theme::WINDOW_BORDER);
        }
    }

    fn draw_context_menu(&self) {
        let menu_w = 200u32;
        let item_h = 32u32;
        let padding_v = 6u32;
        let menu_h = self.context_menu.items.len() as u32 * item_h + padding_v * 2;
        let mx = self.context_menu.x as u32;
        let my = self.context_menu.y as u32;

        // Shadow
        self.backbuffer_fb.fill_rect_rounded_alpha(mx + 2, my + 3, menu_w, menu_h, 8, theme::SHADOW, 100);
        // Background
        self.backbuffer_fb.fill_rect_rounded(mx, my, menu_w, menu_h, 8, theme::MENU_BG);
        // Border
        self.backbuffer_fb.draw_rect_rounded(mx, my, menu_w, menu_h, 8, theme::MENU_BORDER);

        for (i, item) in self.context_menu.items.iter().enumerate() {
            let iy = my + padding_v + (i as u32 * item_h);
            let text_y = iy + (item_h - 8) / 2;
            self.backbuffer_fb.draw_string(mx + 16, text_y, item, theme::MENU_TEXT, theme::MENU_BG);
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

    fn draw_desktop_icons(&self) {
        for icon in &self.icons {
            let ix = icon.x as u32;
            let iy = icon.y as u32;
            let size = 60u32;
            let icon_radius = 12u32;
            let inner_size = 28u32;

            // Icon shadow
            self.backbuffer_fb.fill_rect_rounded_alpha(ix + 1, iy + 2, size, size, icon_radius, theme::SHADOW, 70);

            // Icon background (rounded)
            self.backbuffer_fb.fill_rect_rounded(ix, iy, size, size, icon_radius, theme::ICON_BG);

            // Colored inner icon (rounded)
            let inner_x = ix + (size - inner_size) / 2;
            let inner_y = iy + (size - inner_size) / 2 - 2;
            self.backbuffer_fb.fill_rect_rounded(inner_x, inner_y, inner_size, inner_size, 6, icon.color);

            // Icon border (subtle)
            self.backbuffer_fb.draw_rect_rounded(ix, iy, size, size, icon_radius, theme::ICON_BORDER);

            // Label below icon (centered, with slight shadow for readability)
            let label_len = icon.label.len() as u32 * 8;
            let lx = (ix as i32 + (size as i32 - label_len as i32) / 2).max(0) as u32;
            let label_y = iy + size + 6;
            self.backbuffer_fb.draw_string(lx, label_y, &icon.label, theme::ICON_LABEL, self.desktop_bg);
        }
    }

    fn draw_panel(&self) {
        let width = self.backbuffer_fb.width();

        // Panel background
        self.backbuffer_fb.fill_rect(0, 0, width, PANEL_HEIGHT, theme::PANEL_BG);
        // Subtle top highlight
        self.backbuffer_fb.fill_rect(0, 0, width, 1, theme::PANEL_BG_ACCENT);
        // Bottom border
        self.backbuffer_fb.fill_rect(0, PANEL_HEIGHT - 1, width, 1, theme::PANEL_BORDER);

        // Branding: Atom logo area
        let brand_y = (PANEL_HEIGHT - 8) / 2;
        // Accent dot
        self.backbuffer_fb.fill_rect_rounded(14, brand_y - 1, 10, 10, 3, theme::ACCENT);
        // Brand text
        self.backbuffer_fb.draw_string(28, brand_y, "Atom", theme::PANEL_TEXT, theme::PANEL_BG);

        // Center: focused window title (if any)
        if let Some(focused_id) = self.wm.focused_id {
            if let Some(w) = self.wm.get_window(focused_id) {
                let title_len = w.title.len() as u32 * 8;
                let title_x = (width / 2).saturating_sub(title_len / 2);
                self.backbuffer_fb.draw_string(title_x, brand_y, &w.title, theme::PANEL_TEXT_DIM, theme::PANEL_BG);
            }
        }

        // Right side: clock + status area
        let clock_x = width.saturating_sub(96);
        // Status dot (indicates system running)
        self.backbuffer_fb.fill_rect_rounded(clock_x - 16, brand_y, 8, 8, 4, theme::BTN_MAXIMIZE);
        self.backbuffer_fb.draw_string(clock_x, brand_y, "12:00 PM", theme::PANEL_TEXT, theme::PANEL_BG);
    }

    fn draw_window(&self, window: &Window) {
        if !window.visible || window.state == WindowState::Minimized {
            return;
        }

        let x = window.x as u32;
        let y = window.y as u32;
        let w = window.width;
        let h = window.height;

        // Multi-layer soft shadow (only for non-maximized)
        if window.state != WindowState::Maximized {
            self.backbuffer_fb.fill_rect_rounded_alpha(x + 2, y + 4, w + 4, h + 4, 6, theme::SHADOW, 60);
            self.backbuffer_fb.fill_rect_rounded_alpha(x + 1, y + 2, w + 2, h + 2, 4, theme::SHADOW, 90);
        }

        let border_color = if window.focused {
            theme::WINDOW_BORDER_FOCUSED
        } else {
            theme::WINDOW_BORDER
        };

        // Window outer border (rounded)
        self.backbuffer_fb.fill_rect_rounded(x, y, w, h, 6, border_color);

        // Window header
        let header_color = if window.focused {
            theme::WINDOW_HEADER_FOCUSED
        } else {
            theme::WINDOW_HEADER
        };
        self.backbuffer_fb.fill_rect(x + WINDOW_BORDER_WIDTH, y + WINDOW_BORDER_WIDTH,
                         w - WINDOW_BORDER_WIDTH * 2, WINDOW_HEADER_HEIGHT - WINDOW_BORDER_WIDTH,
                         header_color);
        // Header bottom separator
        self.backbuffer_fb.fill_rect(x + WINDOW_BORDER_WIDTH, y + WINDOW_HEADER_HEIGHT - 1,
                         w - WINDOW_BORDER_WIDTH * 2, 1, border_color);

        // Window title (vertically centered in header)
        let title_y = y + (WINDOW_HEADER_HEIGHT - 8) / 2;
        let title_color = if window.focused { theme::PANEL_TEXT } else { theme::PANEL_TEXT_DIM };
        self.backbuffer_fb.draw_string(x + 16, title_y, &window.title, title_color, header_color);

        // Traffic light buttons (rounded circles)
        let btn_y = y + (WINDOW_HEADER_HEIGHT - 12) / 2;
        let btn_size = 12u32;
        let btn_radius = 6u32;
        let close_x = x + w - 22;
        let max_x = close_x - 20;
        let min_x = max_x - 20;

        if window.focused {
            self.backbuffer_fb.fill_rect_rounded(close_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_CLOSE);
            self.backbuffer_fb.fill_rect_rounded(max_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_MAXIMIZE);
            self.backbuffer_fb.fill_rect_rounded(min_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_MINIMIZE);
        } else {
            self.backbuffer_fb.fill_rect_rounded(close_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_INACTIVE);
            self.backbuffer_fb.fill_rect_rounded(max_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_INACTIVE);
            self.backbuffer_fb.fill_rect_rounded(min_x, btn_y, btn_size, btn_size, btn_radius, theme::BTN_INACTIVE);
        }

        // Content area background
        if window.surface.is_none() || !window.surface_ready {
            self.backbuffer_fb.fill_rect(window.content_x(), window.content_y(),
                             window.content_width(), window.content_height(),
                             theme::WINDOW_BG);
        }

        // Blit application surface
        if window.surface_ready {
            if let Some(ref surface) = window.surface {
                surface.blit_to_framebuffer(&self.backbuffer_fb, window.content_x(), window.content_y());
            }
        }

        // Bottom rounded corners fill
        if window.state != WindowState::Maximized {
            let bottom_y = y + h - 6;
            for dy in 0..6 {
                let fy = 6 - dy;
                let offset = 6u32.saturating_sub(isqrt_helper(6 * 6 - fy * fy));
                // Fill corner pixels with border color to simulate rounded bottom
                for cx in 0..offset {
                    self.backbuffer_fb.draw_pixel(x + cx, bottom_y + dy, self.desktop_bg);
                    self.backbuffer_fb.draw_pixel(x + w - 1 - cx, bottom_y + dy, self.desktop_bg);
                }
            }
        }
    }

    fn draw_dock(&self) {
        let width = self.backbuffer_fb.width();
        let height = self.backbuffer_fb.height();

        let dock_x = (width / 2).saturating_sub(DOCK_WIDTH / 2);
        let dock_y = height.saturating_sub(DOCK_HEIGHT + 12);

        // Dock shadow
        self.backbuffer_fb.fill_rect_rounded_alpha(dock_x + 2, dock_y + 3, DOCK_WIDTH, DOCK_HEIGHT, 14, theme::SHADOW, 80);

        // Dock background (pill shape)
        self.backbuffer_fb.fill_rect_rounded(dock_x, dock_y, DOCK_WIDTH, DOCK_HEIGHT, 14, theme::DOCK_BG);
        // Dock border
        self.backbuffer_fb.draw_rect_rounded(dock_x, dock_y, DOCK_WIDTH, DOCK_HEIGHT, 14, theme::DOCK_BORDER);
        // Top highlight line
        self.backbuffer_fb.fill_rect(dock_x + 14, dock_y, DOCK_WIDTH - 28, 1, theme::DOCK_BORDER);

        let icons: [(Color, &str, &str); 4] = [
            (Color::new(99, 143, 255), "FL", "Files"),
            (Color::new(86, 182, 245), "ST", "Settings"),
            (Color::new(72, 199, 142), "BR", "Browser"),
            (Color::new(200, 160, 255), ">_", "Terminal"),
        ];

        let icon_size = 42u32;
        let icon_radius = 10u32;
        let spacing = 20u32;
        let total_icons_width = icons.len() as u32 * icon_size + (icons.len() as u32 - 1) * spacing;
        let start_x = dock_x + (DOCK_WIDTH - total_icons_width) / 2;
        let icon_y = dock_y + (DOCK_HEIGHT - icon_size) / 2;

        for (i, (color, label, _name)) in icons.iter().enumerate() {
            let ix = start_x + (i as u32 * (icon_size + spacing));

            // Icon background (rounded)
            self.backbuffer_fb.fill_rect_rounded(ix, icon_y, icon_size, icon_size, icon_radius, *color);

            // Icon label centered
            let label_len = label.len() as u32 * 8;
            let lx = ix + (icon_size - label_len) / 2;
            let ly = icon_y + (icon_size - 8) / 2;
            self.backbuffer_fb.draw_string(lx, ly, label, Color::WHITE, *color);

            // Active indicator dot for running apps (optional visual)
            if i == 3 && self.wm.windows.iter().any(|w| w.title == "Terminal" && w.visible) {
                let dot_x = ix + icon_size / 2 - 2;
                let dot_y = icon_y + icon_size + 3;
                self.backbuffer_fb.fill_rect_rounded(dot_x, dot_y, 4, 4, 2, theme::ACCENT);
            }
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