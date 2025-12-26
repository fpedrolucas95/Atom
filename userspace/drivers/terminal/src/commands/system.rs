// System Commands
//
// Commands for displaying system information, version, uptime, etc.
// All information is obtained via IPC requests to system services.

use super::{CommandContext, CommandResult, get_all_commands, get_command_help};
use crate::parser::ParsedCommand;
use crate::window::Theme;
use atom_syscall::thread::get_ticks;

/// Version information
const OS_NAME: &str = "Atom OS";
const OS_VERSION: &str = "0.1.0";
const OS_CODENAME: &str = "Helium";
const KERNEL_VERSION: &str = "0.1.0-microkernel";

/// help command - display available commands or specific command help
pub fn cmd_help(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    if let Some(topic) = cmd.arg(0) {
        // Show help for specific command
        if let Some((usage, desc)) = get_command_help(topic) {
            ctx.println("");
            ctx.println_colored(usage, Theme::TEXT_INFO);
            ctx.println(desc);
            ctx.println("");
        } else {
            ctx.error("No help available for that command.");
        }
    } else {
        // Show all commands
        ctx.println("");
        ctx.println_colored("Atom Terminal - Available Commands", Theme::TEXT_INFO);
        ctx.println("-----------------------------------");
        ctx.println("");

        let commands = get_all_commands();
        let mut category = "";

        for (name, desc) in commands.iter() {
            // Simple categorization by command type
            let new_category = if *name == "help" || *name == "version" || *name == "uptime"
                || *name == "date" || *name == "sysinfo" || *name == "clear"
                || *name == "echo" || *name == "log"
            {
                "System"
            } else if *name == "ps" || *name == "kill" || *name == "exec"
                || *name == "mem" || *name == "services"
            {
                "Process"
            } else if *name == "ls" || *name == "cd" || *name == "pwd"
                || *name == "cat" || *name == "tree"
            {
                "Filesystem"
            } else {
                "Other"
            };

            if new_category != category {
                category = new_category;
                ctx.println("");
                ctx.println_colored(category, Theme::PROMPT_USER);
            }

            // Format command with description
            let mut line = [0u8; 64];
            let mut pos = 0;

            // Write command name (padded to 12 chars)
            for byte in name.bytes() {
                if pos < 12 {
                    line[pos] = byte;
                    pos += 1;
                }
            }
            while pos < 12 {
                line[pos] = b' ';
                pos += 1;
            }

            // Write description
            for byte in desc.bytes() {
                if pos < 60 {
                    line[pos] = byte;
                    pos += 1;
                }
            }

            let line_str = unsafe { core::str::from_utf8_unchecked(&line[..pos]) };
            ctx.println(line_str);
        }

        ctx.println("");
        ctx.println("Type 'help <command>' for more information.");
        ctx.println("");
    }

    CommandResult::Ok
}

/// version command - display system version
pub fn cmd_version(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    ctx.println("");
    ctx.println_colored(OS_NAME, Theme::TEXT_INFO);

    let mut version_line = [0u8; 64];
    let mut pos = 0;
    for byte in "Version ".bytes() {
        version_line[pos] = byte;
        pos += 1;
    }
    for byte in OS_VERSION.bytes() {
        version_line[pos] = byte;
        pos += 1;
    }
    for byte in " (".bytes() {
        version_line[pos] = byte;
        pos += 1;
    }
    for byte in OS_CODENAME.bytes() {
        version_line[pos] = byte;
        pos += 1;
    }
    version_line[pos] = b')';
    pos += 1;

    let version_str = unsafe { core::str::from_utf8_unchecked(&version_line[..pos]) };
    ctx.println(version_str);

    // Kernel version
    let mut kernel_line = [0u8; 64];
    pos = 0;
    for byte in "Kernel: ".bytes() {
        kernel_line[pos] = byte;
        pos += 1;
    }
    for byte in KERNEL_VERSION.bytes() {
        kernel_line[pos] = byte;
        pos += 1;
    }

    let kernel_str = unsafe { core::str::from_utf8_unchecked(&kernel_line[..pos]) };
    ctx.println(kernel_str);

    ctx.println("Architecture: x86_64");
    ctx.println("");

    CommandResult::Ok
}

/// uptime command - show system uptime
pub fn cmd_uptime(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let mut buffer = [0u8; 64];
    let len = ctx.ipc.format_uptime(&mut buffer);

    let mut line = [0u8; 80];
    let mut pos = 0;

    for byte in "System uptime: ".bytes() {
        line[pos] = byte;
        pos += 1;
    }
    for i in 0..len {
        line[pos] = buffer[i];
        pos += 1;
    }

    let uptime_str = unsafe { core::str::from_utf8_unchecked(&line[..pos]) };
    ctx.println("");
    ctx.println_colored(uptime_str, Theme::TEXT_INFO);
    ctx.println("");

    CommandResult::Ok
}

