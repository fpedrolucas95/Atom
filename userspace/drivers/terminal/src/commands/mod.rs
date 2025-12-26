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

/// Execute a parsed command
pub fn execute(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    match cmd.command.to_ascii_lowercase().as_str() {
        // System information commands
        "help" | "?" => system::cmd_help(cmd, ctx),
        "version" | "ver" => system::cmd_version(cmd, ctx),
        "uptime" => system::cmd_uptime(cmd, ctx),
        "date" | "time" => system::cmd_date(cmd, ctx),
        "clear" | "cls" => CommandResult::Clear,
        "echo" => system::cmd_echo(cmd, ctx),
        "sysinfo" => system::cmd_sysinfo(cmd, ctx),

        // Process management commands
        "ps" | "procs" => process::cmd_ps(cmd, ctx),
        "kill" => process::cmd_kill(cmd, ctx),
        "exec" | "run" => process::cmd_exec(cmd, ctx),
        "mem" | "memory" => process::cmd_memory(cmd, ctx),
        "services" | "svc" => process::cmd_services(cmd, ctx),

        // Filesystem commands
        "ls" | "dir" => filesystem::cmd_ls(cmd, ctx),
        "cd" => filesystem::cmd_cd(cmd, ctx),
        "pwd" => filesystem::cmd_pwd(cmd, ctx),
        "cat" | "type" => filesystem::cmd_cat(cmd, ctx),
        "tree" => filesystem::cmd_tree(cmd, ctx),

        // Terminal control
        "exit" | "quit" | "logout" => CommandResult::Exit,

        // Debug/diagnostic commands
        "log" | "dmesg" => system::cmd_log(cmd, ctx),
        "ports" => system::cmd_ports(cmd, ctx),
        "caps" => system::cmd_caps(cmd, ctx),

        // Unknown command
        _ => {
            ctx.error("Unknown command. Type 'help' for available commands.");
            CommandResult::NotFound
        }
    }
}

/// Get command description for help text
pub fn get_command_help(cmd: &str) -> Option<(&'static str, &'static str)> {
    match cmd.to_ascii_lowercase().as_str() {
        "help" | "?" => Some(("help [command]", "Display help information")),
        "version" | "ver" => Some(("version", "Display system version information")),
        "uptime" => Some(("uptime", "Show system uptime")),
        "date" | "time" => Some(("date", "Display current date and time")),
        "clear" | "cls" => Some(("clear", "Clear the terminal screen")),
        "echo" => Some(("echo [text...]", "Display text")),
        "sysinfo" => Some(("sysinfo", "Display system information summary")),
        "ps" | "procs" => Some(("ps", "List running processes")),
        "kill" => Some(("kill <pid>", "Terminate a process")),
        "exec" | "run" => Some(("exec <program>", "Execute a program")),
        "mem" | "memory" => Some(("mem", "Display memory usage")),
        "services" | "svc" => Some(("services", "List registered services")),
        "ls" | "dir" => Some(("ls [path]", "List directory contents")),
        "cd" => Some(("cd <path>", "Change current directory")),
        "pwd" => Some(("pwd", "Print working directory")),
        "cat" | "type" => Some(("cat <file>", "Display file contents")),
        "tree" => Some(("tree [path]", "Display directory tree")),
        "exit" | "quit" => Some(("exit", "Exit the terminal")),
        "log" | "dmesg" => Some(("log", "Display system log")),
        "ports" => Some(("ports", "List IPC ports")),
        "caps" => Some(("caps", "List capabilities")),
        _ => None,
    }
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