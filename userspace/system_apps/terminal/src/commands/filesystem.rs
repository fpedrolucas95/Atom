// Filesystem Commands
//
// POSIX-compatible filesystem commands for the Atom terminal.
// All operations go through atom_syscall::fs (which routes to fsd in kernel).

use super::{CommandContext, CommandResult};
use crate::parser::ParsedCommand;
use crate::window::Theme;

extern crate alloc;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::format;

// ============================================================================
// Current working directory (static, no-std)
// ============================================================================

static mut CURRENT_DIR: [u8; 256] = [b'/'; 256];
static mut CURRENT_DIR_LEN: usize = 1;

fn get_current_dir() -> &'static str {
    unsafe { core::str::from_utf8_unchecked(&CURRENT_DIR[..CURRENT_DIR_LEN]) }
}

fn set_current_dir(path: &str) {
    unsafe {
        let len = path.len().min(255);
        CURRENT_DIR[..len].copy_from_slice(&path.as_bytes()[..len]);
        CURRENT_DIR_LEN = len;
    }
}

// ============================================================================
// Argument helpers
// ============================================================================

/// Get the first argument that doesn't start with `-` (positional / path arg).
/// Skips flags and respects option values following `-X value` patterns.
fn first_path_arg<'a>(cmd: &'a ParsedCommand<'_>) -> Option<&'a str> {
    let mut skip_next = false;
    for i in 0..cmd.arg_count {
        let a = cmd.args[i];
        if skip_next {
            skip_next = false;
            continue;
        }
        // Known flags that consume the next argument as a value
        if a == "-n" || a == "--lines" || a == "-d" || a == "--depth"
            || a == "-maxdepth" || a == "--maxdepth" || a == "-name"
        {
            skip_next = true;
            continue;
        }
        if !a.starts_with('-') {
            return Some(a);
        }
    }
    None
}

/// For two-arg commands (mv, cp, ln): get positional args in order.
fn positional_args_vec<'a>(cmd: &'a ParsedCommand<'_>) -> Vec<&'a str> {
    cmd.positional_args().collect()
}

// ============================================================================
// Path helpers
// ============================================================================

fn resolve_path(path: &str) -> String {
    if path.is_empty() { return String::from(get_current_dir()); }
    if path.starts_with('/') { return normalize(path); }
    if path == "~" { return String::from("/"); }
    let cwd = get_current_dir();
    let combined = if cwd.ends_with('/') {
        format!("{}{}", cwd, path)
    } else {
        format!("{}/{}", cwd, path)
    };
    normalize(&combined)
}

pub(crate) fn resolve_path_for_shell(path: &str) -> String {
    resolve_path(path)
}

fn normalize(path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => { parts.pop(); }
            s => parts.push(s),
        }
    }
    let mut out = String::from("/");
    for (i, p) in parts.iter().enumerate() {
        if i > 0 { out.push('/'); }
        for b in p.bytes() {
            out.push(b as char);
        }
    }
    if out.is_empty() { out.push('/'); }
    out
}

fn file_extension(name: &str) -> &str {
    name.rfind('.').map(|p| &name[p+1..]).unwrap_or("")
}

fn fmt_size(bytes: u64) -> String {
    if bytes < 1024 { format!("{} B", bytes) }
    else if bytes < 1024*1024 {
        format!("{}.{} KB", bytes/1024, (bytes%1024)*10/1024)
    } else if bytes < 1024*1024*1024 {
        format!("{}.{} MB", bytes/(1024*1024), (bytes%(1024*1024))*10/(1024*1024))
    } else {
        format!("{} GB", bytes/(1024*1024*1024))
    }
}

fn fs_read_file(path: &str) -> core::result::Result<Vec<u8>, atom_syscall::fs::FsError> {
    atom_syscall::fs::read_file(path)
}

fn fs_list_dir(path: &str) -> core::result::Result<Vec<atom_syscall::fs::FsDirent>, atom_syscall::fs::FsError> {
    let flags = atom_syscall::fs::OpenFlags::RDONLY | atom_syscall::fs::OpenFlags::DIRECTORY;
    let fd = atom_syscall::fs::open(path, flags, 0)?;
    let entries = atom_syscall::fs::readdir(fd);
    let _ = atom_syscall::fs::close(fd);
    entries
}

