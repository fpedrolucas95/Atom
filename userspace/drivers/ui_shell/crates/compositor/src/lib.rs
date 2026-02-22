//! 2D Software Compositor

#![no_std]
extern crate alloc;

use alloc::vec::Vec;
use atom_syscall::graphics::{Framebuffer, Color};
use desktop_gfx::{Rect, fill_rect};
use desktop_wm::WindowManager;
use desktop_shell::Shell;

pub struct Compositor {
    fb: Framebuffer,
    backbuffer: Framebuffer,
    _backbuffer_data: Vec<u32>,
    dirty_regions: Vec<Rect>,
}

impl Compositor {
    pub fn new(fb: Framebuffer) -> Self {
        let width = fb.width();
        let height = fb.height();
        let stride = fb.stride();
        let mut backbuffer_data = alloc::vec![0u32; (stride * height) as usize];
        let backbuffer = Framebuffer::new_custom(
            backbuffer_data.as_mut_ptr() as usize, width, height, stride, 4
        ).expect("Failed to create backbuffer");

        Self {
            fb,
            backbuffer,
            _backbuffer_data: backbuffer_data,
            dirty_regions: Vec::new(),
        }
    }

    pub fn width(&self) -> u32 { self.fb.width() }
    pub fn height(&self) -> u32 { self.fb.height() }

    pub fn mark_dirty(&mut self, rect: Rect) {
        self.dirty_regions.push(rect);
    }

    pub fn render(&mut self, wm: &WindowManager, shell: &Shell, cursor_x: i32, cursor_y: i32) {
        if self.dirty_regions.is_empty() { return; }

        let dirties = core::mem::take(&mut self.dirty_regions);
        for dirty in dirties {
            // Background
            fill_rect(&self.backbuffer, dirty, Color::new(30, 33, 40));

            // Windows
            for window in &wm.windows {
                if !window.visible { continue; }
                let win_rect = Rect::new(window.x, window.y, window.width, window.height);
                if let Some(overlap) = dirty.intersection(&win_rect) {
                    wm.draw_window(&self.backbuffer, window); // FIXME: wm.draw_window should ideally take clip
                }
            }

            // Shell
            shell.draw_panel(&self.backbuffer);
            shell.draw_dock(&self.backbuffer, &wm.windows);

            // Cursor
            self.draw_cursor(cursor_x, cursor_y);

            // Final Blit
            self.blit_dirty(dirty);
        }
    }

    fn draw_cursor(&self, x: i32, y: i32) {
         // Simple 10x10 white cursor
         fill_rect(&self.backbuffer, Rect::new(x, y, 10, 10), Color::WHITE);
    }

    fn blit_dirty(&self, rect: Rect) {
        let x1 = rect.x.max(0).min(self.fb.width() as i32);
        let y1 = rect.y.max(0).min(self.fb.height() as i32);
        let x2 = (rect.x + rect.width as i32).max(0).min(self.fb.width() as i32);
        let y2 = (rect.y + rect.height as i32).max(0).min(self.fb.height() as i32);
        if x1 >= x2 || y1 >= y2 { return; }

        let copy_width = (x2 - x1) as usize;
        let stride = self.fb.stride() as usize;
        let src_addr = self.backbuffer.address();
        let dst_addr = self.fb.address();

        for y in y1..y2 {
            let offset = (y as usize * stride + x1 as usize) * 4;
            unsafe {
                core::ptr::copy_nonoverlapping(
                    (src_addr + offset) as *const u8,
                    (dst_addr + offset) as *mut u8,
                    copy_width * 4
                );
            }
        }
    }
}
