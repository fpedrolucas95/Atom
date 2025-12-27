// Commands Module
//
// This module provides the command registry and execution framework
// for all built-in terminal commands.

pub mod system;
pub mod process;
pub mod filesystem;

use crate::buffer::DisplayBuffer;
use crate::ipc_client::IpcClient;
use crate::parser::ParsedCommand;
use crate::window::Theme;

/// Result of command execution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandResult {
    /// Command executed successfully
    Ok,
    /// Command not found
    NotFound,
    /// Command failed with error message
    Error,
    /// Request to clear the screen
    Clear,
    /// Request to exit the terminal
    Exit,
}

/// Command context containing resources needed by commands
pub struct CommandContext<'a> {
    pub display: &'a mut DisplayBuffer,
    pub ipc: &'a IpcClient,
}

impl<'a> CommandContext<'a> {
    /// Print a line to the display
    pub fn println(&mut self, text: &str) {
        self.display.writeln(text, Theme::TEXT_NORMAL);
    }

    /// Print with specific color
    pub fn println_colored(&mut self, text: &str, color: atom_syscall::graphics::Color) {
        self.display.writeln(text, color);
    }

    /// Print without newline
    pub fn print(&mut self, text: &str) {
        self.display.write_str(text, Theme::TEXT_NORMAL);
    }

    /// Print error message
    pub fn error(&mut self, text: &str) {
        self.display.writeln(text, Theme::TEXT_ERROR);
    }

    /// Print success message
    pub fn success(&mut self, text: &str) {
        self.display.writeln(text, Theme::TEXT_SUCCESS);
    }

    /// Print info message
    pub fn info(&mut self, text: &str) {
        self.display.writeln(text, Theme::TEXT_INFO);
    }

    /// Print warning message
    pub fn warning(&mut self, text: &str) {
        self.display.writeln(text, Theme::TEXT_WARNING);
    }
}

/// Helper for case-insensitive command matching
fn cmd_eq(input: &str, target: &str) -> bool {
    input.eq_ignore_ascii_case(target)
}

/// Execute a parsed command
pub fn execute(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let c = cmd.command;

    // System information commands
    if cmd_eq(c, "help") || cmd_eq(c, "?") {
        return system::cmd_help(cmd, ctx);
    }
    if cmd_eq(c, "version") || cmd_eq(c, "ver") {
        return system::cmd_version(cmd, ctx);
    }
    if cmd_eq(c, "uptime") {
        return system::cmd_uptime(cmd, ctx);
    }
    if cmd_eq(c, "date") || cmd_eq(c, "time") {
        return system::cmd_date(cmd, ctx);
    }
    if cmd_eq(c, "clear") || cmd_eq(c, "cls") {
        return CommandResult::Clear;
    }
    if cmd_eq(c, "echo") {
        return system::cmd_echo(cmd, ctx);
    }
    if cmd_eq(c, "sysinfo") {
        return system::cmd_sysinfo(cmd, ctx);
    }

    // Process management commands
    if cmd_eq(c, "ps") || cmd_eq(c, "procs") {
        return process::cmd_ps(cmd, ctx);
    }
    if cmd_eq(c, "kill") {
        return process::cmd_kill(cmd, ctx);
    }
    if cmd_eq(c, "exec") || cmd_eq(c, "run") {
        return process::cmd_exec(cmd, ctx);
    }
    if cmd_eq(c, "mem") || cmd_eq(c, "memory") {
        return process::cmd_memory(cmd, ctx);
    }
    if cmd_eq(c, "services") || cmd_eq(c, "svc") {
        return process::cmd_services(cmd, ctx);
    }

    // Filesystem commands
    if cmd_eq(c, "ls") || cmd_eq(c, "dir") {
        return filesystem::cmd_ls(cmd, ctx);
    }
    if cmd_eq(c, "cd") {
        return filesystem::cmd_cd(cmd, ctx);
    }
    if cmd_eq(c, "pwd") {
        return filesystem::cmd_pwd(cmd, ctx);
    }
    if cmd_eq(c, "cat") || cmd_eq(c, "type") {
        return filesystem::cmd_cat(cmd, ctx);
    }
    if cmd_eq(c, "tree") {
        return filesystem::cmd_tree(cmd, ctx);
    }

    // Terminal control
    if cmd_eq(c, "exit") || cmd_eq(c, "quit") || cmd_eq(c, "logout") {
        return CommandResult::Exit;
    }

    // Debug/diagnostic commands
    if cmd_eq(c, "log") || cmd_eq(c, "dmesg") {
        return system::cmd_log(cmd, ctx);
    }
    if cmd_eq(c, "ports") {
        return system::cmd_ports(cmd, ctx);
    }
    if cmd_eq(c, "caps") {
        return system::cmd_caps(cmd, ctx);
    }

    // Unknown command
    ctx.error("Unknown command. Type 'help' for available commands.");
    CommandResult::NotFound
}

