// Atom Terminal - Userspace Terminal Emulator

//

// This is a true userspace application for Atom OS that provides an

// interactive command-line interface. It runs entirely in Ring 3 (userspace)

// and communicates with all system services exclusively via IPC.

//

// Architecture:

// - Window/Rendering: Communicates with display server for graphics

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



#![no_std]

#![no_main]


extern crate alloc;

// Use shared allocator from syscall library
atom_syscall::define_global_allocator!();



mod buffer;

mod commands;

mod input;

mod ipc_client;

mod parser;

mod window;



use core::panic::PanicInfo;



use atom_syscall::graphics::Framebuffer;

use atom_syscall::thread::{exit, yield_now};

use atom_syscall::debug::log;



use buffer::{DisplayBuffer, InputBuffer, History};

use commands::{CommandContext, CommandResult, execute};

use input::{InputHandler, KeyEvent};

use ipc_client::IpcClient;

use parser::parse_command;

use window::{TerminalWindow, Theme};



/// Terminal state

struct Terminal {

    window: TerminalWindow,

    display: DisplayBuffer,

    input: InputBuffer,

    input_handler: InputHandler,

    history: History,

    ipc: IpcClient,

    running: bool,

    prompt_row: usize,

    prompt_col: usize,

    /// IPC port for receiving keyboard events from compositor
    event_port: Option<atom_syscall::ipc::PortId>,

}



impl Terminal {

    fn new() -> Self {

        Self {

            window: TerminalWindow::new("Atom Terminal"),

            display: DisplayBuffer::new(),

            input: InputBuffer::new(),

            input_handler: InputHandler::new(),

            history: History::new(),

            ipc: IpcClient::new(),

            running: true,

            prompt_row: 0,

            prompt_col: 0,

            event_port: None,

        }

    }



    /// Initialize the terminal

    fn init(&mut self, fb: &Framebuffer) {

        // Initialize IPC client

        self.ipc.init();


        // Create IPC port for receiving events from compositor
        match atom_syscall::ipc::create_port() {
            Ok(port) => {
                self.event_port = Some(port);
                log("Terminal: Created IPC event port");
            }
            Err(_) => {
                log("Terminal: Failed to create IPC event port");
            }
        }


        // Set display dimensions from window config

        let cfg = self.window.config();

        let rows = cfg.rows() as usize;

        let cols = cfg.cols() as usize;

        self.display.set_dimensions(rows, cols);



        // Draw window frame

        self.window.draw_frame(fb);



        // Show welcome message

        self.show_welcome();



        // Show initial prompt

        self.show_prompt();



        // Render initial state

        self.render(fb);

    }



    /// Display welcome banner

    fn show_welcome(&mut self) {

        self.display.writeln("", Theme::TEXT_NORMAL);

        self.display.writeln("  Atom Terminal v0.1.0", Theme::TEXT_INFO);

        self.display.writeln("  Type 'help' for available commands.", Theme::TEXT_DIM);

        self.display.writeln("", Theme::TEXT_NORMAL);

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

    }



    /// Handle a key event

