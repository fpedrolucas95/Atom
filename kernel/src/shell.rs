// Embedded Userspace Shell
//
// This module provides a minimal embedded shell that runs in Ring 3 (userspace).
// It uses syscalls to communicate with the kernel for:
// - Getting framebuffer information
// - Polling keyboard and mouse input
// - Yielding to other threads
//
// The shell is copied to user-accessible pages at runtime and executed
// with a proper Ring 3 context (CS=0x1B, SS=0x23).
//
// Future: This will be replaced by loading ui_shell.atxf from userspace/drivers/

use crate::syscall::{
    ESUCCESS, EWOULDBLOCK,
    SYS_THREAD_YIELD, SYS_GET_FRAMEBUFFER, SYS_KEYBOARD_POLL, SYS_MOUSE_POLL,
};

/// Entry point for the embedded userspace shell (Ring 3)
/// This function is designed to run in Ring 3 using syscalls
pub extern "C" fn shell_entry() -> ! {
    // Get framebuffer via syscall
    let mut fb_info = [0u64; 5];
    let fb_result = unsafe { syscall1(SYS_GET_FRAMEBUFFER, fb_info.as_mut_ptr() as u64) };
    
    if fb_result != ESUCCESS {
        loop { unsafe { syscall0(SYS_THREAD_YIELD); } }
    }
    
    let fb_addr = fb_info[0] as usize;
    let width = fb_info[1] as u32;
    let height = fb_info[2] as u32;
    let stride = fb_info[3] as u32;
    let bpp = fb_info[4] as usize;
    
    // Clear screen to dark background
    fill_rect(fb_addr, stride, bpp, 0, 0, width, height, 0x002E3440);
    
    // Draw title bar
    fill_rect(fb_addr, stride, bpp, 0, 0, width, 32, 0x00242933);
    draw_string(fb_addr, stride, bpp, 16, 8, "Atom OS - Userspace Shell", 0x0088C0D0);
    
    // Draw status
    draw_string(fb_addr, stride, bpp, 16, 50, "Running in Ring 3 (User Mode)", 0x00A3BE8C);
    draw_string(fb_addr, stride, bpp, 16, 70, "Drivers loaded from userspace", 0x00ECEFF4);
    
    // Cursor state
    let mut cursor_x = width / 2;
    let mut cursor_y = height / 2;

    // Saved region under cursor (12x16 pixels = 192 pixels max)
    const CURSOR_WIDTH: u32 = 12;
    const CURSOR_HEIGHT: u32 = 16;
    let mut saved_region: [u32; 192] = [0; 192];
    let mut saved_x = cursor_x;
    let mut saved_y = cursor_y;

    // Save initial region and draw cursor
    save_cursor_region(fb_addr, stride, bpp, cursor_x, cursor_y, CURSOR_WIDTH, CURSOR_HEIGHT, &mut saved_region);
    draw_cursor(fb_addr, stride, bpp, cursor_x, cursor_y);

    // Mouse packet state
    let mut mouse_cycle = 0u8;
    let mut mouse_packet = [0u8; 3];

    loop {
        // Poll keyboard
        loop {
            let scancode = unsafe { syscall0(SYS_KEYBOARD_POLL) };
            if scancode == EWOULDBLOCK { break; }
            
            // ESC to halt
            if scancode == 0x01 {
                loop { unsafe { syscall0(SYS_THREAD_YIELD); } }
            }
        }
        
        // Poll mouse (raw bytes) - accumulate all deltas first, then update cursor once
        let mut total_dx: i32 = 0;
        let mut total_dy: i32 = 0;
        let mut mouse_moved = false;

        loop {
            let byte_result = unsafe { syscall0(SYS_MOUSE_POLL) };
            if byte_result == EWOULDBLOCK { break; }

            let byte = byte_result as u8;

            match mouse_cycle {
                0 => {
                    if byte & 0x08 != 0 {
                        mouse_packet[0] = byte;
                        mouse_cycle = 1;
                    }
                }
                1 => {
                    mouse_packet[1] = byte;
                    mouse_cycle = 2;
                }
                2 => {
                    mouse_packet[2] = byte;
                    mouse_cycle = 0;

                    let flags = mouse_packet[0];
                    if flags & 0xC0 != 0 { continue; }

                    let mut dx = mouse_packet[1] as i32;
                    if flags & 0x10 != 0 { dx -= 256; }

                    let mut dy = mouse_packet[2] as i32;
                    if flags & 0x20 != 0 { dy -= 256; }

                    total_dx += dx;
                    total_dy += dy;
                    mouse_moved = true;
                }
                _ => mouse_cycle = 0,
            }
        }

        if mouse_moved {
            restore_cursor_region(fb_addr, stride, bpp, saved_x, saved_y, CURSOR_WIDTH, CURSOR_HEIGHT, &saved_region);

            let new_x = (cursor_x as i32 + total_dx).clamp(0, (width - CURSOR_WIDTH) as i32) as u32;
            let new_y = (cursor_y as i32 - total_dy).clamp(0, (height - CURSOR_HEIGHT) as i32) as u32;

            cursor_x = new_x;
            cursor_y = new_y;

            save_cursor_region(fb_addr, stride, bpp, cursor_x, cursor_y, CURSOR_WIDTH, CURSOR_HEIGHT, &mut saved_region);
            saved_x = cursor_x;
            saved_y = cursor_y;

            draw_cursor(fb_addr, stride, bpp, cursor_x, cursor_y);

            // Memory fence to ensure framebuffer writes are visible
            core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
        }
        
        // Yield
        unsafe { syscall0(SYS_THREAD_YIELD); }
    }
}