// ============================================================================
// ls – list directory
// ============================================================================

pub fn cmd_ls(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let path      = cmd.arg(0).map(|p| resolve_path(p))
                       .unwrap_or_else(|| String::from(get_current_dir()));
    let show_all  = cmd.has_flag("-a", "--all");
    let long_fmt  = cmd.has_flag("-l", "--long");

    ctx.println("");
    let mut header = [0u8; 72];
    let mut pos = 0;
    for b in b"Contents of ".iter() { if pos < header.len() { header[pos] = *b; pos += 1; } }
    for b in path.bytes()            { if pos < header.len() { header[pos] = b;  pos += 1; } }
    let header_str = unsafe { core::str::from_utf8_unchecked(&header[..pos]) };
    ctx.println_colored(header_str, Theme::TEXT_INFO);
    ctx.println("");

    if long_fmt {
        ctx.println_colored("TYPE  SIZE        NAME", Theme::TEXT_DIM);
        ctx.println_colored("----  ----------  ----", Theme::TEXT_DIM);
    }

    let entries = match fs_list_dir(&path) {
        Ok(v) => v,
        Err(e) => {
            let msg = format!("ls: cannot access '{}': {:?}", path, e);
            ctx.error(&msg);
            return CommandResult::Error;
        }
    };

    for entry in entries {
        let name = entry.name;
        if name == "." || name == ".." { continue; }
        if !show_all && name.starts_with('.') { continue; }

        let is_dir = entry.file_type == atom_syscall::fs::FileType::Directory;
        if long_fmt {
            let type_str = if is_dir { "dir " } else { "file" };
            let size = if is_dir {
                0
            } else {
                let full = if path.ends_with('/') {
                    format!("{}{}", path, name)
                } else {
                    format!("{}/{}", path, name)
                };
                atom_syscall::fs::stat(&full).map(|s| s.size).unwrap_or(0)
            };
            let size_str = if is_dir { String::from("   --      ") }
                else { format!("{:<10}  ", size) };
            let line = format!("{}  {}  {}", type_str, size_str, name);
            if is_dir { ctx.println_colored(&line, Theme::PROMPT_PATH); }
            else       { ctx.println(&line); }
        } else if is_dir {
            let mut dir_buf = [0u8; 64];
            let mut dp = 0;
            for b in name.bytes() { if dp < 63 { dir_buf[dp] = b; dp += 1; } }
            dir_buf[dp] = b'/'; dp += 1;
            let ds = unsafe { core::str::from_utf8_unchecked(&dir_buf[..dp]) };
            ctx.println_colored(ds, Theme::PROMPT_PATH);
        } else {
            ctx.println(&name);
        }
    }

    ctx.println("");
    CommandResult::Ok
}

// ============================================================================
// cd – change directory
// ============================================================================

pub fn cmd_cd(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let target = cmd.arg(0).unwrap_or("~");

    if target == "~" || target == "" {
        set_current_dir("/");
        return CommandResult::Ok;
    }
    if target == "-" {
        ctx.println_colored(get_current_dir(), Theme::PROMPT_PATH);
        return CommandResult::Ok;
    }

    let new = resolve_path(target);
    match atom_syscall::fs::stat(&new) {
        Ok(stat) if stat.is_dir() => {
            set_current_dir(&new);
            CommandResult::Ok
        }
        Ok(_) => {
            let msg = format!("cd: {}: not a directory", target);
            ctx.error(&msg);
            CommandResult::Error
        }
        Err(e) => {
            let msg = format!("cd: {}: {:?}", target, e);
            ctx.error(&msg);
            CommandResult::Error
        }
    }
}

// ============================================================================
// pwd – print working directory
// ============================================================================

pub fn cmd_pwd(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    ctx.println_colored(get_current_dir(), Theme::PROMPT_PATH);
    CommandResult::Ok
}

// ============================================================================
// cat – display file contents
// ============================================================================