    fn handle_key(&mut self, event: KeyEvent) {

        match event {

            KeyEvent::Char(ch) => {

                // Insert printable character

                if ch.is_ascii() && !ch.is_ascii_control() {

                    self.input.insert(ch as u8);

                }

            }



            KeyEvent::Enter => {

                // Execute command

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

                            }

                            _ => {}

                        }

                    }

                }



                // Clear input buffer

                self.input.clear();



                // Show new prompt

                self.show_prompt();

            }



            KeyEvent::Backspace => {

                self.input.backspace();

            }



            KeyEvent::Delete => {

                self.input.delete();

            }



            KeyEvent::ArrowLeft => {

                self.input.cursor_left();

            }



            KeyEvent::ArrowRight => {

                self.input.cursor_right();

            }



            KeyEvent::ArrowUp => {

                // Navigate history backward

                if let Some(prev) = self.history.previous() {

                    self.input.set(prev);

                }

            }



            KeyEvent::ArrowDown => {

                // Navigate history forward

                match self.history.next() {

                    Some(next) => self.input.set(next),

                    None => self.input.clear(),

                }

            }



            KeyEvent::Home => {

                self.input.cursor_home();

            }



            KeyEvent::End => {

                self.input.cursor_end();

            }



            KeyEvent::Tab => {

                // TODO: Tab completion

                // For now, just insert spaces

                for _ in 0..4 {

                    self.input.insert(b' ');

                }

            }



            KeyEvent::Escape => {

                // Clear current input

                self.input.clear();

            }



            KeyEvent::Control(ch) => {

                match ch {

                    '\x03' => {

                        // Ctrl+C - cancel current input

                        self.display.writeln("^C", Theme::TEXT_DIM);

                        self.input.clear();

                        self.show_prompt();

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

                    }

                    '\x01' => {

                        // Ctrl+A - beginning of line

                        self.input.cursor_home();

                    }

                    '\x05' => {

                        // Ctrl+E - end of line

                        self.input.cursor_end();

                    }

                    '\x15' => {

                        // Ctrl+U - clear line

                        self.input.clear();

                    }

                    '\x0B' => {

                        // Ctrl+K - kill to end of line

                        while self.input.cursor() < self.input.len() {

                            self.input.delete();

                        }

                    }

                    _ => {}

                }

            }



            _ => {

                // Ignore other keys

            }

        }

    }



    /// Render the terminal to the framebuffer

    fn render(&self, fb: &Framebuffer) {

        let cfg = self.window.config();

        let rows = cfg.rows() as usize;

        let cols = cfg.cols() as usize;



        // Render display buffer lines

        for row in 0..rows {

            if let Some(line) = self.display.get_line(row) {

                for col in 0..cols {

                    if let Some(cell) = line.get(col) {

                        self.window.draw_char(fb, row as u32, col as u32, cell.ch, cell.fg, cell.bg);

                    } else {

                        // Empty cell

                        self.window.draw_char(fb, row as u32, col as u32, b' ', Theme::TEXT_NORMAL, Theme::WINDOW_BG);

                    }

                }

            } else {

                // Clear empty row

                self.window.clear_row(fb, row as u32);

            }

        }



        // Render input line on top of buffer content at prompt position

        let input_row = self.prompt_row;

        let input_start_col = self.prompt_col;



        // Clear the input area

        self.window.clear_to_eol(fb, input_row as u32, input_start_col as u32);



        // Draw input text

        let input_bytes = self.input.as_bytes();

        let cursor_pos = self.input.cursor();



        for (i, &byte) in input_bytes.iter().enumerate() {

            let col = input_start_col + i;

            if col < cols {

                if i == cursor_pos {

                    // Cursor position - draw with inverted colors

                    self.window.draw_char_with_cursor(fb, input_row as u32, col as u32, byte);

                } else {

                    self.window.draw_char(fb, input_row as u32, col as u32, byte, Theme::TEXT_NORMAL, Theme::WINDOW_BG);

                }

            }

        }



        // Draw cursor at end if at end of input

        if cursor_pos >= input_bytes.len() {

            let col = input_start_col + input_bytes.len();

            if col < cols {

                self.window.draw_cursor(fb, input_row as u32, col as u32);

            }

        }

    }



    /// Main event loop

    fn run(&mut self, fb: &Framebuffer) {

        log("Terminal: Entering main event loop");



        while self.running {

            // Poll for input

            let mut needs_render = false;


            // First try to receive IPC keyboard events from compositor
            if let Some(port) = self.event_port {
                let mut buffer = [0u8; 64];
                while let Ok(Some(size)) = atom_syscall::ipc::try_recv(port, &mut buffer) {
                    // Message format: MessageHeader (12 bytes) + KeyEvent (3 bytes) = 15 bytes minimum
                    const MIN_MESSAGE_SIZE: usize = 15;
                    const HEADER_SIZE: usize = 12;
                    
                    if size >= MIN_MESSAGE_SIZE {
                        // Parse key event from message payload
                        let scancode = buffer[HEADER_SIZE];
                        let _character = buffer[HEADER_SIZE + 1];
                        let _modifiers = buffer[HEADER_SIZE + 2];
                        
                        // Process scancode through input handler
                        if let Some(event) = self.input_handler.process_scancode(scancode) {
                            self.handle_key(event);
                            needs_render = true;
                        }
                    }
                }
            }

            // Fallback: also poll keyboard directly (for standalone mode)
            while let Some(event) = self.input_handler.poll() {

                self.handle_key(event);

                needs_render = true;

            }



            // Render if needed

            if needs_render {

                self.render(fb);

            }



            // Yield to scheduler

            yield_now();

        }



        log("Terminal: Exiting");

    }

}



/// Entry point

#[no_mangle]

pub extern "C" fn _start() -> ! {

    main()

}

/// EFI Entry Point
/// 
/// This function serves as the entry point when the binary is loaded as an UEFI application.
/// It delegates to the common main() function. The dual entry point design (_start and efi_main)
/// allows the binary to work both as a standalone executable and as an UEFI application.
#[no_mangle]
pub extern "efiapi" fn efi_main(
    _image_handle: *const core::ffi::c_void,
    _system_table: *const core::ffi::c_void,
) -> usize {
    main()
}



fn main() -> ! {

    log("Terminal: Starting userspace terminal");



    // Acquire framebuffer

    let fb = match Framebuffer::new() {

        Some(fb) => fb,

        None => {

            log("Terminal: Failed to acquire framebuffer");

            exit(1);

        }

    };



    log("Terminal: Framebuffer acquired");



    // Create and initialize terminal

    let mut terminal = Terminal::new();

    terminal.init(&fb);



    // Run main loop

    terminal.run(&fb);



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