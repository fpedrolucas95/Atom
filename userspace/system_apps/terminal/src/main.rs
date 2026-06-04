// Atom Terminal - Userspace Terminal Emulator
//
// This is a true userspace application for Atom OS that provides an
// interactive command-line interface. It runs entirely in Ring 3 (userspace)
// and communicates with all system services exclusively via IPC.
//
// Architecture:
// - Window/Rendering: Receives shared surface from compositor via IPC
// - Input Handling: Receives keyboard events from input service
// - Command Parser: Tokenizes and parses user input
// - Command Execution: Executes built-in commands via IPC to services
// - Buffer Management: Manages display buffer and scrollback
//
// This terminal does NOT:
// - Access kernel internals directly
// - Link against kernel code
// - Use privileged CPU instructions
// - Directly access hardware (all via syscalls)
// - Acquire the global framebuffer (renders only to compositor-provided surface)

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

mod buffer;
mod commands;
mod input;
mod ipc_client;
mod parser;
mod window;

use core::panic::PanicInfo;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};
use alloc::boxed::Box;

// ============================================================================
// Simple Bump Allocator for Userspace
// ============================================================================

const HEAP_SIZE: usize = 512 * 1024; // 512 KB heap

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
        let size = layout.size();
        let align = layout.align().max(16);

        loop {
            let current = self.next.load(Ordering::Relaxed);
            let aligned = (current + align - 1) & !(align - 1);
            let new_next = aligned + size;

            if new_next > HEAP_SIZE {
                return core::ptr::null_mut();
            }

            if self.next.compare_exchange_weak(
                current, new_next, Ordering::SeqCst, Ordering::Relaxed
            ).is_ok() {
                return (self.heap.get() as *mut u8).add(aligned);
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        // Bump allocator doesn't free - memory is reclaimed when process exits
    }
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! {
    loop {}
}

use atom_syscall::graphics::SharedSurface;
use atom_syscall::ipc::{create_port, try_recv, send, wait_any, PortId};
use atom_syscall::thread::{exit, yield_now};
use atom_syscall::debug::log;

use libipc::messages::{
    MessageType, MessageHeader,
    SurfaceAssignMsg, SurfacePresentMsg,
    KeyEvent as IpcKeyEvent,
    MouseButtonEvent, MouseMoveEvent,
};

use buffer::{DisplayBuffer, InputBuffer, History};
use commands::{CommandContext, CommandResult, execute};
use input::{InputHandler, KeyEvent};
use ipc_client::IpcClient;
use parser::parse_command;
use window::Theme;

// ============================================================================
// Scrollbar constants
// ============================================================================

/// Width of the scrollbar strip on the right edge of the terminal.
const SCROLLBAR_W: u32 = 12;
/// Minimum height of the scrollbar thumb in pixels.
const SCROLLBAR_THUMB_MIN_H: u32 = 20;

/// Terminal state
struct Terminal {
    display: DisplayBuffer,
    input: InputBuffer,
    input_handler: InputHandler,
    history: History,
    ipc: IpcClient,
    running: bool,
    prompt_row: usize,
    prompt_col: usize,
    /// Window ID assigned by compositor
    window_id: u32,
    /// Port for communicating with compositor
    compositor_port: PortId,
    /// Our local IPC port for receiving messages
    local_port: PortId,
    /// Shared surface for rendering
    surface: Option<SharedSurface>,
    /// Surface dimensions
    surface_width: u32,
    surface_height: u32,
    /// Character dimensions
    char_width: u32,
    char_height: u32,
    /// Dirty tracking flags
    display_dirty: bool,
    input_dirty: bool,
    full_redraw_needed: bool,

    // ── Scrollbar drag state ──────────────────────────────────────────────────
    /// True while the user is dragging the scrollbar thumb.
    sb_dragging: bool,
    /// Y coordinate (window-relative) where the drag began.
    sb_drag_start_y: i32,
    /// view_offset at the moment the drag started.
    sb_drag_start_off: usize,
}



impl Terminal {
    fn new(window_id: u32, compositor_port: PortId, local_port: PortId, surface: SharedSurface) -> Self {
        let width = surface.width();
        let height = surface.height();
        Self {
            display: DisplayBuffer::new(),
            input: InputBuffer::new(),
            input_handler: InputHandler::new(),
            history: History::new(),
            ipc: IpcClient::new(),
            running: true,
            prompt_row: 0,
            prompt_col: 0,
            window_id,
            compositor_port,
            local_port,
            surface: Some(surface),
            surface_width: width,
            surface_height: height,
            char_width: 8,
            char_height: 8,
            display_dirty: true,
            input_dirty: true,
            full_redraw_needed: true,
            sb_dragging: false,
            sb_drag_start_y: 0,
            sb_drag_start_off: 0,
        }
    }

    // ── Layout helpers ────────────────────────────────────────────────────────

    /// Content columns, reserving SCROLLBAR_W pixels on the right.
    fn cols(&self) -> u32 {
        self.surface_width.saturating_sub(SCROLLBAR_W) / self.char_width
    }

    /// Content rows.
    fn rows(&self) -> u32 {
        self.surface_height / self.char_height
    }

    /// X coordinate of the left edge of the scrollbar.
    fn scrollbar_x(&self) -> u32 {
        self.surface_width.saturating_sub(SCROLLBAR_W)
    }

    // ── Scrollbar geometry helpers ────────────────────────────────────────────

    /// Compute thumb height and thumb top-y for the current scroll state.
    /// Returns `(thumb_h, thumb_y)` in pixels relative to the surface.
    fn scrollbar_thumb_rect(&self) -> (u32, u32) {
        let sh = self.surface_height;
        let max_off = self.display.max_view_offset();
        let max_rows = self.display.max_rows();

        if max_off == 0 || max_rows == 0 {
            // No scrollback: thumb fills the entire track.
            return (sh, 0);
        }

        let total = max_off + max_rows;
        let thumb_h = ((sh as usize * max_rows / total) as u32)
            .max(SCROLLBAR_THUMB_MIN_H)
            .min(sh);

        // view_offset = 0  → thumb at BOTTOM  → thumb_y = sh - thumb_h
        // view_offset = max → thumb at TOP    → thumb_y = 0
        let track_h = sh.saturating_sub(thumb_h);
        let view = self.display.view_offset();
        let thumb_y = if max_off == 0 {
            track_h
        } else {
            (track_h as usize * (max_off - view) / max_off) as u32
        };

        (thumb_h, thumb_y)
    }

    // ── Initialisation ────────────────────────────────────────────────────────

    /// Initialize the terminal
    fn init(&mut self) {
        self.ipc.init();

        let rows = self.rows() as usize;
        let cols = self.cols() as usize;
        self.display.set_dimensions(rows, cols);

        if let Some(ref surface) = self.surface {
            surface.clear(Theme::WINDOW_BG);
        }

        self.show_welcome();
        self.show_prompt();

        if self.render() {
            self.notify_present();
        }
    }

    // ── Welcome / prompt ──────────────────────────────────────────────────────

    fn show_welcome(&mut self) {
        self.display.writeln("", Theme::TEXT_NORMAL);
        self.display.writeln("  Atom Terminal v0.2.0", Theme::TEXT_INFO);
        self.display.writeln("  Type 'help' for available commands.", Theme::TEXT_DIM);
        self.display.writeln("", Theme::TEXT_NORMAL);
        self.display_dirty = true;
    }

    fn show_prompt(&mut self) {
        self.display.write_str("user", Theme::PROMPT_USER);
        self.display.write_str("@", Theme::TEXT_DIM);
        self.display.write_str("atom", Theme::PROMPT_USER);
        self.display.write_str(":", Theme::TEXT_DIM);
        self.display.write_str("/", Theme::PROMPT_PATH);
        self.display.write_str("$ ", Theme::PROMPT_SYMBOL);

        let (row, col) = self.display.cursor_position();
        self.prompt_row = row;
        self.prompt_col = col;
        self.display_dirty = true;
    }

    // ── Input handling ────────────────────────────────────────────────────────

    fn handle_key(&mut self, event: KeyEvent) {
        match event {
            KeyEvent::Char(ch) => {
                if ch.is_ascii() && !ch.is_ascii_control() {
                    self.input.insert(ch as u8);
                    self.input_dirty = true;
                }
            }

            KeyEvent::Enter => {
                // Snap to bottom so the user sees the new output.
                self.display.scroll_view_to_bottom();

                let input_text = self.input.as_str();
                if !input_text.is_empty() {
                    self.display.write_str(input_text, Theme::TEXT_NORMAL);
                }
                self.display.newline();

                let cmd_str = self.input.as_str();
                if !cmd_str.is_empty() {
                    self.history.push(cmd_str);

                    if let Some(cmd) = parse_command(cmd_str) {
                        let mut ctx = CommandContext {
                            display: &mut self.display,
                            ipc: &self.ipc,
                        };

                        match execute(&cmd, &mut ctx) {
                            CommandResult::Exit => {
                                self.running = false;
                                return;
                            }
                            CommandResult::Clear => {
                                self.display.clear();
                                self.full_redraw_needed = true;
                            }
                            _ => {}
                        }
                    }
                }

                self.input.clear();
                self.show_prompt();
                self.display_dirty = true;
                self.input_dirty = true;
            }

            KeyEvent::Backspace => {
                self.input.backspace();
                self.input_dirty = true;
            }

            KeyEvent::Delete => {
                self.input.delete();
                self.input_dirty = true;
            }

            KeyEvent::ArrowLeft => {
                self.input.cursor_left();
                self.input_dirty = true;
            }

            KeyEvent::ArrowRight => {
                self.input.cursor_right();
                self.input_dirty = true;
            }

            KeyEvent::ArrowUp => {
                if let Some(prev) = self.history.previous() {
                    self.input.set(prev);
                    self.input_dirty = true;
                }
            }

            KeyEvent::ArrowDown => {
                match self.history.next() {
                    Some(next) => self.input.set(next),
                    None => self.input.clear(),
                }
                self.input_dirty = true;
            }

            KeyEvent::Home => {
                self.input.cursor_home();
                self.input_dirty = true;
            }

            KeyEvent::End => {
                self.input.cursor_end();
                self.input_dirty = true;
            }

            KeyEvent::PageUp => {
                let page = self.rows() as usize;
                self.display.scroll_view_up(page);
                self.display_dirty = true;
                self.full_redraw_needed = true;
            }

            KeyEvent::PageDown => {
                let page = self.rows() as usize;
                self.display.scroll_view_down(page);
                self.display_dirty = true;
                self.full_redraw_needed = true;
            }

            KeyEvent::Tab => {
                for _ in 0..4 {
                    self.input.insert(b' ');
                }
                self.input_dirty = true;
            }

            KeyEvent::Escape => {
                self.input.clear();
                self.input_dirty = true;
            }

            KeyEvent::Control(ch) => {
                match ch {
                    '\x03' => {
                        self.display.writeln("^C", Theme::TEXT_DIM);
                        self.input.clear();
                        self.show_prompt();
                        self.display_dirty = true;
                        self.input_dirty = true;
                    }
                    '\x04' => {
                        if self.input.is_empty() {
                            self.running = false;
                        }
                    }
                    '\x0C' => {
                        self.display.clear();
                        self.show_prompt();
                        self.display_dirty = true;
                        self.input_dirty = true;
                    }
                    '\x01' => {
                        self.input.cursor_home();
                        self.input_dirty = true;
                    }
                    '\x05' => {
                        self.input.cursor_end();
                        self.input_dirty = true;
                    }
                    '\x15' => {
                        self.input.clear();
                        self.input_dirty = true;
                    }
                    '\x0B' => {
                        while self.input.cursor() < self.input.len() {
                            self.input.delete();
                        }
                        self.input_dirty = true;
                    }
                    _ => {}
                }
            }

            _ => {}
        }
    }

    // ── Mouse handling (scrollbar) ────────────────────────────────────────────

    fn handle_mouse_down(&mut self, x: i32, y: i32) {
        if x < self.scrollbar_x() as i32 {
            return; // Click was in the content area, ignore for scrollbar
        }

        let (thumb_h, thumb_y) = self.scrollbar_thumb_rect();
        let y_u = y.max(0) as u32;

        if y_u >= thumb_y && y_u < thumb_y + thumb_h {
            // Clicked inside the thumb – start drag.
            self.sb_dragging = true;
            self.sb_drag_start_y = y;
            self.sb_drag_start_off = self.display.view_offset();
        } else {
            // Clicked on the track (outside thumb) – page scroll.
            if (y_u as i32) < thumb_y as i32 {
                // Clicked above thumb → scroll toward older content (up).
                let page = self.rows() as usize;
                self.display.scroll_view_up(page);
            } else {
                // Clicked below thumb → scroll toward newer content (down).
                let page = self.rows() as usize;
                self.display.scroll_view_down(page);
            }
            self.display_dirty = true;
            self.full_redraw_needed = true;
        }
    }

    fn handle_mouse_up(&mut self, _x: i32, _y: i32) {
        self.sb_dragging = false;
    }

    fn handle_mouse_move(&mut self, _x: i32, y: i32) {
        if !self.sb_dragging {
            return;
        }

        let sh = self.surface_height;
        let (thumb_h, _) = self.scrollbar_thumb_rect();
        let track_h = sh.saturating_sub(thumb_h) as i32;
        let max_off = self.display.max_view_offset() as i32;

        if track_h <= 0 || max_off <= 0 {
            return;
        }

        // dy > 0 → dragged down → thumb moves toward bottom → view_offset decreases.
        let dy = y - self.sb_drag_start_y;
        let delta = dy * max_off / track_h;
        let new_off = (self.sb_drag_start_off as i32 - delta)
            .clamp(0, max_off) as usize;

        self.display.set_view_offset(new_off);
        self.display_dirty = true;
        self.full_redraw_needed = true;
    }

    // ── Rendering ─────────────────────────────────────────────────────────────

    /// Render the terminal to the shared surface.
    /// Returns true if anything was actually rendered.
    fn render(&mut self) -> bool {
        let surface = match self.surface {
            Some(ref s) => s,
            None => return false,
        };

        let rows = self.rows() as usize;
        let cols = self.cols() as usize;

        if !self.display_dirty && !self.input_dirty && !self.full_redraw_needed {
            return false;
        }

        // Render display buffer lines.
        if self.display_dirty || self.full_redraw_needed {
            for row in 0..rows {
                // Use view-aware getter so scrollback is shown when scrolled up.
                if let Some(line) = self.display.get_line_at_view(row) {
                    for col in 0..cols {
                        if let Some(cell) = line.get(col) {
                            self.draw_char(surface, row as u32, col as u32,
                                           cell.ch, cell.fg, cell.bg);
                        } else {
                            self.draw_char(surface, row as u32, col as u32,
                                           b' ', Theme::TEXT_NORMAL, Theme::WINDOW_BG);
                        }
                    }
                } else {
                    self.clear_row(surface, row as u32);
                }
            }
        }

        // Render the live input line only when at the bottom (not scrolled up).
        if (self.input_dirty || self.full_redraw_needed) && self.display.view_offset() == 0 {
            let input_row = self.prompt_row;
            let input_start_col = self.prompt_col;

            self.clear_to_eol(surface, input_row as u32, input_start_col as u32);

            let input_bytes = self.input.as_bytes();
            let cursor_pos = self.input.cursor();

            for (i, &byte) in input_bytes.iter().enumerate() {
                let col = input_start_col + i;
                if col < cols {
                    if i == cursor_pos {
                        self.draw_char_with_cursor(surface, input_row as u32, col as u32, byte);
                    } else {
                        self.draw_char(surface, input_row as u32, col as u32,
                                       byte, Theme::TEXT_NORMAL, Theme::WINDOW_BG);
                    }
                }
            }

            if cursor_pos >= input_bytes.len() {
                let col = input_start_col + input_bytes.len();
                if col < cols {
                    self.draw_cursor(surface, input_row as u32, col as u32);
                }
            }
        }

        // Always redraw the scrollbar.
        self.draw_scrollbar(surface);

        self.display_dirty = false;
        self.input_dirty = false;
        self.full_redraw_needed = false;

        true
    }

    /// Draw the vertical scrollbar on the right edge.
    fn draw_scrollbar(&self, surface: &SharedSurface) {
        let sx = self.scrollbar_x();
        let sh = self.surface_height;

        // Track background
        surface.fill_rect(sx, 0, SCROLLBAR_W, sh, Theme::SCROLLBAR_TRACK);

        let (thumb_h, thumb_y) = self.scrollbar_thumb_rect();

        // Separator line (left edge of scrollbar)
        surface.fill_rect(sx, 0, 1, sh, Theme::WINDOW_BORDER);

        // Thumb
        let thumb_color = if self.sb_dragging {
            Theme::SCROLLBAR_THUMB_ACTIVE
        } else {
            Theme::SCROLLBAR_THUMB
        };

        // 1-px margin on each horizontal side for a cleaner look
        let tx = sx + 2;
        let tw = SCROLLBAR_W.saturating_sub(4);
        if tw > 0 && thumb_h > 0 {
            surface.fill_rect(tx, thumb_y, tw, thumb_h, thumb_color);
        }
    }

    /// Notify the compositor that we've finished rendering and it should redraw
    fn notify_present(&self) {
        let present_msg = SurfacePresentMsg {
            window_id: self.window_id,
        };
        let header = MessageHeader::new(MessageType::SurfacePresent, SurfacePresentMsg::SIZE as u32);

        let header_bytes = header.to_bytes();
        let payload_bytes = present_msg.to_bytes();

        let mut full_msg = [0u8; MessageHeader::SIZE + SurfacePresentMsg::SIZE];
        full_msg[..MessageHeader::SIZE].copy_from_slice(&header_bytes);
        full_msg[MessageHeader::SIZE..].copy_from_slice(&payload_bytes);

        let _ = send(self.compositor_port, &full_msg);
    }

    // ── Draw primitives ───────────────────────────────────────────────────────

    fn draw_char(&self, surface: &SharedSurface, row: u32, col: u32,
                 ch: u8,
                 fg: atom_syscall::graphics::Color,
                 bg: atom_syscall::graphics::Color)
    {
        let x = col * self.char_width;
        let y = row * self.char_height;
        surface.fill_rect(x, y, self.char_width, self.char_height, bg);
        surface.draw_char(x, y, ch, fg, bg);
    }

    fn draw_cursor(&self, surface: &SharedSurface, row: u32, col: u32) {
        let x = col * self.char_width;
        let y = row * self.char_height;
        surface.fill_rect(x, y, self.char_width, self.char_height, Theme::CURSOR_BG);
    }

    fn draw_char_with_cursor(&self, surface: &SharedSurface, row: u32, col: u32, ch: u8) {
        let x = col * self.char_width;
        let y = row * self.char_height;
        surface.fill_rect(x, y, self.char_width, self.char_height, Theme::CURSOR_BG);
        surface.draw_char(x, y, ch, Theme::WINDOW_BG, Theme::CURSOR_BG);
    }

    fn clear_row(&self, surface: &SharedSurface, row: u32) {
        let y = row * self.char_height;
        let w = self.surface_width.saturating_sub(SCROLLBAR_W);
        surface.fill_rect(0, y, w, self.char_height, Theme::WINDOW_BG);
    }

    fn clear_to_eol(&self, surface: &SharedSurface, row: u32, col: u32) {
        let x = col * self.char_width;
        let y = row * self.char_height;
        let max_x = self.surface_width.saturating_sub(SCROLLBAR_W);
        let w = max_x.saturating_sub(x);
        surface.fill_rect(x, y, w, self.char_height, Theme::WINDOW_BG);
    }

    // ── IPC key conversion ────────────────────────────────────────────────────

    fn convert_ipc_key_event(&self, ipc_event: &IpcKeyEvent) -> Option<KeyEvent> {
        let character = ipc_event.character;
        let modifiers = &ipc_event.modifiers;

        // Handle special keys first (backspace, enter, tab, etc.)
        match character {
            0x08 => return Some(KeyEvent::Backspace),
            b'\n' => return Some(KeyEvent::Enter),
            b'\t' => return Some(KeyEvent::Tab),
            0x1B => return Some(KeyEvent::Escape),
            _ => {}
        }

        // Handle control characters (Ctrl+letter)
        if modifiers.ctrl && character > 0 && character <= 0x1F {
            return Some(KeyEvent::Control(character as char));
        }

        // Handle printable characters
        if character >= 0x20 && character <= 0x7E {
            if modifiers.alt {
                return Some(KeyEvent::Alt(character as char));
            } else {
                return Some(KeyEvent::Char(character as char));
            }
        }

        // Extended scancode-based keys forwarded with character == 0
        // Scancode byte is stored in ipc_event.scancode
        match ipc_event.scancode & 0x7F {
            0x49 => return Some(KeyEvent::PageUp),
            0x51 => return Some(KeyEvent::PageDown),
            0x48 => return Some(KeyEvent::ArrowUp),
            0x50 => return Some(KeyEvent::ArrowDown),
            0x4B => return Some(KeyEvent::ArrowLeft),
            0x4D => return Some(KeyEvent::ArrowRight),
            0x47 => return Some(KeyEvent::Home),
            0x4F => return Some(KeyEvent::End),
            _ => {}
        }

        None
    }

    // ── Main event loop ───────────────────────────────────────────────────────

    fn run(&mut self) {
        log("Terminal: Entering main event loop");

        let mut msg_buffer = [0u8; 64];
        let ports = [self.local_port];

        while self.running {
            while let Ok(Some(len)) = try_recv(self.local_port, &mut msg_buffer) {
                self.process_message(&msg_buffer, len);
            }

            if self.render() {
                self.notify_present();
            }

            match wait_any(&ports, 100) {
                Ok(_) => {}
                Err(_) => {}
            }
        }

        log("Terminal: Exiting");
    }

    fn process_message(&mut self, msg_buffer: &[u8], len: usize) {
        if len < MessageHeader::SIZE {
            return;
        }

        if let Some(header) = MessageHeader::from_bytes(msg_buffer) {
            match header.msg_type {
                MessageType::TerminateRequest => {
                    log("Terminal: Received terminate request");
                    self.running = false;
                }
                MessageType::SurfaceAssign => {
                    let payload_start = MessageHeader::SIZE;
                    if let Some(msg) = SurfaceAssignMsg::from_bytes(&msg_buffer[payload_start..]) {
                        log("Terminal: Received new surface assignment");
                        if let Ok(new_surface) = SharedSurface::from_region(
                            msg.region_id, msg.width, msg.height)
                        {
                            self.surface = Some(new_surface);
                            self.surface_width = msg.width;
                            self.surface_height = msg.height;
                            self.display.set_dimensions(
                                self.rows() as usize,
                                self.cols() as usize,
                            );
                            self.full_redraw_needed = true;
                        }
                    }
                }
                MessageType::KeyPress => {
                    let payload_start = MessageHeader::SIZE;
                    if len >= payload_start + 3 {
                        if let Some(ipc_event) = IpcKeyEvent::from_bytes(&msg_buffer[payload_start..]) {
                            if let Some(event) = self.convert_ipc_key_event(&ipc_event) {
                                self.handle_key(event);
                            }
                        }
                    }
                }
                MessageType::MouseButtonDown => {
                    let payload_start = MessageHeader::SIZE;
                    if let Some(ev) = MouseButtonEvent::from_bytes(&msg_buffer[payload_start..]) {
                        self.handle_mouse_down(ev.x, ev.y);
                    }
                }
                MessageType::MouseButtonUp => {
                    let payload_start = MessageHeader::SIZE;
                    if let Some(ev) = MouseButtonEvent::from_bytes(&msg_buffer[payload_start..]) {
                        self.handle_mouse_up(ev.x, ev.y);
                    }
                }
                MessageType::MouseMove => {
                    let payload_start = MessageHeader::SIZE;
                    if let Some(ev) = MouseMoveEvent::from_bytes(&msg_buffer[payload_start..]) {
                        self.handle_mouse_move(ev.x, ev.y);
                    }
                }
                _ => {}
            }
        }
    }

    fn wait_for_surface(port: PortId) -> Option<SurfaceAssignMsg> {
        let mut buffer = [0u8; 64];
        let ports = [port];

        for _ in 0..100 {
            if wait_any(&ports, 100).is_ok() {
                if let Ok(Some(len)) = try_recv(port, &mut buffer) {
                    if len >= MessageHeader::SIZE {
                        if let Some(header) = MessageHeader::from_bytes(&buffer) {
                            if header.msg_type == MessageType::SurfaceAssign {
                                let payload_start = MessageHeader::SIZE;
                                if len >= payload_start + SurfaceAssignMsg::SIZE {
                                    return SurfaceAssignMsg::from_bytes(&buffer[payload_start..]);
                                }
                            }
                        }
                    }
                }
            }
        }

        None
    }
}

/// Entry point
#[no_mangle]
pub extern "C" fn _start() -> ! {
    main()
}

fn main() -> ! {
    log("Terminal: Starting userspace terminal");

    let local_port = match create_port() {
        Ok(port) => port,
        Err(_) => {
            log("Terminal: Failed to create IPC port");
            exit(1);
        }
    };

    let _ = libipc::protocol::register_service("terminal", local_port);

    log("Terminal: Looking up compositor.register...");
    let register_port = loop {
        match libipc::protocol::lookup_service("compositor.register") {
            Ok(port) => {
                log("Terminal: Found compositor.register");
                break port;
            }
            Err(_) => {
                yield_now();
            }
        }
    };

    let mut full_msg = [0u8; 48];
    let header = MessageHeader::new(MessageType::AppRegister, 16);
    let header_bytes = header.to_bytes();
    full_msg[..MessageHeader::SIZE].copy_from_slice(&header_bytes);
    full_msg[MessageHeader::SIZE..MessageHeader::SIZE + 8]
        .copy_from_slice(&local_port.to_le_bytes());
    full_msg[MessageHeader::SIZE + 8..MessageHeader::SIZE + 16]
        .copy_from_slice(&0u64.to_le_bytes());

    let msg_len = MessageHeader::SIZE + 16;
    let _ = send(register_port, &full_msg[..msg_len]);

    log("Terminal: Message sent, waiting for surface...");

    let surface_info = match Terminal::wait_for_surface(local_port) {
        Some(info) => info,
        None => {
            log("Terminal: Timeout waiting for surface assignment");
            exit(1);
        }
    };

    log("Terminal: Received surface assignment");

    let surface = match SharedSurface::from_region(
        surface_info.region_id,
        surface_info.width,
        surface_info.height,
    ) {
        Ok(s) => s,
        Err(_) => {
            log("Terminal: Failed to map shared surface");
            exit(1);
        }
    };

    log("Terminal: Shared surface mapped successfully");
    log("Terminal: Constructing terminal state");

    let mut terminal = Box::new(Terminal::new(
        surface_info.window_id,
        surface_info.compositor_port,
        local_port,
        surface,
    ));
    log("Terminal: Terminal state constructed");
    terminal.init();
    terminal.run();

    exit(0);
}



#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    log("Terminal: PANIC!");
    exit(0xFF);
}
