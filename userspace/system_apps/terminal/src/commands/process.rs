// Process Commands
//
// Commands for process management: listing, killing, spawning processes.
// All process information is obtained via IPC to the process manager service.

use super::{CommandContext, CommandResult};
use crate::parser::{parse_number, ParsedCommand};
use crate::window::Theme;
use atom_syscall::debug::{
    get_resource_usage, ProcessResourceUsage, SystemResourceUsage, MAX_RESOURCE_CPUS,
};
use atom_syscall::graphics::Color;
use atom_syscall::thread::sleep_ms;

const MAX_RESOURCE_PROCESSES: usize = 32;
const RESOURCE_BAR_WIDTH: usize = 52;

#[derive(Clone, Copy)]
struct ResourceSegment<'a> {
    name: &'a str,
    value_kb: u64,
    color: Color,
}

impl<'a> ResourceSegment<'a> {
    const fn empty() -> Self {
        Self {
            name: "",
            value_kb: 0,
            color: Theme::TEXT_NORMAL,
        }
    }
}

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

        let mut pid_str = [0u8; 8];
        let pid_len = format_number(pid, &mut pid_str);

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

/// kill command
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

/// exec command
pub fn cmd_exec(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let program = match cmd.arg(0) {
        Some(arg) => arg,
        None => {
            ctx.error("Usage: exec <program> [args...]");
            return CommandResult::Error;
        }
    };

    ctx.info("Attempting to execute program...");

    let args: [&str; 16] = {
        let mut arr = [""; 16];
        for i in 1..cmd.arg_count.min(16) {
            arr[i - 1] = cmd.args[i];
        }
        arr
    };

    match ctx
        .ipc
        .spawn_process(program, &args[..cmd.arg_count.saturating_sub(1)])
    {
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
            ctx.error("Failed to spawn process");
        }
    }

    CommandResult::Ok
}

/// mem command
pub fn cmd_memory(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let mut system = SystemResourceUsage::empty();
    let mut processes = [ProcessResourceUsage::empty(); MAX_RESOURCE_PROCESSES];
    let process_count = get_resource_usage(&mut system, &mut processes).min(processes.len());

    if system.total_ram_kb == 0 {
        ctx.error("Unable to read memory accounting from kernel");
        return CommandResult::Error;
    }

    let used_ram_kb = system.total_ram_kb.saturating_sub(system.free_ram_kb);
    let process_private_kb = processes[..process_count]
        .iter()
        .map(|process| process.private_memory_kb)
        .fold(0u64, u64::saturating_add);
    let system_kb = used_ram_kb
        .saturating_sub(process_private_kb)
        .saturating_sub(system.shared_memory_kb);
    let total_resources_kb = system.total_ram_kb.saturating_add(system.framebuffer_kb);

    ctx.println("");
    ctx.println_colored("Memory Usage", Theme::TEXT_INFO);
    ctx.println("------------");
    ctx.println("");

    print_value_line(ctx, "RAM total", system.total_ram_kb, system.total_ram_kb);
    print_value_line(ctx, "RAM used", used_ram_kb, system.total_ram_kb);
    print_value_line(ctx, "RAM free", system.free_ram_kb, system.total_ram_kb);
    if system.framebuffer_kb > 0 {
        print_value_line(
            ctx,
            "Framebuffer",
            system.framebuffer_kb,
            total_resources_kb,
        );
    }
    ctx.println("");

    let mut segments = [ResourceSegment::empty(); MAX_RESOURCE_PROCESSES + 4];
    let mut segment_count = 0usize;
    push_segment(
        &mut segments,
        &mut segment_count,
        ResourceSegment {
            name: "System",
            value_kb: system_kb,
            color: Theme::RESOURCE_SYSTEM,
        },
    );
    push_segment(
        &mut segments,
        &mut segment_count,
        ResourceSegment {
            name: "Framebuffer",
            value_kb: system.framebuffer_kb,
            color: Theme::RESOURCE_FRAMEBUFFER,
        },
    );
    push_segment(
        &mut segments,
        &mut segment_count,
        ResourceSegment {
            name: "Shared memory",
            value_kb: system.shared_memory_kb,
            color: Theme::RESOURCE_SHARED,
        },
    );

    for (index, process) in processes[..process_count].iter().enumerate() {
        if process.private_memory_kb == 0 {
            continue;
        }
        push_segment(
            &mut segments,
            &mut segment_count,
            ResourceSegment {
                name: process.name_str(),
                value_kb: process.private_memory_kb,
                color: app_color(index),
            },
        );
    }

    push_segment(
        &mut segments,
        &mut segment_count,
        ResourceSegment {
            name: "Free",
            value_kb: system.free_ram_kb,
            color: Theme::RESOURCE_FREE,
        },
    );

    draw_segmented_bar(ctx, &segments[..segment_count], total_resources_kb);
    ctx.println("");
    for segment in &segments[..segment_count] {
        print_legend_line(ctx, *segment, total_resources_kb);
    }
    ctx.println_colored(
        "Framebuffer is device memory; process values are private resident RAM.",
        Theme::TEXT_DIM,
    );
    ctx.println("");

    CommandResult::Ok
}

