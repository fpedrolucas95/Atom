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

use libipc::messages::{MessageType, MessageHeader, SurfaceAssignMsg, TerminateRequestMsg, AppRegisterMsg, SurfacePresentMsg, KeyEvent as IpcKeyEvent};



use buffer::{DisplayBuffer, InputBuffer, History};
use commands::{CommandContext, CommandResult, execute};
use input::{InputHandler, KeyEvent};
use ipc_client::IpcClient;
use parser::parse_command;
use window::Theme;



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
}



impl Terminal {
    fn new(window_id: u32, compositor_port: PortId, local_port: PortId, width: u32, height: u32) -> Self {
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
            surface_width: width,
            surface_height: height,
            char_width: 8,
            char_height: 8,
            display_dirty: true,
            input_dirty: true,
            full_redraw_needed: true,
        }
    }

    /// Calculate number of columns
    fn cols(&self) -> u32 {
        self.surface_width / self.char_width
    }

    /// Calculate number of rows
    fn rows(&self) -> u32 {
        self.surface_height / self.char_height
    }



    /// Initialize the terminal
    fn init(&mut self, surface: &SharedSurface) {
        // Initialize IPC client
        self.ipc.init();

        // Set display dimensions based on surface size
        let rows = self.rows() as usize;
        let cols = self.cols() as usize;
        self.display.set_dimensions(rows, cols);

        // Clear surface with terminal background color
        surface.clear(Theme::WINDOW_BG);

        // Show welcome message
        self.show_welcome();

        // Show initial prompt
        self.show_prompt();

        // Render initial state
        if self.render(surface) {
            self.notify_present();
        }
    }



    /// Display welcome banner

    fn show_welcome(&mut self) {

        self.display.writeln("", Theme::TEXT_NORMAL);

        self.display.writeln("  Atom Terminal v0.1.0", Theme::TEXT_INFO);

        self.display.writeln("  Type 'help' for available commands.", Theme::TEXT_DIM);

        self.display.writeln("", Theme::TEXT_NORMAL);

        self.display_dirty = true;

    }



    /// Display the command prompt

    fn show_prompt(&mut self) {

        // Prompt format: user@atom:path$

        self.display.write_str("user", Theme::PROMPT_USER);

        self.display.write_str("@", Theme::TEXT_DIM);

        self.display.write_str("atom", Theme::PROMPT_USER);

        self.display.write_str(":", Theme::TEXT_DIM);

        self.display.write_str("/", Theme::PROMPT_PATH);

        self.display.write_str("$ ", Theme::PROMPT_SYMBOL);



        // Record prompt position for input display

        let (row, col) = self.display.cursor_position();

        self.prompt_row = row;

        self.prompt_col = col;

        self.display_dirty = true;

    }



    /// Handle a key event

    fn handle_key(&mut self, event: KeyEvent) {

        match event {

            KeyEvent::Char(ch) => {

                // Insert printable character

                if ch.is_ascii() && !ch.is_ascii_control() {

                    self.input.insert(ch as u8);
                    self.input_dirty = true;

                }

            }



            KeyEvent::Enter => {

                // First, write the input text to the display buffer so it persists
                let input_text = self.input.as_str();
                if !input_text.is_empty() {
                    self.display.write_str(input_text, Theme::TEXT_NORMAL);
                }

                // Move to next line
                self.display.newline();



                let cmd_str = self.input.as_str();

                if !cmd_str.is_empty() {

                    // Add to history

                    self.history.push(cmd_str);



                    // Parse and execute

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



                // Clear input buffer

                self.input.clear();



                // Show new prompt

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

                // Navigate history backward

                if let Some(prev) = self.history.previous() {

                    self.input.set(prev);
                    self.input_dirty = true;

                }

            }



            KeyEvent::ArrowDown => {

                // Navigate history forward

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



            KeyEvent::Tab => {

                // TODO: Tab completion

                // For now, just insert spaces

                for _ in 0..4 {

                    self.input.insert(b' ');

                }
                self.input_dirty = true;

            }



            KeyEvent::Escape => {

                // Clear current input

                self.input.clear();
                self.input_dirty = true;

            }



            KeyEvent::Control(ch) => {

                match ch {

                    '\x03' => {

                        // Ctrl+C - cancel current input

                        self.display.writeln("^C", Theme::TEXT_DIM);

                        self.input.clear();

                        self.show_prompt();

                        self.display_dirty = true;
                        self.input_dirty = true;

                    }

                    '\x04' => {

                        // Ctrl+D - exit (if input is empty)

                        if self.input.is_empty() {

                            self.running = false;

                        }

                    }

                    '\x0C' => {

                        // Ctrl+L - clear screen

                        self.display.clear();

                        self.show_prompt();

                        self.display_dirty = true;
                        self.input_dirty = true;

                    }

                    '\x01' => {

                        // Ctrl+A - beginning of line

                        self.input.cursor_home();
                        self.input_dirty = true;

                    }

                    '\x05' => {

                        // Ctrl+E - end of line

                        self.input.cursor_end();
                        self.input_dirty = true;

                    }

                    '\x15' => {

                        // Ctrl+U - clear line

                        self.input.clear();
                        self.input_dirty = true;

                    }

                    '\x0B' => {

                        // Ctrl+K - kill to end of line

                        while self.input.cursor() < self.input.len() {

                            self.input.delete();

                        }
                        self.input_dirty = true;

                    }

                    _ => {}

                }

            }



            _ => {

                // Ignore other keys

            }

        }

    }



    /// Render the terminal to the shared surface
    /// Returns true if anything was actually rendered
    fn render(&mut self, surface: &SharedSurface) -> bool {
        let rows = self.rows() as usize;
        let cols = self.cols() as usize;

        // Check if we need to render at all
        if !self.display_dirty && !self.input_dirty && !self.full_redraw_needed {
            return false;
        }

        // Track if we rendered anything
        let did_render = self.display_dirty || self.input_dirty || self.full_redraw_needed;

        // Render display buffer lines if needed
        if self.display_dirty || self.full_redraw_needed {
            for row in 0..rows {
                if let Some(line) = self.display.get_line(row) {
                    for col in 0..cols {
                        if let Some(cell) = line.get(col) {
                            self.draw_char(surface, row as u32, col as u32, cell.ch, cell.fg, cell.bg);
                        } else {
                            // Empty cell
                            self.draw_char(surface, row as u32, col as u32, b' ', Theme::TEXT_NORMAL, Theme::WINDOW_BG);
                        }
                    }
                } else {
                    // Clear empty row
                    self.clear_row(surface, row as u32);
                }
            }
        }

        // Render input line if needed
        if self.input_dirty || self.full_redraw_needed {
            let input_row = self.prompt_row;
            let input_start_col = self.prompt_col;

            // Clear the input area
            self.clear_to_eol(surface, input_row as u32, input_start_col as u32);

            // Draw input text
            let input_bytes = self.input.as_bytes();
            let cursor_pos = self.input.cursor();

            for (i, &byte) in input_bytes.iter().enumerate() {
                let col = input_start_col + i;
                if col < cols {
                    if i == cursor_pos {
                        // Cursor position - draw with inverted colors
                        self.draw_char_with_cursor(surface, input_row as u32, col as u32, byte);
                    } else {
                        self.draw_char(surface, input_row as u32, col as u32, byte, Theme::TEXT_NORMAL, Theme::WINDOW_BG);
                    }
                }
            }

            // Draw cursor at end if at end of input
            if cursor_pos >= input_bytes.len() {
                let col = input_start_col + input_bytes.len();
                if col < cols {
                    self.draw_cursor(surface, input_row as u32, col as u32);
                }
            }
        }

        // Reset dirty flags
        self.display_dirty = false;
        self.input_dirty = false;
        self.full_redraw_needed = false;

        did_render
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

        // Send to compositor
        let _ = send(self.compositor_port, &full_msg);
    }

    /// Draw a character at the given row/column position on the surface
    fn draw_char(&self, surface: &SharedSurface, row: u32, col: u32, ch: u8, fg: atom_syscall::graphics::Color, bg: atom_syscall::graphics::Color) {
        let x = col * self.char_width;
        let y = row * self.char_height;

        // Draw background
        surface.fill_rect(x, y, self.char_width, self.char_height, bg);
        // Draw character
        surface.draw_char(x, y, ch, fg, bg);
    }

    /// Draw cursor at the given position
    fn draw_cursor(&self, surface: &SharedSurface, row: u32, col: u32) {
        let x = col * self.char_width;
        let y = row * self.char_height;
        surface.fill_rect(x, y, self.char_width, self.char_height, Theme::CURSOR_BG);
    }

    /// Draw a character with cursor (inverted colors)
    fn draw_char_with_cursor(&self, surface: &SharedSurface, row: u32, col: u32, ch: u8) {
        let x = col * self.char_width;
        let y = row * self.char_height;

        // Draw cursor background
        surface.fill_rect(x, y, self.char_width, self.char_height, Theme::CURSOR_BG);
        // Draw character in inverted color
        surface.draw_char(x, y, ch, Theme::WINDOW_BG, Theme::CURSOR_BG);
    }

    /// Clear a specific row
    fn clear_row(&self, surface: &SharedSurface, row: u32) {
        let y = row * self.char_height;
        surface.fill_rect(0, y, self.surface_width, self.char_height, Theme::WINDOW_BG);
    }

    /// Clear from cursor position to end of row
    fn clear_to_eol(&self, surface: &SharedSurface, row: u32, col: u32) {
        let x = col * self.char_width;
        let y = row * self.char_height;
        let remaining_width = self.surface_width.saturating_sub(x);
        surface.fill_rect(x, y, remaining_width, self.char_height, Theme::WINDOW_BG);
    }



    /// Convert IPC KeyEvent to Terminal KeyEvent
    fn convert_ipc_key_event(&self, ipc_event: &IpcKeyEvent) -> Option<KeyEvent> {
        let character = ipc_event.character;
        let modifiers = &ipc_event.modifiers;

        // Handle special keys first (backspace, enter, tab, etc.)
        match character {
            0x08 => return Some(KeyEvent::Backspace),   // Backspace
            b'\n' => return Some(KeyEvent::Enter),       // Enter
            b'\t' => return Some(KeyEvent::Tab),         // Tab
            0x1B => return Some(KeyEvent::Escape),       // Escape
            _ => {}
        }

        // Handle control characters (Ctrl+letter)
        if modifiers.ctrl && character > 0 && character <= 0x1F {
            return Some(KeyEvent::Control(character as char));
        }

        // Handle printable characters
        if character >= 0x20 && character <= 0x7E {
            // ASCII printable range
            if modifiers.alt {
                return Some(KeyEvent::Alt(character as char));
            } else {
                return Some(KeyEvent::Char(character as char));
            }
        }

        // For now, ignore keys without ASCII representation
        // TODO: Handle arrow keys, function keys, etc. via extended scancodes
        None
    }

    /// Main event loop
    fn run(&mut self, surface: &SharedSurface) {
        log("Terminal: Entering main event loop");

        let mut msg_buffer = [0u8; 64];
        let ports = [self.local_port];

        while self.running {
            // First, drain any pending messages (non-blocking)
            while let Ok(Some(len)) = try_recv(self.local_port, &mut msg_buffer) {
                self.process_message(&msg_buffer, len);
            }

            // Render if needed and notify compositor only if we actually rendered
            if self.render(surface) {
                self.notify_present();
            }

            // Block waiting for messages with a timeout
            // This prevents busy-waiting and allows the system to be responsive
            // Timeout of 100ms allows for cursor blinking, periodic updates, etc.
            match wait_any(&ports, 100) {
                Ok(_) => {
                    // Message available, will be processed in next iteration
                }
                Err(_) => {
                    // Timeout or error, just continue the loop
                    // This allows for periodic tasks like cursor blinking
                }
            }
        }

        log("Terminal: Exiting");
    }

    /// Process a received IPC message
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
                MessageType::KeyPress => {
                    // Process keyboard event from compositor
                    let payload_start = MessageHeader::SIZE;
                    if len >= payload_start + 3 {
                        if let Some(ipc_event) = IpcKeyEvent::from_bytes(&msg_buffer[payload_start..]) {
                            // Convert IPC event to terminal event and handle it
                            if let Some(event) = self.convert_ipc_key_event(&ipc_event) {
                                self.handle_key(event);
                            }
                        }
                    }
                }
                _ => {
                    // Ignore unknown message types
                }
            }
        }
    }

    /// Poll for surface assignment from compositor
    fn wait_for_surface(port: PortId) -> Option<SurfaceAssignMsg> {
        let mut buffer = [0u8; 64];
        let mut attempts = 0;

        // Poll for surface assignment message (with timeout)
        while attempts < 1000 {
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
            yield_now();
            attempts += 1;
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

    // Create an IPC port to receive messages from compositor
    log("Terminal: About to call create_port");
    let local_port = match create_port() {
        Ok(port) => {
            log("Terminal: create_port returned Ok");
            log("Terminal: Port value received");
            port
        },
        Err(_) => {
            log("Terminal: Failed to create IPC port");
            exit(1);
        }
    };

    log("Terminal: After port assignment");
    log("Terminal: Preparing registration message");

    // Build registration message
    let mut full_msg = [0u8; 32];

    // Create header manually to avoid any potential issues
    let header = MessageHeader::new(MessageType::AppRegister, 16);
    let header_bytes = header.to_bytes();
    full_msg[0..12].copy_from_slice(&header_bytes);

    // Create payload manually (app_port + pid)
    full_msg[12..20].copy_from_slice(&local_port.to_le_bytes());
    full_msg[20..28].copy_from_slice(&0u64.to_le_bytes()); // pid = 0

    log("Terminal: Message built, sending to compositor ports");

    // Send to port 2 (likely the compositor's register_port)
    // The compositor creates two ports first: event_port (1) and register_port (2)
    let msg_slice = &full_msg[0..28]; // 12 bytes header + 16 bytes payload

    log("Terminal: Sending to port 1");
    let _ = send(1, msg_slice);

    log("Terminal: Sending to port 2");
    let _ = send(2, msg_slice);

    log("Terminal: Sending to port 3");
    let _ = send(3, msg_slice);

    log("Terminal: Messages sent, waiting for surface...");

    // Wait for surface assignment from compositor
    let surface_info = match Terminal::wait_for_surface(local_port) {
        Some(info) => info,
        None => {
            log("Terminal: Timeout waiting for surface assignment");
            exit(1);
        }
    };

    log("Terminal: Received surface assignment");

    // Map the shared surface into our address space
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

    // Create and initialize terminal
    let mut terminal = Terminal::new(
        surface_info.window_id,
        surface_info.compositor_port,
        local_port,
        surface_info.width,
        surface_info.height,
    );
    terminal.init(&surface);

    // Run main loop
    terminal.run(&surface);

    // Clean exit
    exit(0);
}



#[panic_handler]

fn panic(info: &PanicInfo) -> ! {

    // Log panic info

    log("Terminal: PANIC!");



    // Try to print panic location if available

    if let Some(location) = info.location() {

        log("Terminal: Panic at file:");

        // Note: Can't easily format the full message without alloc

    }



    exit(0xFF);

}