pub fn cmd_cat(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let Some(filename) = cmd.arg(0) else {
        ctx.error("Usage: cat <file> [file2 ...]");
        return CommandResult::Error;
    };

    // Support multiple files
    let mut i = 0;
    loop {
        let Some(fname) = cmd.arg(i) else { break; };
        let path = resolve_path(fname);

        match fs_read_file(&path) {
            Ok(buffer) => {
                if cmd.arg_count > 1 {
                    // Print header for multiple files
                    let hdr = format!("==> {} <==", fname);
                    ctx.println_colored(&hdr, Theme::TEXT_INFO);
                }
                let content = unsafe { core::str::from_utf8_unchecked(&buffer) };
                for line in content.split('\n') { ctx.println(line); }
            }
            Err(e) => {
                let msg = format!("cat: {}: {:?}", fname, e);
                ctx.error(&msg);
            }
        }
        i += 1;
    }
    let _ = filename; // used above via cmd.arg(0)
    CommandResult::Ok
}

// ============================================================================
// head – show first N lines of file
// ============================================================================

pub fn cmd_head(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let n: usize = cmd.get_option("-n", "--lines")
        .and_then(|s| {
            let mut v = 0usize;
            for c in s.bytes() {
                if c >= b'0' && c <= b'9' { v = v * 10 + (c - b'0') as usize; } else { return None; }
            }
            Some(v)
        })
        .unwrap_or(10);

    let Some(fname) = first_path_arg(cmd) else {
        ctx.error("Usage: head [-n N] <file>");
        return CommandResult::Error;
    };
    let path = resolve_path(fname);
    match fs_read_file(&path) {
        Ok(buf) => {
            let content = unsafe { core::str::from_utf8_unchecked(&buf) };
            for (i, line) in content.split('\n').enumerate() {
                if i >= n { break; }
                ctx.println(line);
            }
        }
        Err(e) => {
            let msg = format!("head: {}: {:?}", fname, e);
            ctx.error(&msg);
            return CommandResult::Error;
        }
    }
    CommandResult::Ok
}

// ============================================================================
// tail – show last N lines of file
// ============================================================================

pub fn cmd_tail(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let n: usize = cmd.get_option("-n", "--lines")
        .and_then(|s| {
            let mut v = 0usize;
            for c in s.bytes() {
                if c >= b'0' && c <= b'9' { v = v * 10 + (c - b'0') as usize; } else { return None; }
            }
            Some(v)
        })
        .unwrap_or(10);

    let Some(fname) = first_path_arg(cmd) else {
        ctx.error("Usage: tail [-n N] <file>");
        return CommandResult::Error;
    };
    let path = resolve_path(fname);
    match fs_read_file(&path) {
        Ok(buf) => {
            let content = unsafe { core::str::from_utf8_unchecked(&buf) };
            let lines: Vec<&str> = content.split('\n').collect();
            let start = if lines.len() > n { lines.len() - n } else { 0 };
            for line in &lines[start..] { ctx.println(line); }
        }
        Err(e) => {
            let msg = format!("tail: {}: {:?}", fname, e);
            ctx.error(&msg);
            return CommandResult::Error;
        }
    }
    CommandResult::Ok
}

// ============================================================================
// wc – word / line / byte count
// ============================================================================

pub fn cmd_wc(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let show_lines = cmd.has_flag("-l", "--lines");
    let show_words = cmd.has_flag("-w", "--words");
    let show_bytes = cmd.has_flag("-c", "--bytes");
    let all        = !show_lines && !show_words && !show_bytes;

    let Some(fname) = first_path_arg(cmd) else {
        ctx.error("Usage: wc [-l] [-w] [-c] <file>");
        return CommandResult::Error;
    };
    let path = resolve_path(fname);
    match fs_read_file(&path) {
        Ok(buf) => {
            let content = unsafe { core::str::from_utf8_unchecked(&buf) };
            let lines = content.split('\n').count();
            let words = content.split_whitespace().count();
            let bytes = buf.len();

            let mut out = String::new();
            if all || show_lines { out.push_str(&format!("{:>8} ", lines)); }
            if all || show_words { out.push_str(&format!("{:>8} ", words)); }
            if all || show_bytes { out.push_str(&format!("{:>8} ", bytes)); }
            out.push_str(fname);
            ctx.println(&out);
        }
        Err(e) => {
            let msg = format!("wc: {}: {:?}", fname, e);
            ctx.error(&msg);
            return CommandResult::Error;
        }
    }
    CommandResult::Ok
}

