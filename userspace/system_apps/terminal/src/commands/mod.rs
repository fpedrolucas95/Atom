// Commands Module
//
// Command registry and execution framework for all built-in terminal commands.

pub mod system;
pub mod process;
pub mod filesystem;
pub mod ping;
pub mod net;

use crate::buffer::DisplayBuffer;
use crate::ipc_client::IpcClient;
use crate::parser::ParsedCommand;
use crate::window::Theme;

/// Result of command execution
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandResult {
    Ok,
    NotFound,
    Error,
    Clear,
    Exit,
}

/// Command context passed to every command handler
pub struct CommandContext<'a> {
    pub display: &'a mut DisplayBuffer,
    pub ipc:     &'a IpcClient,
}

impl<'a> CommandContext<'a> {
    pub fn println(&mut self, text: &str) {
        self.display.writeln(text, Theme::TEXT_NORMAL);
    }
    pub fn println_colored(&mut self, text: &str, color: atom_syscall::graphics::Color) {
        self.display.writeln(text, color);
    }
    pub fn print(&mut self, text: &str) {
        self.display.write_str(text, Theme::TEXT_NORMAL);
    }
    pub fn error(&mut self, text: &str) {
        self.display.writeln(text, Theme::TEXT_ERROR);
    }
    pub fn success(&mut self, text: &str) {
        self.display.writeln(text, Theme::TEXT_SUCCESS);
    }
    pub fn info(&mut self, text: &str) {
        self.display.writeln(text, Theme::TEXT_INFO);
    }
    pub fn warning(&mut self, text: &str) {
        self.display.writeln(text, Theme::TEXT_WARNING);
    }
}

fn cmd_eq(input: &str, target: &str) -> bool {
    input.eq_ignore_ascii_case(target)
}

/// Execute a parsed command, return result code
pub fn execute(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let c = cmd.command;

    // ── System ──────────────────────────────────────────────────────────────
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
    if cmd_eq(c, "log") || cmd_eq(c, "dmesg") {
        return system::cmd_log(cmd, ctx);
    }
    if cmd_eq(c, "ports") {
        return system::cmd_ports(cmd, ctx);
    }
    if cmd_eq(c, "caps") {
        return system::cmd_caps(cmd, ctx);
    }
    if cmd_eq(c, "ping") {
        return ping::cmd_ping(cmd, ctx);
    }
    if cmd_eq(c, "ifconfig") || cmd_eq(c, "netinfo") {
        return net::cmd_ifconfig(cmd, ctx);
    }

    // ── Process management ──────────────────────────────────────────────────
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

    // ── Filesystem – navigation / read ─────────────────────────────────────
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
    if cmd_eq(c, "head") {
        return filesystem::cmd_head(cmd, ctx);
    }
    if cmd_eq(c, "tail") {
        return filesystem::cmd_tail(cmd, ctx);
    }
    if cmd_eq(c, "wc") {
        return filesystem::cmd_wc(cmd, ctx);
    }
    if cmd_eq(c, "tree") {
        return filesystem::cmd_tree(cmd, ctx);
    }
    if cmd_eq(c, "stat") {
        return filesystem::cmd_stat(cmd, ctx);
    }
    if cmd_eq(c, "find") {
        return filesystem::cmd_find(cmd, ctx);
    }

    // ── Filesystem – write / management ────────────────────────────────────
    if cmd_eq(c, "mkdir") {
        return filesystem::cmd_mkdir(cmd, ctx);
    }
    if cmd_eq(c, "rmdir") {
        return filesystem::cmd_rmdir(cmd, ctx);
    }
    if cmd_eq(c, "rm") {
        return filesystem::cmd_rm(cmd, ctx);
    }
    if cmd_eq(c, "mv") {
        return filesystem::cmd_mv(cmd, ctx);
    }
    if cmd_eq(c, "cp") {
        return filesystem::cmd_cp(cmd, ctx);
    }
    if cmd_eq(c, "touch") {
        return filesystem::cmd_touch(cmd, ctx);
    }
    if cmd_eq(c, "fsync") {
        return filesystem::cmd_fsync(cmd, ctx);
    }
    if cmd_eq(c, "chmod") {
        return filesystem::cmd_chmod(cmd, ctx);
    }
    if cmd_eq(c, "ln") {
        return filesystem::cmd_ln(cmd, ctx);
    }
    if cmd_eq(c, "df") {
        return filesystem::cmd_df(cmd, ctx);
    }
    if cmd_eq(c, "du") {
        return filesystem::cmd_du(cmd, ctx);
    }

    // ── Terminal control ────────────────────────────────────────────────────
    if cmd_eq(c, "exit") || cmd_eq(c, "quit") || cmd_eq(c, "logout") {
        return CommandResult::Exit;
    }

    ctx.error("Unknown command. Type 'help' for available commands.");
    CommandResult::NotFound
}

