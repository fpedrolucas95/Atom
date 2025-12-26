// Filesystem Commands

//

// Commands for filesystem navigation and file viewing.

// All filesystem operations are performed via IPC to the filesystem service.



use super::{CommandContext, CommandResult};

use crate::parser::ParsedCommand;

use crate::window::Theme;



/// Current working directory (static storage for no_std)

static mut CURRENT_DIR: [u8; 256] = [b'/'; 256];

static mut CURRENT_DIR_LEN: usize = 1;



/// Get current directory as string

fn get_current_dir() -> &'static str {

    unsafe {

        core::str::from_utf8_unchecked(&CURRENT_DIR[..CURRENT_DIR_LEN])

    }

}



/// Set current directory

fn set_current_dir(path: &str) {

    unsafe {

        let len = path.len().min(255);

        CURRENT_DIR[..len].copy_from_slice(path.as_bytes());

        CURRENT_DIR_LEN = len;

    }

}



/// ls command - list directory contents

pub fn cmd_ls(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {

    let path = cmd.arg(0).unwrap_or(get_current_dir());

    let show_all = cmd.has_flag("-a", "--all");

    let long_format = cmd.has_flag("-l", "--long");



    ctx.println("");



    let mut header = [0u8; 64];

    let mut pos = 0;

    for byte in "Contents of ".bytes() {

        header[pos] = byte;

        pos += 1;

    }

    for byte in path.bytes() {

        if pos < 60 {

            header[pos] = byte;

            pos += 1;

        }

    }

    let header_str = unsafe { core::str::from_utf8_unchecked(&header[..pos]) };

    ctx.println_colored(header_str, Theme::TEXT_INFO);

    ctx.println("");



    if long_format {

        ctx.println("TYPE  SIZE      NAME");

        ctx.println("----  ----      ----");

    }



    ctx.ipc.list_directory(path, |name, is_dir, size| {

        // Skip hidden files unless -a

        if !show_all && name.starts_with('.') {

            return;

        }



        if long_format {

            let mut line = [0u8; 80];

            let mut lpos = 0;



            // Type indicator

            if is_dir {

                for byte in "dir   ".bytes() {

                    line[lpos] = byte;

                    lpos += 1;

                }

            } else {

                for byte in "file  ".bytes() {

                    line[lpos] = byte;

                    lpos += 1;

                }

            }



            // Size (10 chars)

            if is_dir {

                for byte in "-         ".bytes() {

                    line[lpos] = byte;

                    lpos += 1;

                }

            } else {

                let mut size_buf = [0u8; 10];

                let size_len = format_number(size, &mut size_buf);

                for i in 0..size_len {

                    line[lpos] = size_buf[i];

                    lpos += 1;

                }

                while lpos < 16 {

                    line[lpos] = b' ';

                    lpos += 1;

                }

            }



            // Name

            for byte in name.bytes() {

                if lpos < 76 {

                    line[lpos] = byte;

                    lpos += 1;

                }

            }



            let line_str = unsafe { core::str::from_utf8_unchecked(&line[..lpos]) };



            if is_dir {

                ctx.println_colored(line_str, Theme::PROMPT_PATH);

            } else {

                ctx.println(line_str);

            }

        } else {

            // Short format

            if is_dir {

                let mut dir_name = [0u8; 64];

                let mut dpos = 0;

                for byte in name.bytes() {

                    dir_name[dpos] = byte;

                    dpos += 1;

                }

                dir_name[dpos] = b'/';

                dpos += 1;

                let dir_str = unsafe { core::str::from_utf8_unchecked(&dir_name[..dpos]) };

                ctx.println_colored(dir_str, Theme::PROMPT_PATH);

            } else {

                ctx.println(name);

            }

        }

    });



    ctx.println("");



    CommandResult::Ok

}



/// cd command - change directory

pub fn cmd_cd(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {

    let target = match cmd.arg(0) {

        Some(path) => path,

        None => {

            // cd with no args goes to root

            set_current_dir("/");

            return CommandResult::Ok;

        }

    };



    // Handle special paths

    if target == "~" || target == "/" {

        set_current_dir("/");

        return CommandResult::Ok;

    }



    if target == ".." {

        // Go up one level

        let current = get_current_dir();

        if current == "/" {

            return CommandResult::Ok;

        }



        // Find last slash

        let bytes = current.as_bytes();

        let mut last_slash = 0;

        for i in 0..bytes.len() - 1 {

            if bytes[i] == b'/' {

                last_slash = i;

            }

        }



        if last_slash == 0 {

            set_current_dir("/");

        } else {

            let new_path = unsafe { core::str::from_utf8_unchecked(&bytes[..last_slash]) };

            set_current_dir(new_path);

        }

        return CommandResult::Ok;

    }



    if target == "." {

        return CommandResult::Ok;

    }



    // Build new path

    let mut new_path = [0u8; 256];

    let mut pos = 0;



    if target.starts_with('/') {

        // Absolute path

        for byte in target.bytes() {

            if pos < 255 {

                new_path[pos] = byte;

                pos += 1;

            }

        }

    } else {

        // Relative path

        let current = get_current_dir();

        for byte in current.bytes() {

            if pos < 254 {

                new_path[pos] = byte;

                pos += 1;

            }

        }

        if pos > 0 && new_path[pos - 1] != b'/' {

            new_path[pos] = b'/';

            pos += 1;

        }

        for byte in target.bytes() {

            if pos < 255 {

                new_path[pos] = byte;

                pos += 1;

            }

        }

    }



    // Remove trailing slash if not root

    if pos > 1 && new_path[pos - 1] == b'/' {

        pos -= 1;

    }



    let path_str = unsafe { core::str::from_utf8_unchecked(&new_path[..pos]) };

    set_current_dir(path_str);



    CommandResult::Ok

}



/// pwd command - print working directory

pub fn cmd_pwd(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {

    ctx.println_colored(get_current_dir(), Theme::PROMPT_PATH);

    CommandResult::Ok

}



/// cat command - display file contents

pub fn cmd_cat(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {

    let filename = match cmd.arg(0) {

        Some(f) => f,

        None => {

            ctx.error("Usage: cat <filename>");

            return CommandResult::Error;

        }

    };



    // Build full path

    let mut full_path = [0u8; 256];

    let mut pos = 0;



    if filename.starts_with('/') {

        for byte in filename.bytes() {

            if pos < 255 {

                full_path[pos] = byte;

                pos += 1;

            }

        }

    } else {

        let current = get_current_dir();

        for byte in current.bytes() {

            if pos < 254 {

                full_path[pos] = byte;

                pos += 1;

            }

        }

        if pos > 0 && full_path[pos - 1] != b'/' {

            full_path[pos] = b'/';

            pos += 1;

        }

        for byte in filename.bytes() {

            if pos < 255 {

                full_path[pos] = byte;

                pos += 1;

            }

        }

    }



    let path_str = unsafe { core::str::from_utf8_unchecked(&full_path[..pos]) };



    // Try to read file

    let mut buffer = [0u8; 1024];

    match ctx.ipc.read_file(path_str, &mut buffer) {

        Some(len) => {

            ctx.println("");

            let content = unsafe { core::str::from_utf8_unchecked(&buffer[..len]) };

            for line in content.split('\n') {

                ctx.println(line);

            }

            ctx.println("");

        }

        None => {

            ctx.warning("File reading not yet implemented");

            ctx.info("In a full implementation, this would:");

            ctx.info("  1. Send request to filesystem service");

            ctx.info("  2. Receive file data via shared memory");

            ctx.info("  3. Display contents to terminal");

        }

    }



    CommandResult::Ok

}



/// tree command - display directory tree

pub fn cmd_tree(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {

    let path = cmd.arg(0).unwrap_or(get_current_dir());

    let max_depth = cmd.get_option("-d", "--depth")

        .and_then(|s| s.parse().ok())

        .unwrap_or(3u32);



    ctx.println("");

    ctx.println_colored(path, Theme::PROMPT_PATH);



    // Simple tree display (not recursive in early implementation)

    ctx.ipc.list_directory(path, |name, is_dir, _size| {

        let mut line = [0u8; 64];

        let mut pos = 0;



        // Tree prefix

        for byte in "|-- ".bytes() {

            line[pos] = byte;

            pos += 1;

        }



        // Name

        for byte in name.bytes() {

            if pos < 60 {

                line[pos] = byte;

                pos += 1;

            }

        }



        if is_dir {

            line[pos] = b'/';

            pos += 1;

        }



        let line_str = unsafe { core::str::from_utf8_unchecked(&line[..pos]) };



        if is_dir {

            ctx.println_colored(line_str, Theme::PROMPT_PATH);

        } else {

            ctx.println(line_str);

        }

    });



    ctx.println("");

    let _ = max_depth; // Used in full implementation



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