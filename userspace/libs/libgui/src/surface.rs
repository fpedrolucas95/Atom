//! Drawing Surface
//!
//! Provides an abstract drawing surface for applications.
//! Applications draw to their assigned surface, and the desktop
//! compositor handles actual screen rendering.

extern crate alloc;

use crate::color::Color;
use crate::font::{get_glyph, FONT_WIDTH, FONT_HEIGHT};
use atom_syscall::graphics::{SharedSurface, SharedRegionId};
use libipc::messages::{WindowId, WmCreateWindowResponse, WmCommitFrameMsg, MessageType};
use libipc::protocol::send_message_async;
use atom_syscall::ipc::PortId;

/// A drawing surface for an application window
pub struct Surface {
    /// Window ID
    window_id: WindowId,
    /// Shared surface implementation
    shared: SharedSurface,
    /// Compositor WM port
    wm_port: PortId,
    /// Dirty flag for damage tracking
    dirty: bool,
}

unsafe impl Send for Surface {}
unsafe impl Sync for Surface {}

impl Surface {
    /// Create a surface from window manager response
    pub fn from_wm_response(resp: WmCreateWindowResponse, wm_port: PortId) -> Result<Self, atom_syscall::SyscallError> {
        let shared = SharedSurface::from_region(resp.region_id, resp.width, resp.height)?;

        Ok(Self {
            window_id: resp.window_id,
            shared,
            wm_port,
            dirty: false,
        })
    }

    /// Get window ID
    pub fn window_id(&self) -> WindowId {
        self.window_id
    }

    /// Get surface width
    pub fn width(&self) -> u32 {
        self.shared.width()
    }

    /// Get surface height
    pub fn height(&self) -> u32 {
        self.shared.height()
    }

    /// Check if point is within surface bounds
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as u32) < self.width() && (y as u32) < self.height()
    }

    /// Set a pixel at the given coordinates
    pub fn set_pixel(&mut self, x: u32, y: u32, color: Color) {
        let atom_color = atom_syscall::graphics::Color::new(color.r, color.g, color.b);
        self.shared.draw_pixel(x, y, atom_color);
        self.dirty = true;
    }

    /// Get a pixel at the given coordinates
    pub fn get_pixel(&self, x: u32, y: u32) -> Option<Color> {
        if x >= self.width() || y >= self.height() {
            return None;
        }

        if let Some(addr) = self.shared.address() {
            let offset = (y * self.shared.stride() + x) as usize * self.shared.bytes_per_pixel();
            let ptr = (addr + offset) as *const u32;
            let value = unsafe { ptr.read_volatile() };
            return Some(Color::rgb(
                (value & 0xFF) as u8,
                ((value >> 8) & 0xFF) as u8,
                ((value >> 16) & 0xFF) as u8,
            ));
        }
        None
    }

    /// Fill the entire surface with a color
    pub fn clear(&mut self, color: Color) {
        let atom_color = atom_syscall::graphics::Color::new(color.r, color.g, color.b);
        self.shared.clear(atom_color);
        self.dirty = true;
    }

    /// Fill a rectangle with a color
    pub fn fill_rect(&mut self, x: u32, y: u32, width: u32, height: u32, color: Color) {
        let atom_color = atom_syscall::graphics::Color::new(color.r, color.g, color.b);
        self.shared.fill_rect(x, y, width, height, atom_color);
        self.dirty = true;
    }

    /// Draw a horizontal line
    pub fn draw_hline(&mut self, x: u32, y: u32, length: u32, color: Color) {
        self.fill_rect(x, y, length, 1, color);
    }

    /// Draw a vertical line
    pub fn draw_vline(&mut self, x: u32, y: u32, length: u32, color: Color) {
        self.fill_rect(x, y, 1, length, color);
    }

    /// Draw a rectangle outline
    pub fn draw_rect(&mut self, x: u32, y: u32, width: u32, height: u32, color: Color) {
        self.draw_hline(x, y, width, color);
        self.draw_hline(x, y + height.saturating_sub(1), width, color);
        self.draw_vline(x, y, height, color);
        self.draw_vline(x + width.saturating_sub(1), y, height, color);
    }

    /// Draw a single character at the given position
    pub fn draw_char(&mut self, x: u32, y: u32, ch: u8, fg: Color, bg: Color) {
        let glyph = get_glyph(ch);
        for row in 0..FONT_HEIGHT {
            for col in 0..FONT_WIDTH {
                let bit = (glyph[row as usize] >> (7 - col)) & 1;
                let color = if bit == 1 { fg } else { bg };
                self.set_pixel(x + col, y + row, color);
            }
        }
    }

    /// Draw a string at the given position
    pub fn draw_string(&mut self, x: u32, y: u32, text: &str, fg: Color, bg: Color) {
        let mut cx = x;
        for ch in text.bytes() {
            if cx + FONT_WIDTH > self.width() {
                break;
            }
            self.draw_char(cx, y, ch, fg, bg);
            cx += FONT_WIDTH;
        }
    }

    /// Present the surface (signal compositor to display)
    pub fn present(&mut self) {
        if self.dirty {
            let msg = WmCommitFrameMsg {
                window_id: self.window_id,
            };
            let _ = send_message_async(self.wm_port, MessageType::WmCommitFrame, &msg.to_bytes());
            self.dirty = false;
        }
    }
}