fn save_cursor_region(
    fb: usize,
    stride: u32,
    bpp: usize,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    buffer: &mut [u32; 192],
) {
    for row in 0..h {
        for col in 0..w {
            let offset = ((y + row) * stride + (x + col)) as usize * bpp;
            let idx = (row * w + col) as usize;
            if idx < 192 {
                unsafe {
                    let ptr = (fb + offset) as *const u32;
                    buffer[idx] = ptr.read_volatile();
                }
            }
        }
    }
}

fn restore_cursor_region(
    fb: usize,
    stride: u32,
    bpp: usize,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    buffer: &[u32; 192],
) {
    for row in 0..h {
        for col in 0..w {
            let offset = ((y + row) * stride + (x + col)) as usize * bpp;
            let idx = (row * w + col) as usize;
            if idx < 192 {
                unsafe {
                    let ptr = (fb + offset) as *mut u32;
                    ptr.write_volatile(buffer[idx]);
                }
            }
        }
    }
}

fn fill_rect(fb: usize, stride: u32, bpp: usize, x: u32, y: u32, w: u32, h: u32, color: u32) {
    for row in 0..h {
        for col in 0..w {
            let offset = ((y + row) * stride + (x + col)) as usize * bpp;
            unsafe {
                let ptr = (fb + offset) as *mut u32;
                ptr.write_volatile(color);
            }
        }
    }
}

fn draw_cursor(fb: usize, stride: u32, bpp: usize, x: u32, y: u32) {
    let cursor = [
        [1,0,0,0,0,0,0,0,0,0],
        [1,1,0,0,0,0,0,0,0,0],
        [1,2,1,0,0,0,0,0,0,0],
        [1,2,2,1,0,0,0,0,0,0],
        [1,2,2,2,1,0,0,0,0,0],
        [1,2,2,2,2,1,0,0,0,0],
        [1,2,2,2,2,2,1,0,0,0],
        [1,2,2,2,2,2,2,1,0,0],
        [1,2,2,2,2,2,2,2,1,0],
        [1,2,2,2,2,2,2,2,2,1],
        [1,2,2,2,2,1,1,1,1,1],
        [1,2,1,2,1,0,0,0,0,0],
        [1,1,0,1,2,1,0,0,0,0],
        [0,0,0,1,2,1,0,0,0,0],
        [0,0,0,0,1,2,1,0,0,0],
        [0,0,0,0,1,1,0,0,0,0],
    ];
    
    for (row, cols) in cursor.iter().enumerate() {
        for (col, &px) in cols.iter().enumerate() {
            let color = match px {
                1 => 0x00000000, // Black outline
                2 => 0x00FFFFFF, // White fill
                _ => continue,
            };
            let offset = ((y + row as u32) * stride + (x + col as u32)) as usize * bpp;
            unsafe {
                let ptr = (fb + offset) as *mut u32;
                ptr.write_volatile(color);
            }
        }
    }
}

fn draw_string(fb: usize, stride: u32, bpp: usize, x: u32, y: u32, s: &str, color: u32) {
    let mut cx = x;
    for ch in s.bytes() {
        draw_char(fb, stride, bpp, cx, y, ch, color);
        cx += 8;
    }
}

fn draw_char(fb: usize, stride: u32, bpp: usize, x: u32, y: u32, ch: u8, color: u32) {
    let glyph = get_glyph(ch);
    for row in 0..8 {
        let bits = glyph[row];
        for col in 0..8 {
            if bits & (0x80 >> col) != 0 {
                let offset = ((y + row as u32) * stride + (x + col as u32)) as usize * bpp;
                unsafe {
                    let ptr = (fb + offset) as *mut u32;
                    ptr.write_volatile(color);
                }
            }
        }
    }
}

