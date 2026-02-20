#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;
use core::panic::PanicInfo;
use core::arch::asm;

mod error;
mod fs;

use error::{FilManagerError, Result};
use fs::{File, Dir, FsOps, FsQuery, FileMode};

// ============================================================================
// Memory Allocator
// ============================================================================

mod allocator {
    use core::alloc::{GlobalAlloc, Layout};
    use core::cell::UnsafeCell;
    use core::ptr::null_mut;

    const HEAP_SIZE: usize = 1024 * 1024; // 1 MB heap for fileman

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
            let next_ptr = HEAP.next.get();
            let heap_start = HEAP.data.get() as *mut u8;

            let align = layout.align();
            let size = layout.size();
            let current = *next_ptr;
            let aligned = (current + align - 1) & !(align - 1);

            if aligned + size > HEAP_SIZE {
                return null_mut();
            }

            *next_ptr = aligned + size;
            heap_start.add(aligned)
        }

        unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
            // Bump allocator doesn't free
        }
    }

    #[global_allocator]
    static ALLOCATOR: BumpAllocator = BumpAllocator;
}

// ============================================================================
// Panic Handler
// ============================================================================

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        unsafe {
            asm!("hlt");
        }
    }
}

#[no_mangle]
pub extern "C" fn _start() -> ! {
    main();
    loop {
        unsafe {
            asm!("hlt");
        }
    }
}

// ============================================================================
// I/O Helpers
// ============================================================================

fn print(s: &str) {
    atom_syscall::debug::log(s);
}

fn println(s: &str) {
    let mut buf = alloc::string::String::new();
    use core::fmt::Write;
    let _ = writeln!(buf, "{}", s);
    print(&buf);
}

fn print_error(msg: &str) {
    let mut buf = String::new();
    use core::fmt::Write;
    let _ = writeln!(buf, "error: {}", msg);
    print(&buf);
}

/// Read a line from stdin (simplified: reads until newline)
fn read_line() -> String {
    // For now, we'll use a stub that returns empty line
    // In production, this would read from a proper input driver
    String::new()
}

// ============================================================================
// Command Handlers
// ============================================================================

struct CommandContext {
    cwd: String,
}

impl CommandContext {
    fn new() -> Self {
        // Try to set starting directory, fall back to /
        let cwd = String::from("/");
        CommandContext { cwd }
    }

    /// Resolve path relative to current working directory
    fn resolve_path(&self, path: &str) -> Result<String> {
        if path.is_empty() {
            return Ok(self.cwd.clone());
        }

        let resolved = if path.starts_with('/') {
            // Absolute path
            error::normalize_path(path)?
        } else {
            // Relative path
            let full = if self.cwd.ends_with('/') {
                format!("{}{}", self.cwd, path)
            } else {
                format!("{}/{}", self.cwd, path)
            };
            error::normalize_path(&full)?
        };

        Ok(resolved)
    }

    /// Change working directory
    fn cmd_cd(&mut self, args: &[&str]) -> Result<()> {
        if args.is_empty() {
            // cd without args goes to root
            self.cwd = String::from("/");
            return Ok(());
        }

        if args.len() > 1 {
            return Err(FilManagerError::InvalidCommand(
                error::CommandError::WrongArgCount {
                    expected: 1,
                    got: args.len(),
                },
            ));
        }

        let target = self.resolve_path(args[0])?;

        // Verify target is a directory
        if !FsQuery::is_dir(&target)? {
            return Err(FilManagerError::fs(
                atom_syscall::fs::FsError::NotDir,
                "cd",
            ));
        }

        self.cwd = target;
        Ok(())
    }

    /// Print working directory
    fn cmd_pwd(&self, _args: &[&str]) -> Result<()> {
        println(&self.cwd);
        Ok(())
    }

