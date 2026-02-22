//! Desktop Shell

#![no_std]
extern crate alloc;

use alloc::vec::Vec;
use alloc::string::String;
use atom_syscall::graphics::{Color, Framebuffer};
use desktop_gfx::{Rect, fill_rect, draw_string};
use desktop_wm::Window;

pub const PANEL_HEIGHT: u32 = 32;
pub const DOCK_HEIGHT: u32 = 60;

pub struct DesktopIcon {
    pub name: String,
    pub executable: String,
    pub x: i32,
    pub y: i32,
    pub color: Color,
}

pub struct Shell {
    pub icons: Vec<DesktopIcon>,
    pub launcher_text: String,
}

impl Shell {
    pub fn new() -> Self {
        let mut icons = Vec::new();
        icons.push(DesktopIcon {
            name: String::from("Terminal"),
            executable: String::from("terminal"),
            x: 20,
            y: PANEL_HEIGHT as i32 + 20,
            color: Color::new(76, 86, 106),
        });
        icons.push(DesktopIcon {
            name: String::from("File Manager"),
            executable: String::from("fileman"),
            x: 20,
            y: PANEL_HEIGHT as i32 + 120,
            color: Color::new(136, 192, 208),
        });

        Self {
            icons,
            launcher_text: String::from("Type to search..."),
        }
    }

    pub fn draw_panel(&self, fb: &Framebuffer) {
        let panel_bg = Color::new(20, 22, 28);
        fill_rect(fb, Rect::new(0, 0, fb.width(), PANEL_HEIGHT), panel_bg);

        // Launcher field
        fill_rect(fb, Rect::new(10, 4, 180, 24), Color::new(35, 39, 46));
        draw_string(fb, 20, 8, &self.launcher_text, Color::new(100, 100, 110), Color::new(35, 39, 46));

        // System area
        draw_string(fb, fb.width() as i32 - 100, 8, "12:00 PM", Color::WHITE, panel_bg);
    }

    pub fn draw_dock(&self, fb: &Framebuffer, windows: &[Window]) {
        let dock_w = 400u32;
        let dock_x = (fb.width() as i32 - dock_w as i32) / 2;
        let dock_y = fb.height() as i32 - DOCK_HEIGHT as i32 - 16;
        let dock_bg = Color::new(20, 22, 28);
        fill_rect(fb, Rect::new(dock_x, dock_y, dock_w, DOCK_HEIGHT), dock_bg);

        // Draw window indicators
        for (i, window) in windows.iter().enumerate() {
            if i >= 8 { break; }
            let ix = dock_x + 20 + (i as i32 * 45);
            let iy = dock_y + 10;
            fill_rect(fb, Rect::new(ix, iy, 35, 35), Color::new(55, 61, 73));
            if window.focused {
                fill_rect(fb, Rect::new(ix + 5, iy + 37, 25, 2), Color::new(136, 192, 208));
            }
        }
    }

    pub fn draw_desktop(&self, fb: &Framebuffer) {
        for icon in &self.icons {
            fill_rect(fb, Rect::new(icon.x, icon.y, 48, 48), icon.color);
            draw_string(fb, icon.x, icon.y + 52, &icon.name, Color::WHITE, Color::new(30, 33, 40));
        }
    }
}