// ============================================================================
// mkdir – create directory
// ============================================================================

pub fn cmd_mkdir(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let make_parents = cmd.has_flag("-p", "--parents");

    let Some(arg) = first_path_arg(cmd) else {
        ctx.error("Usage: mkdir [-p] <directory>");
        return CommandResult::Error;
    };
    let path = resolve_path(arg);

    if make_parents {
        // Create each component
        let mut cumulative = String::from("/");
        for seg in path.trim_start_matches('/').split('/') {
            if seg.is_empty() { continue; }
            if cumulative != "/" { cumulative.push('/'); }
            for b in seg.bytes() { cumulative.push(b as char); }
            let _ = atom_syscall::fs::mkdir(&cumulative, 0o755);
        }
        let msg = format!("mkdir: created '{}'", arg);
        ctx.success(&msg);
    } else {
        match atom_syscall::fs::mkdir(&path, 0o755) {
            Ok(()) => {
                let msg = format!("mkdir: created directory '{}'", arg);
                ctx.success(&msg);
            }
            Err(e) => {
                let msg = format!("mkdir: cannot create '{}': {:?}", arg, e);
                ctx.error(&msg);
                return CommandResult::Error;
            }
        }
    }
    CommandResult::Ok
}

// ============================================================================
// rmdir – remove empty directory
// ============================================================================

pub fn cmd_rmdir(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let Some(arg) = first_path_arg(cmd) else {
        ctx.error("Usage: rmdir <directory>");
        return CommandResult::Error;
    };
    let path = resolve_path(arg);
    match atom_syscall::fs::rmdir(&path) {
        Ok(()) => {
            let msg = format!("rmdir: removed '{}'", arg);
            ctx.success(&msg);
        }
        Err(e) => {
            let msg = format!("rmdir: failed to remove '{}': {:?}", arg, e);
            ctx.error(&msg);
            return CommandResult::Error;
        }
    }
    CommandResult::Ok
}

// ============================================================================
// rm – remove files (and directories with -r)
// ============================================================================

pub fn cmd_rm(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let recursive = cmd.has_flag("-r", "--recursive")
        || cmd.has_flag("-R", "--recursive")
        || cmd.has_flag("-rf", "");
    let force     = cmd.has_flag("-f", "--force");

    let Some(arg) = first_path_arg(cmd) else {
        ctx.error("Usage: rm [-r] [-f] <file>");
        return CommandResult::Error;
    };
    let path = resolve_path(arg);

    if recursive {
        if let Err(e) = rm_recursive(&path) {
            if !force {
                let msg = format!("rm: cannot remove '{}': {:?}", arg, e);
                ctx.error(&msg);
                return CommandResult::Error;
            }
        } else {
            let msg = format!("rm: removed '{}'", arg);
            ctx.success(&msg);
        }
    } else {
        match atom_syscall::fs::unlink(&path) {
            Ok(()) => {
                let msg = format!("rm: removed '{}'", arg);
                ctx.success(&msg);
            }
            Err(atom_syscall::fs::FsError::IsDir) => {
                ctx.error("rm: is a directory (use -r to remove)");
                return CommandResult::Error;
            }
            Err(e) => {
                if !force {
                    let msg = format!("rm: cannot remove '{}': {:?}", arg, e);
                    ctx.error(&msg);
                    return CommandResult::Error;
                }
            }
        }
    }
    CommandResult::Ok
}