    /// List directory contents
    fn cmd_ls(&self, args: &[&str]) -> Result<()> {
        let path = if args.is_empty() {
            self.cwd.clone()
        } else if args.len() == 1 {
            self.resolve_path(args[0])?
        } else {
            return Err(FilManagerError::InvalidCommand(
                error::CommandError::WrongArgCount {
                    expected: 1,
                    got: args.len(),
                },
            ));
        };

        let dir = Dir::open(&path)?;
        let entries = dir.list()?;

        if entries.is_empty() {
            println("");
            return Ok(());
        }

        for entry in entries {
            let output = if entry.is_dir() {
                format!("{}/", entry.name)
            } else {
                format!("{} ({})", entry.name, entry.size_string())
            };
            println(&output);
        }

        Ok(())
    }

    /// Create directory
    fn cmd_mkdir(&self, args: &[&str]) -> Result<()> {
        if args.is_empty() {
            return Err(FilManagerError::InvalidCommand(
                error::CommandError::MissingArg("path"),
            ));
        }

        if args.len() > 1 {
            return Err(FilManagerError::InvalidCommand(
                error::CommandError::WrongArgCount {
                    expected: 1,
                    got: args.len(),
                },
            ));
        }

        let path = self.resolve_path(args[0])?;
        FsOps::mkdir(&path)?;
        
        let msg = format!("mkdir: created directory '{}'", args[0]);
        println(&msg);
        Ok(())
    }

    /// Remove file
    fn cmd_rm(&self, args: &[&str]) -> Result<()> {
        if args.is_empty() {
            return Err(FilManagerError::InvalidCommand(
                error::CommandError::MissingArg("file"),
            ));
        }

        let mut recursive = false;
        
        // Parse options
        let mut i = 0;
        while i < args.len() && args[i].starts_with('-') {
            match args[i] {
                "-r" | "-R" | "--recursive" => recursive = true,
                _ => {
                    let msg = format!("rm: unknown option '{}'", args[i]);
                    print_error(&msg);
                    return Err(FilManagerError::InvalidCommand(
                        error::CommandError::UnknownCommand(args[i].len()),
                    ));
                }
            }
            i += 1;
        }

        if i >= args.len() {
            return Err(FilManagerError::InvalidCommand(
                error::CommandError::MissingArg("file"),
            ));
        }

        let path = self.resolve_path(args[i])?;

        if recursive {
            FsOps::rm_recursive(&path)?;
            let msg = format!("rm: removed '{}' and its contents", args[i]);
            println(&msg);
        } else {
            // Try to remove as file first
            match FsOps::unlink(&path) {
                Ok(()) => {
                    let msg = format!("rm: removed '{}'", args[i]);
                    println(&msg);
                }
                Err(FilManagerError::FsOp(atom_syscall::fs::FsError::IsDir, _)) => {
                    return Err(FilManagerError::FsOp(
                        atom_syscall::fs::FsError::IsDir,
                        "rm",
                    ));
                }
                Err(e) => return Err(e),
            }
        }

        Ok(())
    }

    /// Move/rename file or directory
    fn cmd_mv(&self, args: &[&str]) -> Result<()> {
        if args.len() < 2 {
            return Err(FilManagerError::InvalidCommand(
                error::CommandError::WrongArgCount {
                    expected: 2,
                    got: args.len(),
                },
            ));
        }

        if args.len() > 2 {
            return Err(FilManagerError::InvalidCommand(
                error::CommandError::WrongArgCount {
                    expected: 2,
                    got: args.len(),
                },
            ));
        }

        let src = self.resolve_path(args[0])?;
        let dst = self.resolve_path(args[1])?;

        FsOps::rename(&src, &dst)?;
        
        let msg = format!("mv: moved '{}' to '{}'", args[0], args[1]);
        println(&msg);
        Ok(())
    }