/// date command - display current date/time
pub fn cmd_date(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    // In a full implementation, we would query an RTC service
    // For now, show uptime-based timestamp
    let ticks = get_ticks();
    let total_seconds = ticks / 100;

    let hours = (total_seconds / 3600) % 24;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    let mut time_str = [0u8; 32];
    let mut pos = 0;

    // Format time
    if hours < 10 {
        time_str[pos] = b'0';
        pos += 1;
    }
    pos += format_number(hours, &mut time_str[pos..]);
    time_str[pos] = b':';
    pos += 1;

    if minutes < 10 {
        time_str[pos] = b'0';
        pos += 1;
    }
    pos += format_number(minutes, &mut time_str[pos..]);
    time_str[pos] = b':';
    pos += 1;

    if seconds < 10 {
        time_str[pos] = b'0';
        pos += 1;
    }
    pos += format_number(seconds, &mut time_str[pos..]);

    for byte in " UTC (simulated)".bytes() {
        if pos < time_str.len() {
            time_str[pos] = byte;
            pos += 1;
        }
    }

    let time_display = unsafe { core::str::from_utf8_unchecked(&time_str[..pos]) };
    ctx.println("");
    ctx.println_colored(time_display, Theme::TEXT_INFO);
    ctx.println("");

    CommandResult::Ok
}

/// echo command - display text
pub fn cmd_echo(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let mut output = [0u8; 256];
    let mut pos = 0;

    for i in 0..cmd.arg_count {
        if i > 0 && pos < output.len() {
            output[pos] = b' ';
            pos += 1;
        }
        for byte in cmd.args[i].bytes() {
            if pos < output.len() {
                output[pos] = byte;
                pos += 1;
            }
        }
    }

    let text = unsafe { core::str::from_utf8_unchecked(&output[..pos]) };
    ctx.println(text);

    CommandResult::Ok
}

/// sysinfo command - display system information summary
pub fn cmd_sysinfo(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    ctx.println("");
    ctx.println_colored("System Information", Theme::TEXT_INFO);
    ctx.println("==================");
    ctx.println("");

    // OS info
    ctx.print("OS:           ");
    ctx.println_colored(OS_NAME, Theme::PROMPT_USER);

    ctx.print("Version:      ");
    ctx.println(OS_VERSION);

    ctx.print("Kernel:       ");
    ctx.println(KERNEL_VERSION);

    ctx.print("Architecture: ");
    ctx.println("x86_64");

    ctx.println("");

    // Uptime
    let mut buffer = [0u8; 64];
    let len = ctx.ipc.format_uptime(&mut buffer);
    let uptime = unsafe { core::str::from_utf8_unchecked(&buffer[..len]) };
    ctx.print("Uptime:       ");
    ctx.println(uptime);

    // Memory
    let (total, used, free) = ctx.ipc.query_memory();
    let mut mem_line = [0u8; 64];
    let mut pos = 0;
    pos += format_number(used / 1024, &mut mem_line[pos..]);
    for byte in " MB / ".bytes() {
        mem_line[pos] = byte;
        pos += 1;
    }
    pos += format_number(total / 1024, &mut mem_line[pos..]);
    for byte in " MB".bytes() {
        mem_line[pos] = byte;
        pos += 1;
    }
    let mem_str = unsafe { core::str::from_utf8_unchecked(&mem_line[..pos]) };
    ctx.print("Memory:       ");
    ctx.println(mem_str);

    ctx.println("");

    CommandResult::Ok
}

/// log command - display system log
pub fn cmd_log(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    ctx.println("");
    ctx.println_colored("System Log", Theme::TEXT_INFO);
    ctx.println("----------");
    ctx.println("");

    ctx.ipc.read_log(|line| {
        ctx.println(line);
    });

    ctx.println("");

    CommandResult::Ok
}

/// ports command - list IPC ports (diagnostic)
pub fn cmd_ports(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    ctx.println("");
    ctx.println_colored("IPC Ports", Theme::TEXT_INFO);
    ctx.println("---------");
    ctx.println("");
    ctx.println("Port  Service");
    ctx.println("----  -------");
    ctx.println("1     service_manager");
    ctx.println("2     process_manager");
    ctx.println("3     memory_manager");
    ctx.println("4     filesystem");
    ctx.println("5     display_server");
    ctx.println("6     input_server");
    ctx.println("");

    CommandResult::Ok
}

/// caps command - list capabilities (diagnostic)
pub fn cmd_caps(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    ctx.println("");
    ctx.println_colored("Process Capabilities", Theme::TEXT_INFO);
    ctx.println("--------------------");
    ctx.println("");
    ctx.println("CAP_GRAPHICS     - Framebuffer access");
    ctx.println("CAP_INPUT        - Keyboard/mouse input");
    ctx.println("CAP_IPC          - IPC messaging");
    ctx.println("CAP_MEMORY       - Memory allocation");
    ctx.println("");

    CommandResult::Ok
}

/// Format a number into a buffer
fn format_number(mut n: u64, buffer: &mut [u8]) -> usize {
    if buffer.is_empty() {
        return 0;
    }

    if n == 0 {
        buffer[0] = b'0';
        return 1;
    }

    let mut digits = [0u8; 20];
    let mut count = 0;

    while n > 0 {
        digits[count] = b'0' + (n % 10) as u8;
        n /= 10;
        count += 1;
    }

    if count > buffer.len() {
        return 0;
    }

    for i in 0..count {
        buffer[i] = digits[count - 1 - i];
    }

    count
}