fn rm_recursive(path: &str) -> core::result::Result<(), atom_syscall::fs::FsError> {
    // Try to stat first
    let stat = atom_syscall::fs::stat(path)?;
    if stat.is_dir() {
        // Read directory entries
        let flags = atom_syscall::fs::OpenFlags::RDONLY
            | atom_syscall::fs::OpenFlags::DIRECTORY;
        let fd = atom_syscall::fs::open(path, flags, 0)?;
        let entries = atom_syscall::fs::readdir(fd)?;
        let _ = atom_syscall::fs::close(fd);

        for entry in entries {
            if entry.name == "." || entry.name == ".." { continue; }
            let child = if path.ends_with('/') {
                format!("{}{}", path, entry.name)
            } else {
                format!("{}/{}", path, entry.name)
            };
            rm_recursive(&child)?;
        }
        atom_syscall::fs::rmdir(path)
    } else {
        atom_syscall::fs::unlink(path)
    }
}

// ============================================================================
// mv – move / rename
// ============================================================================

pub fn cmd_mv(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let (Some(src_arg), Some(dst_arg)) = (cmd.arg(0), cmd.arg(1)) else {
        ctx.error("Usage: mv <source> <destination>");
        return CommandResult::Error;
    };
    let src = resolve_path(src_arg);
    let dst = resolve_path(dst_arg);

    match atom_syscall::fs::rename(&src, &dst) {
        Ok(()) => {
            let msg = format!("mv: '{}' -> '{}'", src_arg, dst_arg);
            ctx.success(&msg);
        }
        Err(e) => {
            let msg = format!("mv: cannot move '{}': {:?}", src_arg, e);
            ctx.error(&msg);
            return CommandResult::Error;
        }
    }
    CommandResult::Ok
}

// ============================================================================
// cp – copy file
// ============================================================================

pub fn cmd_cp(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let (Some(src_arg), Some(dst_arg)) = (cmd.arg(0), cmd.arg(1)) else {
        ctx.error("Usage: cp <source> <destination>");
        return CommandResult::Error;
    };
    let src = resolve_path(src_arg);
    let dst = resolve_path(dst_arg);

    match copy_file(&src, &dst) {
        Ok(()) => {
            let msg = format!("cp: '{}' -> '{}'", src_arg, dst_arg);
            ctx.success(&msg);
        }
        Err(e) => {
            let msg = format!("cp: cannot copy '{}': {:?}", src_arg, e);
            ctx.error(&msg);
            return CommandResult::Error;
        }
    }
    CommandResult::Ok
}

fn copy_file(src: &str, dst: &str) -> core::result::Result<(), atom_syscall::fs::FsError> {
    let src_fd = atom_syscall::fs::open(src,
        atom_syscall::fs::OpenFlags::RDONLY, 0)?;
    let dst_fd = atom_syscall::fs::open(dst,
        atom_syscall::fs::OpenFlags::CREAT
        | atom_syscall::fs::OpenFlags::TRUNC
        | atom_syscall::fs::OpenFlags::WRONLY, 0o644)?;

    let mut buf = [0u8; 4096];
    loop {
        let n = atom_syscall::fs::read(src_fd, &mut buf)?;
        if n == 0 { break; }
        atom_syscall::fs::write_all(dst_fd, &buf[..n])?;
    }
    let _ = atom_syscall::fs::close(src_fd);
    let _ = atom_syscall::fs::close(dst_fd);
    Ok(())
}

// ============================================================================
// touch – create file or update timestamp
// ============================================================================

pub fn cmd_touch(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let Some(arg) = first_path_arg(cmd) else {
        ctx.error("Usage: touch <file>");
        return CommandResult::Error;
    };
    let path = resolve_path(arg);

    // Try to open for write (creates if not exists)
    let flags = atom_syscall::fs::OpenFlags::CREAT | atom_syscall::fs::OpenFlags::WRONLY;
    match atom_syscall::fs::open(&path, flags, 0o644) {
        Ok(fd) => {
            let _ = atom_syscall::fs::close(fd);
            let msg = format!("touch: '{}'", arg);
            ctx.success(&msg);
        }
        Err(e) => {
            let msg = format!("touch: cannot touch '{}': {:?}", arg, e);
            ctx.error(&msg);
            return CommandResult::Error;
        }
    }
    CommandResult::Ok
}

// ============================================================================
// fsync – force file durability
// ============================================================================