    /// Copy file
    fn cmd_cp(&self, args: &[&str]) -> Result<()> {
        if args.len() < 2 {
            return Err(FilManagerError::InvalidCommand(
                error::CommandError::WrongArgCount {
                    expected: 2,
                    got: args.len(),
                },
            ));
        }

        if args.len() > 2 {
            return Err(FilManagerError::InvalidCommand(
                error::CommandError::WrongArgCount {
                    expected: 2,
                    got: args.len(),
                },
            ));
        }

        let src = self.resolve_path(args[0])?;
        let dst = self.resolve_path(args[1])?;

        FsOps::copy(&src, &dst)?;
        
        let msg = format!("cp: copied '{}' to '{}'", args[0], args[1]);
        println(&msg);
        Ok(())
    }

    /// Display file contents
    fn cmd_cat(&self, args: &[&str]) -> Result<()> {
        if args.is_empty() {
            return Err(FilManagerError::InvalidCommand(
                error::CommandError::MissingArg("file"),
            ));
        }

        if args.len() > 1 {
            return Err(FilManagerError::InvalidCommand(
                error::CommandError::WrongArgCount {
                    expected: 1,
                    got: args.len(),
                },
            ));
        }

        let path = self.resolve_path(args[0])?;
        let mut file = File::open(&path, FileMode::ReadOnly)?;
        let content = file.read_all()?;

        // Convert to string for display (assuming UTF-8)
        match core::str::from_utf8(&content) {
            Ok(text) => {
                print(text);
            }
            Err(_) => {
                println("[binary file - cannot display]");
            }
        }

        Ok(())
    }

    /// Show help
    fn cmd_help(&self, _args: &[&str]) -> Result<()> {
        let help_text = r#"Fileman - Simple File Manager for Atom OS

Commands:
  pwd              - Print working directory
  cd [path]        - Change directory (default: /)
  ls [path]        - List directory contents
  cat <file>       - Display file contents
  mkdir <path>     - Create directory
  rm [-r] <path>   - Remove file (or directory with -r)
  mv <src> <dst>   - Move/rename file or directory
  cp <src> <dst>   - Copy file
  help             - Show this help message
  exit             - Exit fileman

Options:
  -r, -R           - Recursive (for rm)

All paths can be absolute (/path) or relative.
"#;
        println(help_text);
        Ok(())
    }

    /// Exit application
    fn should_exit(&self, _args: &[&str]) -> bool {
        true
    }
}

// ============================================================================
// Main CLI Loop
// ============================================================================

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let ctx = CommandContext::new();

    println("=================================");
    println("Fileman - File Manager for Atom OS");
    println("Type 'help' for available commands");
    println("=================================");

    // For now, print initial prompt - in production this would be an interactive loop
    let prompt = format!("fileman:{}> ", ctx.cwd);
    println(&prompt);

    0
}

// ============================================================================
// Testing/Development Entry Point
// ============================================================================

/// Internal test helper - can be called from other modules
#[allow(dead_code)]
fn execute_command(ctx: &mut CommandContext, cmd: &str) -> Result<bool> {
    if cmd.trim().is_empty() {
        return Ok(false);
    }

    // Parse command and arguments
    let parts: Vec<&str> = cmd.trim().split_whitespace().collect();
    if parts.is_empty() {
        return Ok(false);
    }

    let cmd_name = parts[0];
    let args = &parts[1..];

    let should_exit = match cmd_name {
        "pwd" => { ctx.cmd_pwd(args)?; false },
        "cd" => { ctx.cmd_cd(args)?; false },
        "ls" => { ctx.cmd_ls(args)?; false },
        "cat" => { ctx.cmd_cat(args)?; false },
        "mkdir" => { ctx.cmd_mkdir(args)?; false },
        "rm" => { ctx.cmd_rm(args)?; false },
        "mv" => { ctx.cmd_mv(args)?; false },
        "cp" => { ctx.cmd_cp(args)?; false },
        "help" => { ctx.cmd_help(args)?; false },
        "exit" => ctx.should_exit(args),
        _ => {
            let msg = format!("fileman: unknown command '{}'", cmd_name);
            print_error(&msg);
            return Err(FilManagerError::InvalidCommand(
                error::CommandError::UnknownCommand(cmd_name.len()),
            ));
        }
    };

    Ok(should_exit)
}
