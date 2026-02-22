//! Drawing Surface
//!
//! Provides an abstract drawing surface for applications.
//! Applications draw to their assigned surface, and the desktop
//! compositor handles actual screen rendering.

extern crate alloc;

use crate::color::Color;
use crate::font::{get_glyph, FONT_WIDTH, FONT_HEIGHT};
use atom_syscall::graphics::SharedSurface;
use libipc::messages::{WindowId, WmCreateWindowResponse, SurfacePresentMsg, MessageType};
use libipc::protocol::send_message_async;
use atom_syscall::ipc::PortId;
use alloc::rc::Rc;
use core::cell::RefCell;

/// Internal state for a drawing surface
pub(crate) struct SurfaceInner {
    pub window_id: WindowId,
    pub shared: SharedSurface,
    pub wm_port: PortId,
    pub dirty: bool,
    pub dirty_rect: Option<libipc::messages::Rect>,
    pub sequence: u32,
    pub scale_factor: u32,
}

/// A drawing surface for an application window
#[derive(Clone)]
pub struct Surface {
    pub(crate) inner: Rc<RefCell<SurfaceInner>>,
}

impl Surface {
    /// Create a surface from window manager response
    pub fn from_wm_response(resp: WmCreateWindowResponse, wm_port: PortId) -> Result<Self, atom_syscall::SyscallError> {
        let shared = SharedSurface::from_region(resp.region_id, resp.width, resp.height)?;

        Ok(Self {
            inner: Rc::new(RefCell::new(SurfaceInner {
                window_id: resp.window_id,
                shared,
                wm_port,
                dirty: false,
                dirty_rect: None,
                sequence: 0,
                scale_factor: 1000, // 1.0 default
            })),
        })
    }

    /// Get window ID
    pub fn window_id(&self) -> WindowId {
        self.inner.borrow().window_id
    }

    /// Get surface width
    pub fn width(&self) -> u32 {
        self.inner.borrow().shared.width()
    }

    /// Get surface height
    pub fn height(&self) -> u32 {
        self.inner.borrow().shared.height()
    }