pub fn cmd_fsync(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let Some(arg) = first_path_arg(cmd) else {
        ctx.error("Usage: fsync <file>");
        return CommandResult::Error;
    };
    let path = resolve_path(arg);

    let fd = match atom_syscall::fs::open(&path, atom_syscall::fs::OpenFlags::RDONLY, 0) {
        Ok(fd) => fd,
        Err(atom_syscall::fs::FsError::IsDir) => {
            let flags = atom_syscall::fs::OpenFlags::RDONLY | atom_syscall::fs::OpenFlags::DIRECTORY;
            match atom_syscall::fs::open(&path, flags, 0) {
                Ok(fd) => fd,
                Err(e) => {
                    let msg = format!("fsync: cannot open '{}': {:?}", arg, e);
                    ctx.error(&msg);
                    return CommandResult::Error;
                }
            }
        }
        Err(e) => {
            let msg = format!("fsync: cannot open '{}': {:?}", arg, e);
            ctx.error(&msg);
            return CommandResult::Error;
        }
    };

    let sync_result = atom_syscall::fs::fsync(fd);
    let _ = atom_syscall::fs::close(fd);

    match sync_result {
        Ok(()) => {
            let msg = format!("fsync: '{}': ok", arg);
            ctx.success(&msg);
            CommandResult::Ok
        }
        Err(e) => {
            let msg = format!("fsync: '{}': {:?}", arg, e);
            ctx.error(&msg);
            CommandResult::Error
        }
    }
}

// ============================================================================
// stat – file status / metadata
// ============================================================================

pub fn cmd_stat(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let Some(arg) = first_path_arg(cmd) else {
        ctx.error("Usage: stat <file>");
        return CommandResult::Error;
    };
    let path = resolve_path(arg);

    match atom_syscall::fs::stat(&path) {
        Ok(s) => {
            ctx.println("");
            let line = format!("  File: {}", arg);
            ctx.println_colored(&line, Theme::TEXT_INFO);
            let sz_line = format!("  Size: {}  ({})", s.size, fmt_size(s.size));
            ctx.println(&sz_line);
            let type_str = if s.is_dir() { "directory" }
                else if s.is_file() { "regular file" }
                else { "other" };
            let type_line = format!("  Type: {}", type_str);
            ctx.println(&type_line);
            let ext = file_extension(arg);
            if !ext.is_empty() {
                let ext_line = format!("  Ext:  .{}", ext);
                ctx.println(&ext_line);
            }
            let mode_line = format!("  Mode: {:04o}", s.mode & 0o7777);
            ctx.println(&mode_line);
            let inode_line = format!("  Inode: {}", s.inode);
            ctx.println(&inode_line);
            ctx.println("");
        }
        Err(e) => {
            let msg = format!("stat: {}: {:?}", arg, e);
            ctx.error(&msg);
            return CommandResult::Error;
        }
    }
    CommandResult::Ok
}

// ============================================================================
// chmod – change permissions (simulated for now)
// ============================================================================

pub fn cmd_chmod(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let (Some(mode_arg), Some(path_arg)) = (cmd.arg(0), cmd.arg(1)) else {
        ctx.error("Usage: chmod <mode> <file>");
        ctx.info("  Examples: chmod 755 script.sh  |  chmod 644 readme.txt");
        return CommandResult::Error;
    };

    // Parse octal mode
    let mut mode = 0u32;
    for c in mode_arg.bytes() {
        if c >= b'0' && c <= b'7' { mode = mode * 8 + (c - b'0') as u32; }
        else {
            ctx.error("chmod: mode must be an octal number (e.g. 755)");
            return CommandResult::Error;
        }
    }

    let path = resolve_path(path_arg);
    match atom_syscall::fs::chmod(&path, mode) {
        Ok(()) => {
            let msg = format!("chmod: {} -> {:04o}", path_arg, mode);
            ctx.success(&msg);
        }
        Err(e) => {
            let msg = format!("chmod: {}: {:?}", path_arg, e);
            ctx.error(&msg);
            return CommandResult::Error;
        }
    }
    CommandResult::Ok
}

// ============================================================================
// ln – create hard or symbolic link
// ============================================================================