pub fn cmd_cpu(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let mut before_system = SystemResourceUsage::empty();
    let mut before_processes = [ProcessResourceUsage::empty(); MAX_RESOURCE_PROCESSES];
    let before_count =
        get_resource_usage(&mut before_system, &mut before_processes).min(before_processes.len());

    // The global tick counter advances once per online CPU.
    let sample_scale = before_system.cpu_count.clamp(1, MAX_RESOURCE_CPUS as u64);
    sleep_ms(250 * sample_scale);

    let mut after_system = SystemResourceUsage::empty();
    let mut after_processes = [ProcessResourceUsage::empty(); MAX_RESOURCE_PROCESSES];
    let after_count =
        get_resource_usage(&mut after_system, &mut after_processes).min(after_processes.len());

    let cpu_count = (after_system.cpu_count as usize).min(MAX_RESOURCE_CPUS);
    if cpu_count == 0 {
        ctx.error("Unable to read CPU accounting from kernel");
        return CommandResult::Error;
    }

    let mut total_delta = 0u64;
    let mut idle_delta = 0u64;
    for cpu in 0..cpu_count {
        total_delta = total_delta.saturating_add(
            after_system.cpu_total_ticks[cpu].saturating_sub(before_system.cpu_total_ticks[cpu]),
        );
        idle_delta = idle_delta.saturating_add(
            after_system.cpu_idle_ticks[cpu].saturating_sub(before_system.cpu_idle_ticks[cpu]),
        );
    }
    let busy_delta = total_delta.saturating_sub(idle_delta);

    ctx.println("");
    ctx.println_colored("CPU Usage (250 ms sample)", Theme::TEXT_INFO);
    ctx.println("-------------------------");
    ctx.println("");
    print_cpu_line(ctx, "Total", busy_delta, total_delta, Theme::TEXT_SUCCESS);

    for cpu in 0..cpu_count {
        let cpu_total =
            after_system.cpu_total_ticks[cpu].saturating_sub(before_system.cpu_total_ticks[cpu]);
        let cpu_idle =
            after_system.cpu_idle_ticks[cpu].saturating_sub(before_system.cpu_idle_ticks[cpu]);
        let mut label = [0u8; 16];
        let mut pos = 0;
        for byte in "CPU ".bytes() {
            label[pos] = byte;
            pos += 1;
        }
        pos += format_number(cpu as u64, &mut label[pos..]);
        let label = unsafe { core::str::from_utf8_unchecked(&label[..pos]) };
        print_cpu_line(
            ctx,
            label,
            cpu_total.saturating_sub(cpu_idle),
            cpu_total,
            app_color(cpu),
        );
    }

    ctx.println("");
    ctx.println_colored("By process", Theme::TEXT_INFO);

    let mut process_busy = 0u64;
    for (index, process) in after_processes[..after_count].iter().enumerate() {
        let previous_ticks = before_processes[..before_count]
            .iter()
            .find(|previous| previous.pid == process.pid)
            .map(|previous| previous.cpu_ticks)
            .unwrap_or(process.cpu_ticks);
        let delta = process.cpu_ticks.saturating_sub(previous_ticks);
        if delta == 0 {
            continue;
        }
        process_busy = process_busy.saturating_add(delta);
        print_cpu_line(
            ctx,
            process.name_str(),
            delta,
            total_delta,
            app_color(index),
        );
    }

    let kernel_delta = busy_delta.saturating_sub(process_busy);
    if kernel_delta > 0 {
        print_cpu_line(
            ctx,
            "System",
            kernel_delta,
            total_delta,
            Theme::RESOURCE_SYSTEM,
        );
    }
    ctx.println("");

    CommandResult::Ok
}

fn push_segment<'a>(
    segments: &mut [ResourceSegment<'a>],
    count: &mut usize,
    segment: ResourceSegment<'a>,
) {
    if segment.value_kb == 0 || *count >= segments.len() {
        return;
    }
    segments[*count] = segment;
    *count += 1;
}

fn app_color(index: usize) -> Color {
    match index % 4 {
        0 => Theme::RESOURCE_APP_1,
        1 => Theme::RESOURCE_APP_2,
        2 => Theme::RESOURCE_APP_3,
        _ => Theme::RESOURCE_APP_4,
    }
}

