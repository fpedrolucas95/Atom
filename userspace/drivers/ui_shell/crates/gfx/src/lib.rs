//! Graphics primitives and utility functions

#![no_std]

use atom_syscall::graphics::{Color, Framebuffer, SharedSurface};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self { x, y, width, height }
    }

    pub fn intersects(&self, other: &Rect) -> bool {
        self.x < other.x + other.width as i32 &&
        self.x + self.width as i32 > other.x &&
        self.y < other.y + other.height as i32 &&
        self.y + self.height as i32 > other.y
    }

    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        let x1 = self.x.max(other.x);
        let y1 = self.y.max(other.y);
        let x2 = (self.x + self.width as i32).min(other.x + other.width as i32);
        let y2 = (self.y + self.height as i32).min(other.y + other.height as i32);

        if x1 < x2 && y1 < y2 {
            Some(Rect::new(x1, y1, (x2 - x1) as u32, (y2 - y1) as u32))
        } else {
            None
        }
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && x < self.x + self.width as i32 &&
        y >= self.y && y < self.y + self.height as i32
    }

    pub fn is_empty(&self) -> bool {
        self.width == 0 || self.height == 0
    }
}

pub fn fill_rect(fb: &Framebuffer, rect: Rect, color: Color) {
    if rect.is_empty() { return; }

    let fb_w = fb.width() as i32;
    let fb_h = fb.height() as i32;

    let x1 = rect.x.max(0).min(fb_w);
    let y1 = rect.y.max(0).min(fb_h);
    let x2 = (rect.x + rect.width as i32).max(0).min(fb_w);
    let y2 = (rect.y + rect.height as i32).max(0).min(fb_h);

    if x1 >= x2 || y1 >= y2 { return; }

    let width = (x2 - x1) as u32;
    let height = (y2 - y1) as u32;
    let pixel = color.to_bgr32();
    let fb_stride = fb.stride();
    let fb_addr = fb.address();

    for y in 0..height {
        let py = y1 + y as i32;
        let line_addr = fb_addr + (py as usize * fb_stride as usize + x1 as usize) * 4;
        let line_ptr = line_addr as *mut u32;

        unsafe {
            fill_line_fast(line_ptr, width, pixel);
        }
    }
}

pub fn draw_string(fb: &Framebuffer, x: i32, y: i32, text: &str, fg: Color, bg: Color) {
    fb.draw_string(x as u32, y as u32, text, fg, bg);
}

#[inline(always)]
unsafe fn fill_line_fast(ptr: *mut u32, width: u32, pixel: u32) {
    #[cfg(target_arch = "x86_64")]
    {
        use core::arch::x86_64::*;
        let mut i = 0;

        if width >= 4 {
            let color_vec = _mm_set1_epi32(pixel as i32);
            while i + 3 < width {
                _mm_storeu_si128(ptr.add(i as usize) as *mut __m128i, color_vec);
                i += 4;
            }
        }

        while i < width {
            core::ptr::write_volatile(ptr.add(i as usize), pixel);
            i += 1;
        }
    }

    #[cfg(not(target_arch = "x86_64"))]
    {
        for i in 0..width {
            core::ptr::write_volatile(ptr.add(i as usize), pixel);
        }
    }
}

pub fn blit(src: &SharedSurface, src_rect: Rect, dst: &Framebuffer, dst_x: i32, dst_y: i32) {
    let Some(src_addr) = src.address() else { return };

    let sw = src.width() as i32;
    let sh = src.height() as i32;
    let dw = dst.width() as i32;
    let dh = dst.height() as i32;

    let clip_x1 = src_rect.x.max(0);
    let clip_y1 = src_rect.y.max(0);
    let clip_x2 = (src_rect.x + src_rect.width as i32).min(sw);
    let clip_y2 = (src_rect.y + src_rect.height as i32).min(sh);

    let mut dx = dst_x + (clip_x1 - src_rect.x);
    let mut dy = dst_y + (clip_y1 - src_rect.y);

    if dx < 0 {
        let diff = -dx;
        // clip_x1 is immutable, so we'll use a new variable
    }
    // Re-calculating with more care
    let mut actual_src_x = clip_x1;
    let mut actual_src_y = clip_y1;
    let mut actual_dst_x = dx;
    let mut actual_dst_y = dy;

    if actual_dst_x < 0 {
        actual_src_x += -actual_dst_x;
        actual_dst_x = 0;
    }
    if actual_dst_y < 0 {
        actual_src_y += -actual_dst_y;
        actual_dst_y = 0;
    }

    let width = (clip_x2 - actual_src_x).min(dw - actual_dst_x);
    let height = (clip_y2 - actual_src_y).min(dh - actual_dst_y);

    if width <= 0 || height <= 0 { return; }

    let src_stride = src.stride() as usize;
    let dst_stride = dst.stride() as usize;

    for y in 0..height {
        let sy = actual_src_y + y;
        let dy_final = actual_dst_y + y;

        let src_line = src_addr + (sy as usize * src_stride + actual_src_x as usize) * 4;
        let dst_line = dst.address() + (dy_final as usize * dst_stride + actual_dst_x as usize) * 4;

        unsafe {
            core::ptr::copy_nonoverlapping(src_line as *const u8, dst_line as *mut u8, width as usize * 4);
        }
    }
}