fn get_glyph(ch: u8) -> [u8; 8] {
    // Minimal 8x8 font (ASCII 32-127)
    match ch {
        b' ' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
        b'A' => [0x18, 0x3C, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x00],
        b'B' => [0x7C, 0x66, 0x66, 0x7C, 0x66, 0x66, 0x7C, 0x00],
        b'C' => [0x3C, 0x66, 0x60, 0x60, 0x60, 0x66, 0x3C, 0x00],
        b'D' => [0x78, 0x6C, 0x66, 0x66, 0x66, 0x6C, 0x78, 0x00],
        b'E' => [0x7E, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x7E, 0x00],
        b'F' => [0x7E, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x60, 0x00],
        b'G' => [0x3C, 0x66, 0x60, 0x6E, 0x66, 0x66, 0x3C, 0x00],
        b'H' => [0x66, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x66, 0x00],
        b'I' => [0x3C, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0x00],
        b'L' => [0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x7E, 0x00],
        b'M' => [0xC6, 0xEE, 0xFE, 0xD6, 0xC6, 0xC6, 0xC6, 0x00],
        b'N' => [0x66, 0x76, 0x7E, 0x7E, 0x6E, 0x66, 0x66, 0x00],
        b'O' => [0x3C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00],
        b'P' => [0x7C, 0x66, 0x66, 0x7C, 0x60, 0x60, 0x60, 0x00],
        b'R' => [0x7C, 0x66, 0x66, 0x7C, 0x6C, 0x66, 0x66, 0x00],
        b'S' => [0x3C, 0x66, 0x60, 0x3C, 0x06, 0x66, 0x3C, 0x00],
        b'T' => [0x7E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00],
        b'U' => [0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00],
        b'V' => [0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x18, 0x00],
        b'W' => [0xC6, 0xC6, 0xC6, 0xD6, 0xFE, 0xEE, 0xC6, 0x00],
        b'a' => [0x00, 0x00, 0x3C, 0x06, 0x3E, 0x66, 0x3E, 0x00],
        b'c' => [0x00, 0x00, 0x3C, 0x60, 0x60, 0x60, 0x3C, 0x00],
        b'd' => [0x06, 0x06, 0x3E, 0x66, 0x66, 0x66, 0x3E, 0x00],
        b'e' => [0x00, 0x00, 0x3C, 0x66, 0x7E, 0x60, 0x3C, 0x00],
        b'f' => [0x1C, 0x30, 0x7C, 0x30, 0x30, 0x30, 0x30, 0x00],
        b'g' => [0x00, 0x00, 0x3E, 0x66, 0x66, 0x3E, 0x06, 0x3C],
        b'i' => [0x18, 0x00, 0x38, 0x18, 0x18, 0x18, 0x3C, 0x00],
        b'l' => [0x38, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0x00],
        b'm' => [0x00, 0x00, 0x6C, 0xFE, 0xD6, 0xC6, 0xC6, 0x00],
        b'n' => [0x00, 0x00, 0x7C, 0x66, 0x66, 0x66, 0x66, 0x00],
        b'o' => [0x00, 0x00, 0x3C, 0x66, 0x66, 0x66, 0x3C, 0x00],
        b'p' => [0x00, 0x00, 0x7C, 0x66, 0x66, 0x7C, 0x60, 0x60],
        b'r' => [0x00, 0x00, 0x6E, 0x70, 0x60, 0x60, 0x60, 0x00],
        b's' => [0x00, 0x00, 0x3E, 0x60, 0x3C, 0x06, 0x7C, 0x00],
        b't' => [0x30, 0x30, 0x7C, 0x30, 0x30, 0x30, 0x1C, 0x00],
        b'u' => [0x00, 0x00, 0x66, 0x66, 0x66, 0x66, 0x3E, 0x00],
        b'v' => [0x00, 0x00, 0x66, 0x66, 0x66, 0x3C, 0x18, 0x00],
        b'y' => [0x00, 0x00, 0x66, 0x66, 0x66, 0x3E, 0x06, 0x3C],
        b'-' => [0x00, 0x00, 0x00, 0x7E, 0x00, 0x00, 0x00, 0x00],
        b'(' => [0x0C, 0x18, 0x30, 0x30, 0x30, 0x18, 0x0C, 0x00],
        b')' => [0x30, 0x18, 0x0C, 0x0C, 0x0C, 0x18, 0x30, 0x00],
        b'0' => [0x3C, 0x66, 0x6E, 0x76, 0x66, 0x66, 0x3C, 0x00],
        b'1' => [0x18, 0x38, 0x18, 0x18, 0x18, 0x18, 0x7E, 0x00],
        b'2' => [0x3C, 0x66, 0x06, 0x1C, 0x30, 0x60, 0x7E, 0x00],
        b'3' => [0x3C, 0x66, 0x06, 0x1C, 0x06, 0x66, 0x3C, 0x00],
        _ => [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    }
}

// Syscall helpers
#[inline(always)]
unsafe fn syscall0(num: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    result
}

#[inline(always)]
unsafe fn syscall1(num: u64, arg0: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        in("rdi") arg0,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    result
}

#[inline(always)]
unsafe fn syscall2(num: u64, arg0: u64, arg1: u64) -> u64 {
    let result: u64;
    core::arch::asm!(
        "syscall",
        inlateout("rax") num => result,
        in("rdi") arg0,
        in("rsi") arg1,
        out("rcx") _,
        out("r11") _,
        options(nostack, preserves_flags)
    );
    result
}