// ─── Help metadata ──────────────────────────────────────────────────────────

/// (usage, description) for a single command
pub fn get_command_help(c: &str) -> Option<(&'static str, &'static str)> {
    COMMANDS.iter()
        .find(|(name, _, _)| name.eq_ignore_ascii_case(c))
        .map(|(_, usage, desc)| (*usage, *desc))
}

/// Full command table: (name, usage, short-description)
pub fn get_all_commands() -> &'static [(&'static str, &'static str, &'static str)] {
    COMMANDS
}

static COMMANDS: &[(&str, &str, &str)] = &[
    // ── System ────────────────────────────────────────────────────────────
    ("help",     "help [command]",         "Show help (scroll up to read more)"),
    ("version",  "version",                "Display system version"),
    ("uptime",   "uptime",                 "Show system uptime"),
    ("date",     "date",                   "Show current date / time"),
    ("sysinfo",  "sysinfo",                "System info with ASCII logo"),
    ("clear",    "clear",                  "Clear the terminal screen"),
    ("echo",     "echo [text...]",         "Print text to terminal"),
    ("log",      "log",                    "Show system log (dmesg)"),
    // ── Process ───────────────────────────────────────────────────────────
    ("ps",       "ps",                     "List running processes"),
    ("kill",     "kill <pid>",             "Terminate a process"),
    ("exec",     "exec <program>",         "Launch a program"),
    ("mem",      "mem",                    "Memory usage statistics"),
    ("services", "services",               "List registered services"),
    // ── Filesystem – read ─────────────────────────────────────────────────
    ("ls",       "ls [-l] [-a] [path]",   "List directory contents"),
    ("cd",       "cd [path]",             "Change working directory"),
    ("pwd",      "pwd",                    "Print working directory"),
    ("cat",      "cat <file> [...]",      "Display file contents"),
    ("head",     "head [-n N] <file>",    "Show first N lines (default 10)"),
    ("tail",     "tail [-n N] <file>",    "Show last N lines (default 10)"),
    ("wc",       "wc [-l|-w|-c] <file>",  "Count lines / words / bytes"),
    ("stat",     "stat <file>",            "Show file metadata"),
    ("find",     "find [path] [-name pat]","Find files matching pattern"),
    ("tree",     "tree [-d N] [path]",    "Directory tree (depth N, default 3)"),
    // ── Filesystem – write ────────────────────────────────────────────────
    ("mkdir",    "mkdir [-p] <dir>",      "Create directory (-p: parents)"),
    ("rmdir",    "rmdir <dir>",            "Remove empty directory"),
    ("rm",       "rm [-r] [-f] <path>",   "Remove file (-r: recursive)"),
    ("mv",       "mv <src> <dst>",        "Move / rename file or directory"),
    ("cp",       "cp <src> <dst>",        "Copy file"),
    ("touch",    "touch <file>",           "Create file or update timestamp"),
    ("fsync",    "fsync <path>",           "Force file or directory metadata to disk"),
    ("chmod",    "chmod <mode> <path>",   "Change file permissions (octal)"),
    ("ln",       "ln [-s] <tgt> <link>",  "Create hard or symbolic link"),
    ("df",       "df",                     "Show disk free space"),
    ("du",       "du [-s] [-h] [path]",   "Show disk usage"),
    // ── Terminal ──────────────────────────────────────────────────────────
    ("exit",     "exit",                   "Exit the terminal"),
    ("ports",    "ports",                  "List IPC ports"),
    ("caps",     "caps",                   "List process capabilities"),
    ("ping",     "ping <host> [options]",  "Send ICMP ECHO_REQUEST to network hosts"),
    ("ifconfig", "ifconfig",               "Display network interface configuration"),
];