    /// Check if point is within surface bounds
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= 0 && y >= 0 && (x as u32) < self.width() && (y as u32) < self.height()
    }

    /// Set a pixel at the given coordinates
    pub fn set_pixel(&mut self, x: u32, y: u32, color: Color) {
        let mut inner = self.inner.borrow_mut();
        let atom_color = atom_syscall::graphics::Color::new(color.r, color.g, color.b);
        inner.shared.draw_pixel(x, y, atom_color);

        let rect = libipc::messages::Rect::new(x as i32, y as i32, 1, 1);
        Self::update_dirty_rect(&mut inner, rect);
    }

    fn update_dirty_rect(inner: &mut core::cell::RefMut<SurfaceInner>, new_rect: libipc::messages::Rect) {
        if let Some(existing) = inner.dirty_rect {
            let x1 = existing.x.min(new_rect.x);
            let y1 = existing.y.min(new_rect.y);
            let x2 = (existing.x + existing.width as i32).max(new_rect.x + new_rect.width as i32);
            let y2 = (existing.y + existing.height as i32).max(new_rect.y + new_rect.height as i32);
            inner.dirty_rect = Some(libipc::messages::Rect::new(x1, y1, (x2 - x1) as u32, (y2 - y1) as u32));
        } else {
            inner.dirty_rect = Some(new_rect);
        }
        inner.dirty = true;
    }

    /// Get a pixel at the given coordinates
    pub fn get_pixel(&self, x: u32, y: u32) -> Option<Color> {
        let inner = self.inner.borrow();
        if x >= inner.shared.width() || y >= inner.shared.height() {
            return None;
        }

        if let Some(addr) = inner.shared.address() {
            let offset = (y * inner.shared.stride() + x) as usize * inner.shared.bytes_per_pixel();
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
        let mut inner = self.inner.borrow_mut();
        let atom_color = atom_syscall::graphics::Color::new(color.r, color.g, color.b);
        let w = inner.shared.width();
        let h = inner.shared.height();
        inner.shared.clear(atom_color);
        Self::update_dirty_rect(&mut inner, libipc::messages::Rect::new(0, 0, w, h));
    }

    /// Fill a rectangle with a color
    pub fn fill_rect(&mut self, x: u32, y: u32, width: u32, height: u32, color: Color) {
        let mut inner = self.inner.borrow_mut();
        let atom_color = atom_syscall::graphics::Color::new(color.r, color.g, color.b);
        inner.shared.fill_rect(x, y, width, height, atom_color);
        Self::update_dirty_rect(&mut inner, libipc::messages::Rect::new(x as i32, y as i32, width, height));
    }

    /// Draw a horizontal line
    pub fn draw_hline(&mut self, x: u32, y: u32, length: u32, color: Color) {
        let mut inner = self.inner.borrow_mut();
        let atom_color = atom_syscall::graphics::Color::new(color.r, color.g, color.b);
        inner.shared.fill_rect(x, y, length, 1, atom_color);
        inner.dirty = true;
    }

    /// Draw a vertical line
    pub fn draw_vline(&mut self, x: u32, y: u32, length: u32, color: Color) {
        let mut inner = self.inner.borrow_mut();
        let atom_color = atom_syscall::graphics::Color::new(color.r, color.g, color.b);
        inner.shared.fill_rect(x, y, 1, length, atom_color);
        inner.dirty = true;
    }

    /// Draw a rectangle outline
    pub fn draw_rect(&mut self, x: u32, y: u32, width: u32, height: u32, color: Color) {
        let mut inner = self.inner.borrow_mut();
        let atom_color = atom_syscall::graphics::Color::new(color.r, color.g, color.b);
        inner.shared.fill_rect(x, y, width, 1, atom_color);
        inner.shared.fill_rect(x, y + height.saturating_sub(1), width, 1, atom_color);
        inner.shared.fill_rect(x, y, 1, height, atom_color);
        inner.shared.fill_rect(x + width.saturating_sub(1), y, 1, height, atom_color);
        inner.dirty = true;
    }

    /// Internal helper to draw a single glyph
    fn draw_glyph(shared: &SharedSurface, x: u32, y: u32, ch: u8, fg: atom_syscall::graphics::Color, bg: atom_syscall::graphics::Color) {
        let glyph = get_glyph(ch);
        for row in 0..FONT_HEIGHT {
            for col in 0..FONT_WIDTH {
                let bit = (glyph[row as usize] >> (7 - col)) & 1;
                let color = if bit == 1 { fg } else { bg };
                shared.draw_pixel(x + col, y + row, color);
            }
        }
    }

    /// Draw a single character at the given position
    pub fn draw_char(&mut self, x: u32, y: u32, ch: u8, fg: Color, bg: Color) {
        let mut inner = self.inner.borrow_mut();
        let fg_atom = atom_syscall::graphics::Color::new(fg.r, fg.g, fg.b);
        let bg_atom = atom_syscall::graphics::Color::new(bg.r, bg.g, bg.b);

        Self::draw_glyph(&inner.shared, x, y, ch, fg_atom, bg_atom);
        inner.dirty = true;
    }

    /// Draw a string at the given position
    pub fn draw_string(&mut self, x: u32, y: u32, text: &str, fg: Color, bg: Color) {
        let mut inner = self.inner.borrow_mut();
        let mut cx = x;
        let fg_atom = atom_syscall::graphics::Color::new(fg.r, fg.g, fg.b);
        let bg_atom = atom_syscall::graphics::Color::new(bg.r, bg.g, bg.b);
        let width = inner.shared.width();

        for ch in text.bytes() {
            if cx + FONT_WIDTH > width {
                break;
            }
            Self::draw_glyph(&inner.shared, cx, y, ch, fg_atom, bg_atom);
            cx += FONT_WIDTH;
        }
        inner.dirty = true;
    }

    /// Invalidate a region of the surface
    pub fn invalidate_rect(&mut self, x: i32, y: i32, width: u32, height: u32) {
        let mut inner = self.inner.borrow_mut();
        let new_rect = libipc::messages::Rect::new(x, y, width, height);
        if let Some(existing) = inner.dirty_rect {
            // Simple union
            let x1 = existing.x.min(new_rect.x);
            let y1 = existing.y.min(new_rect.y);
            let x2 = (existing.x + existing.width as i32).max(new_rect.x + new_rect.width as i32);
            let y2 = (existing.y + existing.height as i32).max(new_rect.y + new_rect.height as i32);
            inner.dirty_rect = Some(libipc::messages::Rect::new(x1, y1, (x2 - x1) as u32, (y2 - y1) as u32));
        } else {
            inner.dirty_rect = Some(new_rect);
        }
        inner.dirty = true;
    }

    /// Present the surface (signal compositor to display)
    pub fn present(&mut self) {
        let mut inner = self.inner.borrow_mut();
        if inner.dirty {
            let rect = inner.dirty_rect.unwrap_or(libipc::messages::Rect::new(0, 0, inner.shared.width(), inner.shared.height()));
            let msg = SurfacePresentMsg {
                window_id: inner.window_id,
                sequence: inner.sequence,
                dirty_x: rect.x,
                dirty_y: rect.y,
                dirty_w: rect.width,
                dirty_h: rect.height,
            };
            let _ = send_message_async(inner.wm_port, MessageType::SurfacePresent, &msg.to_bytes());
            inner.sequence = inner.sequence.wrapping_add(1);
            inner.dirty = false;
            inner.dirty_rect = None;
        }
    }
}