fn draw_segmented_bar(
    ctx: &mut CommandContext<'_>,
    segments: &[ResourceSegment<'_>],
    total_kb: u64,
) {
    ctx.print("[");
    let mut cumulative = 0u64;
    let mut segment_index = 0usize;

    for column in 0..RESOURCE_BAR_WIDTH {
        let sample = if total_kb == 0 {
            0
        } else {
            ((column as u64 * 2 + 1) * total_kb) / (RESOURCE_BAR_WIDTH as u64 * 2)
        };
        while segment_index + 1 < segments.len()
            && sample >= cumulative.saturating_add(segments[segment_index].value_kb)
        {
            cumulative = cumulative.saturating_add(segments[segment_index].value_kb);
            segment_index += 1;
        }
        let color = segments
            .get(segment_index)
            .map(|segment| segment.color)
            .unwrap_or(Theme::RESOURCE_FREE);
        ctx.print_colored("#", color);
    }
    ctx.println("]");
}

fn print_legend_line(ctx: &mut CommandContext<'_>, segment: ResourceSegment<'_>, total_kb: u64) {
    ctx.print_colored("# ", segment.color);
    let mut line = [0u8; 128];
    let mut pos = copy_bytes(segment.name.as_bytes(), &mut line, 0);
    if pos < line.len() {
        line[pos] = b':';
        pos += 1;
    }
    if pos < line.len() {
        line[pos] = b' ';
        pos += 1;
    }
    pos += format_size_kb(segment.value_kb, &mut line[pos..]);
    pos = append_percent(segment.value_kb, total_kb, &mut line, pos);
    ctx.println(unsafe { core::str::from_utf8_unchecked(&line[..pos]) });
}

fn print_value_line(ctx: &mut CommandContext<'_>, name: &str, value: u64, total: u64) {
    let mut line = [0u8; 96];
    let mut pos = copy_bytes(name.as_bytes(), &mut line, 0);
    if pos < 14 {
        while pos < 14 {
            line[pos] = b' ';
            pos += 1;
        }
    }
    pos += format_size_kb(value, &mut line[pos..]);
    pos = append_percent(value, total, &mut line, pos);
    ctx.println(unsafe { core::str::from_utf8_unchecked(&line[..pos]) });
}

fn print_cpu_line(
    ctx: &mut CommandContext<'_>,
    name: &str,
    busy_ticks: u64,
    total_ticks: u64,
    color: Color,
) {
    let percent = if total_ticks == 0 {
        0
    } else {
        busy_ticks.saturating_mul(100) / total_ticks
    };
    let width = 30usize;
    let used = ((percent.min(100) * width as u64) / 100) as usize;

    let mut label = [0u8; 40];
    let mut pos = copy_bytes(name.as_bytes(), &mut label, 0);
    while pos < 16 {
        label[pos] = b' ';
        pos += 1;
    }
    ctx.print(unsafe { core::str::from_utf8_unchecked(&label[..pos]) });
    ctx.print("[");
    for column in 0..width {
        if column < used {
            ctx.print_colored("#", color);
        } else {
            ctx.print_colored("-", Theme::RESOURCE_FREE);
        }
    }
    ctx.print("] ");
    let mut percent_text = [0u8; 8];
    let len = format_number(percent, &mut percent_text);
    ctx.print(unsafe { core::str::from_utf8_unchecked(&percent_text[..len]) });
    ctx.println("%");
}

fn append_percent(value: u64, total: u64, buffer: &mut [u8], mut pos: usize) -> usize {
    let percent = if total == 0 {
        0
    } else {
        value.saturating_mul(100) / total
    };
    for byte in " (".bytes() {
        if pos < buffer.len() {
            buffer[pos] = byte;
            pos += 1;
        }
    }
    pos += format_number(percent, &mut buffer[pos..]);
    for byte in "%)".bytes() {
        if pos < buffer.len() {
            buffer[pos] = byte;
            pos += 1;
        }
    }
    pos
}

fn copy_bytes(source: &[u8], destination: &mut [u8], start: usize) -> usize {
    let mut pos = start;
    for byte in source {
        if pos >= destination.len() {
            break;
        }
        destination[pos] = *byte;
        pos += 1;
    }
    pos
}

/// services command - list registered services
pub fn cmd_services(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    ctx.println("");
    ctx.println_colored("Registered Services", Theme::TEXT_INFO);
    ctx.println("-------------------");
    ctx.println("");
    ctx.println("NAME                 PORT     STATUS");
    ctx.println("----                 ----     ------");

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

        line[pos] = b' ';
        pos += 1;

        // Port (8 chars)
        let mut port_str = [0u8; 8];
        let port_len = format_number(port, &mut port_str);
        for i in 0..port_len {
            line[pos] = port_str[i];
            pos += 1;
        }
        while pos < 29 {
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

/// number formatter
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

    for i in 0..count {
        buffer[i] = digits[count - 1 - i];
    }

    count
}

/// size formatter
fn format_size_kb(kb: u64, buffer: &mut [u8]) -> usize {
    let mut pos = 0;

    if kb < 1024 {
        pos += format_number(kb, &mut buffer[pos..]);
        for b in " KB".bytes() {
            buffer[pos] = b;
            pos += 1;
        }
    } else if kb < 1024 * 1024 {
        let mb = kb / 1024;
        pos += format_number(mb, &mut buffer[pos..]);
        for b in " MB".bytes() {
            buffer[pos] = b;
            pos += 1;
        }
    } else {
        let gb = kb / (1024 * 1024);
        let frac = (kb % (1024 * 1024)) * 10 / (1024 * 1024);

        pos += format_number(gb, &mut buffer[pos..]);
        buffer[pos] = b'.';
        pos += 1;
        pos += format_number(frac, &mut buffer[pos..]);

        for b in " GB".bytes() {
            buffer[pos] = b;
            pos += 1;
        }
    }

    pos
}