pub fn cmd_ln(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let symbolic = cmd.has_flag("-s", "--symbolic");
    let pos = positional_args_vec(cmd);
    let (Some(target_arg), Some(link_arg)) = (pos.get(0).copied(), pos.get(1).copied()) else {
        ctx.error("Usage: ln [-s] <target> <link_name>");
        return CommandResult::Error;
    };
    let target = resolve_path(target_arg);
    let link   = resolve_path(link_arg);

    let result = if symbolic {
        atom_syscall::fs::symlink(&target, &link)
    } else {
        atom_syscall::fs::link(&target, &link)
    };

    match result {
        Ok(()) => {
            let kind = if symbolic { "symlink" } else { "link" };
            let msg = format!("ln: {} '{}' -> '{}'", kind, link_arg, target_arg);
            ctx.success(&msg);
        }
        Err(e) => {
            let msg = format!("ln: cannot create link: {:?}", e);
            ctx.error(&msg);
            return CommandResult::Error;
        }
    }
    CommandResult::Ok
}

// ============================================================================
// find – find files by name pattern
// ============================================================================

pub fn cmd_find(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let root    = first_path_arg(cmd).unwrap_or(get_current_dir());
    let pattern = cmd.get_option("-name", "-name").unwrap_or("*");
    let max_depth = cmd.get_option("-maxdepth", "--maxdepth")
        .and_then(|s| {
            let mut v = 0u32;
            for c in s.bytes() { if c >= b'0' && c <= b'9' { v = v*10+(c-b'0')as u32; } else { return None; } }
            Some(v)
        })
        .unwrap_or(u32::MAX);

    ctx.println("");
    let root_path = resolve_path(root);
    find_recursive(&root_path, pattern, 0, max_depth, ctx);
    ctx.println("");
    CommandResult::Ok
}

fn find_recursive(dir: &str, pattern: &str, depth: u32, max: u32,
                  ctx: &mut CommandContext<'_>) {
    if depth > max { return; }
    let flags = atom_syscall::fs::OpenFlags::RDONLY
        | atom_syscall::fs::OpenFlags::DIRECTORY;
    let Ok(fd) = atom_syscall::fs::open(dir, flags, 0) else { return };
    let Ok(entries) = atom_syscall::fs::readdir(fd) else {
        let _ = atom_syscall::fs::close(fd); return;
    };
    let _ = atom_syscall::fs::close(fd);

    for entry in entries {
        if entry.name == "." || entry.name == ".." { continue; }
        let child = if dir.ends_with('/') {
            format!("{}{}", dir, entry.name)
        } else {
            format!("{}/{}", dir, entry.name)
        };
        if matches_glob(&entry.name, pattern) {
            ctx.println(&child);
        }
        if entry.file_type == atom_syscall::fs::FileType::Directory {
            find_recursive(&child, pattern, depth + 1, max, ctx);
        }
    }
}

fn matches_glob(name: &str, pattern: &str) -> bool {
    if pattern == "*" { return true; }
    if pattern.starts_with('*') && pattern.ends_with('*') {
        let mid = &pattern[1..pattern.len()-1];
        return name.contains(mid);
    }
    if pattern.starts_with('*') {
        let suffix = &pattern[1..];
        return name.ends_with(suffix);
    }
    if pattern.ends_with('*') {
        let prefix = &pattern[..pattern.len()-1];
        return name.starts_with(prefix);
    }
    name == pattern
}

// ============================================================================
// df – disk free (simulated)
// ============================================================================

pub fn cmd_df(_cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    ctx.println("");
    ctx.println_colored("Filesystem      Size     Used    Avail  Use%  Mounted on",
        Theme::TEXT_DIM);
    ctx.println_colored("─────────────────────────────────────────────────────────",
        Theme::TEXT_DIM);
    ctx.println("/dev/sda1       512 MB   128 MB   384 MB  25%   /");
    ctx.println("tmpfs            64 MB     2 MB    62 MB   3%   /tmp");
    ctx.println("");
    ctx.info("(simulated – real values pending disk service)");
    ctx.println("");
    CommandResult::Ok
}

// ============================================================================
// du – disk usage (simulated / recursive size)
// ============================================================================

