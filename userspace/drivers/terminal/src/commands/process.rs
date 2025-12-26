// Process Commands

//

// Commands for process management: listing, killing, spawning processes.

// All process information is obtained via IPC to the process manager service.



use super::{CommandContext, CommandResult};

use crate::parser::{ParsedCommand, parse_number};

use crate::window::Theme;



/// ps command - list running processes

pub fn cmd_ps(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {

    ctx.println("");

    ctx.println_colored("Running Processes", Theme::TEXT_INFO);

    ctx.println("-----------------");

    ctx.println("");

    ctx.println("PID   NAME             STATE");

    ctx.println("---   ----             -----");



    ctx.ipc.query_processes(|pid, name, state| {

        let mut line = [0u8; 64];

        let mut pos = 0;



        // Format PID (right-aligned in 5 chars)

        let mut pid_str = [0u8; 8];

        let pid_len = format_number(pid, &mut pid_str);



        // Pad PID to 6 chars

        for _ in 0..(5 - pid_len.min(5)) {

            line[pos] = b' ';

            pos += 1;

        }

        for i in 0..pid_len.min(5) {

            line[pos] = pid_str[i];

            pos += 1;

        }

        line[pos] = b' ';

        pos += 1;



        // Name (16 chars)

        for byte in name.bytes() {

            if pos < 22 {

                line[pos] = byte;

                pos += 1;

            }

        }

        while pos < 22 {

            line[pos] = b' ';

            pos += 1;

        }



        // State

        for byte in state.bytes() {

            if pos < 60 {

                line[pos] = byte;

                pos += 1;

            }

        }



        let line_str = unsafe { core::str::from_utf8_unchecked(&line[..pos]) };

        ctx.println(line_str);

    });



    ctx.println("");



    CommandResult::Ok

}



/// kill command - terminate a process

pub fn cmd_kill(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {

    let pid_arg = match cmd.arg(0) {

        Some(arg) => arg,

        None => {

            ctx.error("Usage: kill <pid>");

            return CommandResult::Error;

        }

    };



    let pid = match parse_number(pid_arg) {

        Some(n) => n,

        None => {

            ctx.error("Invalid PID");

            return CommandResult::Error;

        }

    };



    // Check for protected system processes

    if pid < 10 {

        ctx.warning("Cannot terminate system process");

        return CommandResult::Error;

    }



    if ctx.ipc.kill_process(pid) {

        let mut msg = [0u8; 64];

        let mut pos = 0;

        for byte in "Signal sent to process ".bytes() {

            msg[pos] = byte;

            pos += 1;

        }

        pos += format_number(pid, &mut msg[pos..]);



        let msg_str = unsafe { core::str::from_utf8_unchecked(&msg[..pos]) };

        ctx.success(msg_str);

    } else {

        ctx.error("Failed to send signal to process");

        return CommandResult::Error;

    }



    CommandResult::Ok

}



/// exec command - execute a program

pub fn cmd_exec(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {

    let program = match cmd.arg(0) {

        Some(arg) => arg,

        None => {

            ctx.error("Usage: exec <program> [args...]");

            return CommandResult::Error;

        }

    };



    ctx.info("Attempting to execute program...");



    // Collect arguments

    let args: [&str; 16] = {

        let mut arr = [""; 16];

        for i in 1..cmd.arg_count.min(16) {

            arr[i - 1] = cmd.args[i];

        }

        arr

    };



    match ctx.ipc.spawn_process(program, &args[..cmd.arg_count.saturating_sub(1)]) {

        Some(pid) => {

            let mut msg = [0u8; 64];

            let mut pos = 0;

            for byte in "Started process with PID ".bytes() {

                msg[pos] = byte;

                pos += 1;

            }

            pos += format_number(pid, &mut msg[pos..]);



            let msg_str = unsafe { core::str::from_utf8_unchecked(&msg[..pos]) };

            ctx.success(msg_str);

        }

        None => {

            ctx.warning("Process spawning not yet implemented");

            ctx.info("In a full implementation, this would:");

            ctx.info("  1. Send request to process manager");

            ctx.info("  2. Load executable from filesystem");

            ctx.info("  3. Create new address space");

            ctx.info("  4. Start execution");

        }

    }



    CommandResult::Ok

}



/// mem command - display memory usage

pub fn cmd_memory(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {

    ctx.println("");

    ctx.println_colored("Memory Usage", Theme::TEXT_INFO);

    ctx.println("------------");

    ctx.println("");



    let (total_kb, used_kb, free_kb) = ctx.ipc.query_memory();



    // Total memory

    let mut line = [0u8; 64];

    let mut pos = 0;

    for byte in "Total:     ".bytes() {

        line[pos] = byte;

        pos += 1;

    }

    pos += format_size_kb(total_kb, &mut line[pos..]);

    let line_str = unsafe { core::str::from_utf8_unchecked(&line[..pos]) };

    ctx.println(line_str);



    // Used memory

    pos = 0;

    for byte in "Used:      ".bytes() {

        line[pos] = byte;

        pos += 1;

    }

    pos += format_size_kb(used_kb, &mut line[pos..]);



    // Calculate percentage

    let percent = if total_kb > 0 {

        (used_kb * 100 / total_kb) as u8

    } else {

        0

    };

    for byte in " (".bytes() {

        line[pos] = byte;

        pos += 1;

    }

    pos += format_number(percent as u64, &mut line[pos..]);

    for byte in "%)".bytes() {

        line[pos] = byte;

        pos += 1;

    }



    let line_str = unsafe { core::str::from_utf8_unchecked(&line[..pos]) };

    ctx.println(line_str);



    // Free memory

    pos = 0;

    for byte in "Free:      ".bytes() {

        line[pos] = byte;

        pos += 1;

    }

    pos += format_size_kb(free_kb, &mut line[pos..]);



    let free_percent = if total_kb > 0 {

        (free_kb * 100 / total_kb) as u8

    } else {

        0

    };

    for byte in " (".bytes() {

        line[pos] = byte;

        pos += 1;

    }

    pos += format_number(free_percent as u64, &mut line[pos..]);

    for byte in "%)".bytes() {

        line[pos] = byte;

        pos += 1;

    }



    let line_str = unsafe { core::str::from_utf8_unchecked(&line[..pos]) };

    ctx.println(line_str);



    ctx.println("");



    // Memory bar visualization

    let bar_width = 40usize;

    let used_bars = if total_kb > 0 {

        ((used_kb * bar_width as u64) / total_kb) as usize

    } else {

        0

    };



    let mut bar = [0u8; 64];

    pos = 0;

    bar[pos] = b'[';

    pos += 1;



    for i in 0..bar_width {

        if i < used_bars {

            bar[pos] = b'#';

        } else {

            bar[pos] = b'-';

        }

        pos += 1;

    }



    bar[pos] = b']';

    pos += 1;



    let bar_str = unsafe { core::str::from_utf8_unchecked(&bar[..pos]) };

    ctx.println_colored(bar_str, Theme::TEXT_INFO);

    ctx.println("");



    CommandResult::Ok

}



/// services command - list registered services

pub fn cmd_services(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {

    ctx.println("");

    ctx.println_colored("Registered Services", Theme::TEXT_INFO);

    ctx.println("-------------------");

    ctx.println("");

    ctx.println("NAME                PORT    STATUS");

    ctx.println("----                ----    ------");



    ctx.ipc.query_services(|name, port, status| {

        let mut line = [0u8; 64];

        let mut pos = 0;



        // Name (20 chars)

        for byte in name.bytes() {

            if pos < 20 {

                line[pos] = byte;

                pos += 1;

            }

        }

        while pos < 20 {

            line[pos] = b' ';

            pos += 1;

        }



        // Port (8 chars)

        let mut port_str = [0u8; 8];

        let port_len = format_number(port, &mut port_str);

        for i in 0..port_len {

            line[pos] = port_str[i];

            pos += 1;

        }

        while pos < 28 {

            line[pos] = b' ';

            pos += 1;

        }



        // Status

        for byte in status.bytes() {

            if pos < 60 {

                line[pos] = byte;

                pos += 1;

            }

        }



        let line_str = unsafe { core::str::from_utf8_unchecked(&line[..pos]) };



        // Color based on status

        if status == "active" {

            ctx.println_colored(line_str, Theme::TEXT_SUCCESS);

        } else {

            ctx.println(line_str);

        }

    });



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



/// Format size in KB as human-readable

fn format_size_kb(kb: u64, buffer: &mut [u8]) -> usize {

    let mut pos = 0;



    if kb < 1024 {

        pos += format_number(kb, &mut buffer[pos..]);

        for byte in " KB".bytes() {

            if pos < buffer.len() {

                buffer[pos] = byte;

                pos += 1;

            }

        }

    } else if kb < 1024 * 1024 {

        pos += format_number(kb / 1024, &mut buffer[pos..]);

        for byte in " MB".bytes() {

            if pos < buffer.len() {

                buffer[pos] = byte;

                pos += 1;

            }

        }

    } else {

        pos += format_number(kb / (1024 * 1024), &mut buffer[pos..]);

        for byte in " GB".bytes() {

            if pos < buffer.len() {

                buffer[pos] = byte;

                pos += 1;

            }

        }

    }



    pos

}