/// Get command description for help text
pub fn get_command_help(c: &str) -> Option<(&'static str, &'static str)> {
    if cmd_eq(c, "help") || cmd_eq(c, "?") {
        return Some(("help [command]", "Display help information"));
    }
    if cmd_eq(c, "version") || cmd_eq(c, "ver") {
        return Some(("version", "Display system version information"));
    }
    if cmd_eq(c, "uptime") {
        return Some(("uptime", "Show system uptime"));
    }
    if cmd_eq(c, "date") || cmd_eq(c, "time") {
        return Some(("date", "Display current date and time"));
    }
    if cmd_eq(c, "clear") || cmd_eq(c, "cls") {
        return Some(("clear", "Clear the terminal screen"));
    }
    if cmd_eq(c, "echo") {
        return Some(("echo [text...]", "Display text"));
    }
    if cmd_eq(c, "sysinfo") {
        return Some(("sysinfo", "Display system information summary"));
    }
    if cmd_eq(c, "ps") || cmd_eq(c, "procs") {
        return Some(("ps", "List running processes"));
    }
    if cmd_eq(c, "kill") {
        return Some(("kill <pid>", "Terminate a process"));
    }
    if cmd_eq(c, "exec") || cmd_eq(c, "run") {
        return Some(("exec <program>", "Execute a program"));
    }
    if cmd_eq(c, "mem") || cmd_eq(c, "memory") {
        return Some(("mem", "Display memory usage"));
    }
    if cmd_eq(c, "services") || cmd_eq(c, "svc") {
        return Some(("services", "List registered services"));
    }
    if cmd_eq(c, "ls") || cmd_eq(c, "dir") {
        return Some(("ls [path]", "List directory contents"));
    }
    if cmd_eq(c, "cd") {
        return Some(("cd <path>", "Change current directory"));
    }
    if cmd_eq(c, "pwd") {
        return Some(("pwd", "Print working directory"));
    }
    if cmd_eq(c, "cat") || cmd_eq(c, "type") {
        return Some(("cat <file>", "Display file contents"));
    }
    if cmd_eq(c, "tree") {
        return Some(("tree [path]", "Display directory tree"));
    }
    if cmd_eq(c, "exit") || cmd_eq(c, "quit") {
        return Some(("exit", "Exit the terminal"));
    }
    if cmd_eq(c, "log") || cmd_eq(c, "dmesg") {
        return Some(("log", "Display system log"));
    }
    if cmd_eq(c, "ports") {
        return Some(("ports", "List IPC ports"));
    }
    if cmd_eq(c, "caps") {
        return Some(("caps", "List capabilities"));
    }
    None
}

/// Get all available commands for help display
pub fn get_all_commands() -> &'static [(&'static str, &'static str)] {
    &[
        // System
        ("help", "Display help information"),
        ("version", "Show system version"),
        ("uptime", "Show system uptime"),
        ("date", "Show current date/time"),
        ("sysinfo", "System information summary"),
        ("clear", "Clear terminal screen"),
        ("echo", "Display text"),
        ("log", "Display system log"),
        // Process
        ("ps", "List processes"),
        ("kill", "Terminate a process"),
        ("exec", "Execute a program"),
        ("mem", "Memory usage"),
        ("services", "List services"),
        // Filesystem
        ("ls", "List directory"),
        ("cd", "Change directory"),
        ("pwd", "Print working directory"),
        ("cat", "Display file contents"),
        ("tree", "Directory tree"),
        // Terminal
        ("exit", "Exit terminal"),
    ]
}