pub fn cmd_du(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let summary  = cmd.has_flag("-s", "--summarize");
    let human    = cmd.has_flag("-h", "--human-readable");
    let path_arg = first_path_arg(cmd).unwrap_or(get_current_dir());
    let path     = resolve_path(path_arg);

    ctx.println("");
    let total = du_recursive(&path, summary, human, ctx);
    if summary {
        let s = if human { fmt_size(total) } else { format!("{}", total / 1024) };
        let line = format!("{:<8} {}", s, path_arg);
        ctx.println(&line);
    }
    ctx.println("");
    CommandResult::Ok
}

fn du_recursive(path: &str, summary: bool, human: bool,
                ctx: &mut CommandContext<'_>) -> u64 {
    let flags = atom_syscall::fs::OpenFlags::RDONLY
        | atom_syscall::fs::OpenFlags::DIRECTORY;
    let Ok(fd) = atom_syscall::fs::open(path, flags, 0) else {
        // It's a file
        if let Ok(s) = atom_syscall::fs::stat(path) { return s.size; }
        return 0;
    };
    let Ok(entries) = atom_syscall::fs::readdir(fd) else {
        let _ = atom_syscall::fs::close(fd); return 0;
    };
    let _ = atom_syscall::fs::close(fd);

    let mut total = 0u64;
    for entry in entries {
        if entry.name == "." || entry.name == ".." { continue; }
        let child = if path.ends_with('/') {
            format!("{}{}", path, entry.name)
        } else {
            format!("{}/{}", path, entry.name)
        };
        let size = du_recursive(&child, summary, human, ctx);
        total += size;
        if !summary {
            let s = if human { fmt_size(size) } else { format!("{}", size / 1024) };
            let line = format!("{:<8} {}", s, child);
            ctx.println(&line);
        }
    }
    total
}

// ============================================================================
// tree – directory tree view
// ============================================================================

pub fn cmd_tree(cmd: &ParsedCommand<'_>, ctx: &mut CommandContext<'_>) -> CommandResult {
    let path = cmd.arg(0).map(|p| resolve_path(p))
                  .unwrap_or_else(|| String::from(get_current_dir()));
    let max_depth = cmd.get_option("-d", "--depth")
        .and_then(|s| {
            let mut v = 0u32;
            for c in s.bytes() { if c >= b'0' && c <= b'9' { v = v*10+(c-b'0') as u32; } else { return None; } }
            Some(v)
        })
        .unwrap_or(3u32);

    ctx.println("");
    ctx.println_colored(&path, Theme::PROMPT_PATH);
    tree_recursive(&path, "", 0, max_depth, ctx);
    ctx.println("");
    CommandResult::Ok
}

fn tree_recursive(dir: &str, prefix: &str, depth: u32, max: u32,
                  ctx: &mut CommandContext<'_>) {
    if depth >= max { return; }
    let flags = atom_syscall::fs::OpenFlags::RDONLY
        | atom_syscall::fs::OpenFlags::DIRECTORY;
    let Ok(fd) = atom_syscall::fs::open(dir, flags, 0) else { return };
    let Ok(entries) = atom_syscall::fs::readdir(fd) else {
        let _ = atom_syscall::fs::close(fd); return;
    };
    let _ = atom_syscall::fs::close(fd);

    // Filter out . and ..
    let real: Vec<_> = entries.iter()
        .filter(|e| e.name != "." && e.name != "..")
        .collect();

    for (i, entry) in real.iter().enumerate() {
        let last  = i + 1 == real.len();
        let connector = if last { "`-- " } else { "|-- " };
        let line = format!("{}{}{}", prefix, connector, entry.name);
        if entry.file_type == atom_syscall::fs::FileType::Directory {
            ctx.println_colored(&line, Theme::PROMPT_PATH);
            let child = if dir.ends_with('/') {
                format!("{}{}", dir, entry.name)
            } else {
                format!("{}/{}", dir, entry.name)
            };
            let new_prefix = if last {
                format!("{}    ", prefix)
            } else {
                format!("{}|   ", prefix)
            };
            tree_recursive(&child, &new_prefix, depth + 1, max, ctx);
        } else {
            ctx.println(&line);
        }
    }
}
