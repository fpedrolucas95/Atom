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

const ASCII_LOGO: &str = r#"
                          vmiddirv
                       rWAAAAAAAAAA6v
                     vNAAAAAAAAAAAAAA1
                    vIAAAA0v    r1AAAAN
                    WAAAEr        mEAAANv
                   vAAAAd          1AAAAAr
                   vAAAA1          RAAAAAAm
                    dAAAAd        1AAAAAAAA0
                 v   1AAAA6      RAAAAi0AAAA1
              vRAAAi  dAAAAW   vEAAAAi  dAAAARv
             dAAAAEm   iAAAAi vEAAAIv    mEAAAEv
            RAAAA1      v0Wi iAAAANv      vIAAAEm
          mIAAAEi           0AAAAW         vRAAAAd
         6AAAANr           WAAAA6            1AAAA0
       vIAAAAd           vIAAAAd              0AAAAW
       1AAAIv            IAAAEr                iEAAAd
      vNAAAd           mAAAAEr  dIERv           RAAAR
      vNAAAd          iAAAAR    IAAAAm          RAAAW
       1AAAEm        1AAAAW      RAAAEi        dAAAEi
        NAAAAIm    0AAAAA0        RAAAAIi    dEAAAA1
         0AAAAAAAAAAAAAIr          dEAAAAAAAAAAAAIm
           iRAAAAAAAE1r              mWEAAAAAAEWm
                v
"#;

/// help command - display available commands or detailed help for one command
pub fn cmd_help(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    if let Some(topic) = cmd.arg(0) {
        // ── Per-command help with examples ───────────────────────────────────
        show_command_help(topic, ctx);
    } else {
        // ── Full command listing ──────────────────────────────────────────────
        ctx.println("");
        ctx.println_colored("╔══════════════════════════════════════════════╗", Theme::TEXT_INFO);
        ctx.println_colored("║        Atom Terminal – Command Reference     ║", Theme::TEXT_INFO);
        ctx.println_colored("╚══════════════════════════════════════════════╝", Theme::TEXT_INFO);
        ctx.println_colored("  Scroll up (Page Up / Ctrl+Up) to read more", Theme::TEXT_DIM);
        ctx.println("");

        let commands = get_all_commands();

        // Category headers and their members
        let categories: &[(&str, &[&str])] = &[
            ("System", &["help","version","uptime","date","sysinfo","clear","echo","log"]),
            ("Process", &["ps","kill","exec","mem","services"]),
            ("Navigation & Read", &["ls","cd","pwd","cat","head","tail","wc","stat","find","tree"]),
            ("Write & Manage", &["mkdir","rmdir","rm","mv","cp","touch","chmod","ln","df","du"]),
            ("Terminal", &["exit","ports","caps"]),
        ];

        for (cat_name, members) in categories {
            ctx.println_colored(cat_name, Theme::PROMPT_USER);
            ctx.println_colored("──────────────────────────────────────────────", Theme::SEPARATOR);

            for (name, usage, desc) in commands.iter() {
                // name, usage, desc are &&str here (slice iter returns &T)
                if !members.iter().any(|m| m == name) { continue; }

                // Format:  usage (padded to 32 chars) + desc
                let mut line = [0u8; 92];
                let mut pos = 0;
                line[pos] = b' '; pos += 1;
                line[pos] = b' '; pos += 1;
                for b in usage.bytes() { if pos < 32 { line[pos] = b; pos += 1; } }
                while pos < 34 { line[pos] = b' '; pos += 1; }
                for b in desc.bytes()  { if pos < 90 { line[pos] = b; pos += 1; } }
                let s = unsafe { core::str::from_utf8_unchecked(&line[..pos]) };
                ctx.println(s);
            }
            ctx.println("");
        }

        ctx.println_colored("Keyboard shortcuts", Theme::PROMPT_USER);
        ctx.println_colored("──────────────────────────────────────────────", Theme::SEPARATOR);
        ctx.println("  Ctrl+C   Cancel current input");
        ctx.println("  Ctrl+L   Clear screen");
        ctx.println("  Ctrl+A   Beginning of line");
        ctx.println("  Ctrl+E   End of line");
        ctx.println("  Ctrl+U   Clear current line");
        ctx.println("  Ctrl+K   Delete to end of line");
        ctx.println("  Ctrl+D   Exit (when input is empty)");
        ctx.println("  Up/Down  Navigate command history");
        ctx.println("  PgUp     Scroll terminal output up");
        ctx.println("  PgDn     Scroll terminal output down");
        ctx.println("");
        ctx.println_colored("  Type 'help <command>' for usage and examples.", Theme::TEXT_DIM);
        ctx.println("");
    }

    CommandResult::Ok
}

