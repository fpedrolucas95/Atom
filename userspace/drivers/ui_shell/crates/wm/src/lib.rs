//! Window Manager

#![no_std]
extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
pub use libipc::messages::WindowId;
use atom_syscall::graphics::{SharedSurface, Color, Framebuffer};
use atom_syscall::process::ProcessId;
use atom_syscall::ipc::PortId;
use desktop_gfx::{Rect, fill_rect, blit};

pub const WINDOW_HEADER_HEIGHT: u32 = 32;
pub const WINDOW_BORDER_WIDTH: u32 = 1;
pub const WINDOW_MIN_WIDTH: u32 = 120;
pub const WINDOW_MIN_HEIGHT: u32 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowState {
    Normal,
    Minimized,
    Maximized,
    SnappedLeft,
    SnappedRight,
}

pub struct Window {
    pub id: WindowId,
    pub title: String,
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub state: WindowState,
    pub visible: bool,
    pub focused: bool,
    pub event_port: Option<PortId>,
    pub process_id: Option<ProcessId>,
    pub surface: Option<SharedSurface>,
    pub saved_rect: Rect,
}

impl Window {
    pub fn new(id: WindowId, title: &str, x: i32, y: i32, width: u32, height: u32) -> Self {
        let width = width.max(WINDOW_MIN_WIDTH);
        let height = height.max(WINDOW_MIN_HEIGHT);
        Self {
            id,
            title: String::from(title),
            x,
            y,
            width,
            height,
            state: WindowState::Normal,
            visible: true,
            focused: false,
            event_port: None,
            process_id: None,
            surface: None,
            saved_rect: Rect::new(x, y, width, height),
        }
    }

    pub fn content_rect(&self) -> Rect {
        Rect::new(
            self.x + WINDOW_BORDER_WIDTH as i32,
            self.y + WINDOW_HEADER_HEIGHT as i32,
            self.width.saturating_sub(WINDOW_BORDER_WIDTH * 2),
            self.height.saturating_sub(WINDOW_HEADER_HEIGHT + WINDOW_BORDER_WIDTH)
        )
    }

    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && px < self.x + self.width as i32 &&
        py >= self.y && py < self.y + self.height as i32
    }
}

pub struct WindowManager {
    pub windows: Vec<Window>,
    pub focused_id: Option<WindowId>,
    pub next_id: WindowId,
    pub work_area: Rect,
}

impl WindowManager {
    pub fn new(screen_width: u32, screen_height: u32) -> Self {
        let work_area = Rect::new(0, 32, screen_width, screen_height - 32 - 60);
        Self {
            windows: Vec::new(),
            focused_id: None,
            next_id: 1,
            work_area,
        }
    }

    pub fn create_window(&mut self, title: &str, width: u32, height: u32) -> WindowId {
        let id = self.next_id;
        self.next_id += 1;
        let mut window = Window::new(id, title, 100 + (id as i32 * 20) % 200, 100 + (id as i32 * 20) % 200, width, height);

        let content = window.content_rect();
        if let Ok(surface) = SharedSurface::create(content.width, content.height) {
            window.surface = Some(surface);
        }

        self.windows.push(window);
        self.focus_window(id);
        id
    }

    pub fn get_window_mut(&mut self, id: WindowId) -> Option<&mut Window> {
        self.windows.iter_mut().find(|w| w.id == id)
    }

    pub fn destroy_window(&mut self, id: WindowId) {
        self.windows.retain(|w| w.id != id);
        if self.focused_id == Some(id) {
            self.focused_id = self.windows.last().map(|w| w.id);
        }
    }

    pub fn move_window(&mut self, id: WindowId, nx: i32, ny: i32) {
        if let Some(w) = self.get_window_mut(id) {
            w.x = nx;
            w.y = ny;
            if w.state == WindowState::Normal {
                w.saved_rect.x = nx;
                w.saved_rect.y = ny;
            }
        }
    }

    pub fn resize_window(&mut self, id: WindowId, nw: u32, nh: u32) {
        if let Some(w) = self.get_window_mut(id) {
            w.width = nw.max(WINDOW_MIN_WIDTH);
            w.height = nh.max(WINDOW_MIN_HEIGHT);

            let content = w.content_rect();
            if let Ok(surface) = SharedSurface::create(content.width, content.height) {
                w.surface = Some(surface);
            }
        }
    }

    pub fn focus_window(&mut self, id: WindowId) {
        if let Some(fid) = self.focused_id {
            if let Some(prev) = self.get_window_mut(fid) {
                prev.focused = false;
            }
        }
        if let Some(pos) = self.windows.iter().position(|w| w.id == id) {
            let mut window = self.windows.remove(pos);
            window.focused = true;
            window.visible = true;
            self.windows.push(window);
            self.focused_id = Some(id);
        }
    }

    pub fn snap_window(&mut self, id: WindowId, state: WindowState) {
        let (nx, ny, nw, nh) = match state {
            WindowState::Maximized => (self.work_area.x, self.work_area.y, self.work_area.width, self.work_area.height),
            WindowState::SnappedLeft => (self.work_area.x, self.work_area.y, self.work_area.width / 2, self.work_area.height),
            WindowState::SnappedRight => (self.work_area.x + (self.work_area.width / 2) as i32, self.work_area.y, self.work_area.width / 2, self.work_area.height),
            _ => return,
        };
        if let Some(w) = self.get_window_mut(id) {
            if w.state == WindowState::Normal { w.saved_rect = Rect::new(w.x, w.y, w.width, w.height); }
            w.x = nx; w.y = ny; w.width = nw; w.height = nh; w.state = state;

            let content = w.content_rect();
            if let Ok(surface) = SharedSurface::create(content.width, content.height) {
                w.surface = Some(surface);
            }
        }
    }

    pub fn draw_window(&self, fb: &Framebuffer, window: &Window) {
        let header_color = if window.focused { Color::new(55, 61, 73) } else { Color::new(35, 39, 46) };
        fill_rect(fb, Rect::new(window.x, window.y, window.width, WINDOW_HEADER_HEIGHT), header_color);
        if let Some(ref surface) = window.surface {
            let content = window.content_rect();
            blit(surface, Rect::new(0, 0, content.width, content.height), fb, content.x, content.y);
        }
    }
}