/// Per-command detailed help with examples
fn show_command_help(topic: &str, ctx: &mut CommandContext<'_>) {
    ctx.println("");

    // Check static table first
    if let Some((usage, desc)) = get_command_help(topic) {
        ctx.println_colored(usage, Theme::TEXT_INFO);
        ctx.println_colored("──────────────────────────────────────────────", Theme::SEPARATOR);
        ctx.println(desc);
        ctx.println("");
    }

    // Print detailed help per command
    match topic {
        "ls" | "dir" => {
            ctx.println_colored("Options:", Theme::TEXT_DIM);
            ctx.println("  -l, --long   Long format with type and size");
            ctx.println("  -a, --all    Show hidden files (starting with .)");
            ctx.println("");
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  ls");
            ctx.println("  ls /etc");
            ctx.println("  ls -l /var/log");
            ctx.println("  ls -la");
        }
        "cd" => {
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  cd /home/user");
            ctx.println("  cd ..           (go up one level)");
            ctx.println("  cd ~            (go to root)");
            ctx.println("  cd -            (print current dir)");
        }
        "cat" | "type" => {
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  cat readme.txt");
            ctx.println("  cat /etc/version");
            ctx.println("  cat file1.txt file2.txt   (concatenate)");
        }
        "head" => {
            ctx.println_colored("Options:", Theme::TEXT_DIM);
            ctx.println("  -n N   Show first N lines (default: 10)");
            ctx.println("");
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  head log.txt");
            ctx.println("  head -n 20 /var/log/system.log");
        }
        "tail" => {
            ctx.println_colored("Options:", Theme::TEXT_DIM);
            ctx.println("  -n N   Show last N lines (default: 10)");
            ctx.println("");
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  tail log.txt");
            ctx.println("  tail -n 50 /var/log/boot.log");
        }
        "wc" => {
            ctx.println_colored("Options:", Theme::TEXT_DIM);
            ctx.println("  -l   Count lines only");
            ctx.println("  -w   Count words only");
            ctx.println("  -c   Count bytes only");
            ctx.println("");
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  wc readme.txt          (lines, words, bytes)");
            ctx.println("  wc -l /var/log/sys.log");
            ctx.println("  wc -c large_file.bin");
        }
        "stat" => {
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  stat readme.txt");
            ctx.println("  stat /etc/version");
        }
        "find" => {
            ctx.println_colored("Options:", Theme::TEXT_DIM);
            ctx.println("  -name <pattern>       Match filename (supports * wildcard)");
            ctx.println("  -maxdepth <N>         Limit search depth");
            ctx.println("");
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  find /home -name '*.txt'");
            ctx.println("  find . -name 'main*'");
            ctx.println("  find /etc -maxdepth 1");
        }
        "tree" => {
            ctx.println_colored("Options:", Theme::TEXT_DIM);
            ctx.println("  -d N   Maximum depth (default: 3)");
            ctx.println("");
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  tree");
            ctx.println("  tree /home");
            ctx.println("  tree -d 2 /etc");
        }
        "mkdir" => {
            ctx.println_colored("Options:", Theme::TEXT_DIM);
            ctx.println("  -p, --parents   Create parent directories as needed");
            ctx.println("");
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  mkdir docs");
            ctx.println("  mkdir /home/user/projects");
            ctx.println("  mkdir -p /home/user/a/b/c");
        }
        "rmdir" => {
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  rmdir empty_dir");
            ctx.println("  rmdir /tmp/cache");
        }
        "rm" => {
            ctx.println_colored("Options:", Theme::TEXT_DIM);
            ctx.println("  -r, -R   Recursive (remove directory and contents)");
            ctx.println("  -f       Force (ignore errors)");
            ctx.println("");
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  rm old_file.txt");
            ctx.println("  rm -r /tmp/build");
            ctx.println("  rm -rf /tmp/old_cache");
        }
        "mv" => {
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  mv file.txt renamed.txt");
            ctx.println("  mv notes.txt /home/user/docs/");
            ctx.println("  mv old_dir/ new_dir/");
        }
        "cp" => {
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  cp readme.txt readme.bak");
            ctx.println("  cp /etc/config /home/user/config.bak");
        }
        "touch" => {
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  touch newfile.txt    (create if not exists)");
            ctx.println("  touch existing.log   (update timestamp)");
        }
        "chmod" => {
            ctx.println_colored("Common modes:", Theme::TEXT_DIM);
            ctx.println("  755   rwxr-xr-x   (executable, world-readable)");
            ctx.println("  644   rw-r--r--   (regular file)");
            ctx.println("  600   rw-------   (private file)");
            ctx.println("  777   rwxrwxrwx   (world-writable – use with care)");
            ctx.println("");
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  chmod 755 script.sh");
            ctx.println("  chmod 644 readme.txt");
            ctx.println("  chmod 600 /etc/secret.key");
        }
        "ln" => {
            ctx.println_colored("Options:", Theme::TEXT_DIM);
            ctx.println("  -s, --symbolic   Create symbolic link (default: hard link)");
            ctx.println("");
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  ln -s /home/user/docs /tmp/docs_link");
            ctx.println("  ln /etc/config /home/user/config");
        }
        "df" => {
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  df           (show all mounted filesystems)");
        }
        "du" => {
            ctx.println_colored("Options:", Theme::TEXT_DIM);
            ctx.println("  -s, --summarize    Show only total");
            ctx.println("  -h, --human        Human-readable sizes");
            ctx.println("");
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  du /home/user");
            ctx.println("  du -sh /var");
            ctx.println("  du -h /tmp");
        }
        "ps" | "procs" => {
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  ps          (list all running processes)");
        }
        "kill" => {
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  kill 42     (terminate process 42)");
        }
        "exec" | "run" => {
            ctx.println_colored("Examples:", Theme::PROMPT_USER);
            ctx.println("  exec terminal");
            ctx.println("  run fileman");
        }
        _ => {
            if get_command_help(topic).is_none() {
                ctx.error("No help available for that command.");
                ctx.info("  Type 'help' to see all commands.");
            }
        }
    }
    ctx.println("");
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

pub fn cmd_date(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let ticks = get_ticks();
    let total_seconds = ticks / 100;

    let hours = (total_seconds / 3600) % 24;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    let mut time_str = [0u8; 32];
    let mut pos = 0;

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

pub fn cmd_sysinfo(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    ctx.println("");

    for line in ASCII_LOGO.lines() {
        if !line.is_empty() {
            ctx.println_colored(line, Theme::TEXT_INFO);
        }
    }

    ctx.println("");

    let mut version_buf = [0u8; 128];
    if let Some(len) = ctx.ipc.read_file("/etc/version", &mut version_buf) {
        let version_str = unsafe { core::str::from_utf8_unchecked(&version_buf[..len]) };
        for line in version_str.lines() {
            if !line.is_empty() {
                ctx.println(line);
            }
        }
    } else {
        ctx.print("OS:           ");
        ctx.println_colored(OS_NAME, Theme::PROMPT_USER);
        ctx.print("Kernel:       ");
        ctx.println(KERNEL_VERSION);
    }

    let pkg_count = ctx.ipc.count_processes();
    ctx.print("Packages:     ");
    let mut num_buf = [0u8; 16];
    let n = format_number(pkg_count as u64, &mut num_buf);
    ctx.println(unsafe { core::str::from_utf8_unchecked(&num_buf[..n]) });

    if let Some((w, h)) = ctx.ipc.get_resolution() {
        ctx.print("Resolution:   ");
        let mut res_buf = [0u8; 32];
        let mut pos = format_number(w as u64, &mut res_buf);
        res_buf[pos] = b'x';
        pos += 1;
        pos += format_number(h as u64, &mut res_buf[pos..]);
        ctx.println(unsafe { core::str::from_utf8_unchecked(&res_buf[..pos]) });
    }

    ctx.print("CPU:          ");
    let mut cpu_buf = [0u8; 64];
    let cpu_len = ctx.ipc.get_cpu_brand(&mut cpu_buf);
    if cpu_len > 0 {
        ctx.println(unsafe { core::str::from_utf8_unchecked(&cpu_buf[..cpu_len]) });
    } else {
        ctx.println("x86_64 (Atom Core)");
    }

    // Memory
    let (total, used, _) = ctx.ipc.query_memory();
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

    let mut uptime_buf = [0u8; 64];
    let len = ctx.ipc.format_uptime(&mut uptime_buf);
    let uptime = unsafe { core::str::from_utf8_unchecked(&uptime_buf[..len]) };
    ctx.print("Uptime:       ");
    ctx.println(uptime);

    ctx.println("");

    CommandResult::Ok
}

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