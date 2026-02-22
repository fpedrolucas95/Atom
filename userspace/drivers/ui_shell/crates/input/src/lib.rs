//! Input handling

#![no_std]

use atom_syscall::input::{MouseDriver, keyboard_poll, scancodes};
use desktop_wm::{WindowManager, WindowId, WindowState, WINDOW_HEADER_HEIGHT};
use desktop_compositor::Compositor;
use desktop_shell::Shell;
use desktop_gfx::Rect;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MouseAction {
    None,
    Dragging(WindowId),
    Resizing(WindowId, ResizeDirection),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResizeDirection {
    Right,
    Bottom,
    BottomRight,
}

pub struct InputHandler {
    pub mouse: MouseDriver,
    pub mouse_x: i32,
    pub mouse_y: i32,
    pub mouse_left_down: bool,
    action: MouseAction,
    drag_start_mouse_x: i32,
    drag_start_mouse_y: i32,
    drag_start_win_x: i32,
    drag_start_win_y: i32,
    drag_start_win_w: u32,
    drag_start_win_h: u32,
}

impl InputHandler {
    pub fn new(screen_width: u32, screen_height: u32) -> Self {
        let mut mouse = MouseDriver::new();
        mouse.init();
        Self {
            mouse,
            mouse_x: (screen_width / 2) as i32,
            mouse_y: (screen_height / 2) as i32,
            mouse_left_down: false,
            action: MouseAction::None,
            drag_start_mouse_x: 0,
            drag_start_mouse_y: 0,
            drag_start_win_x: 0,
            drag_start_win_y: 0,
            drag_start_win_w: 0,
            drag_start_win_h: 0,
        }
    }

    pub fn poll_and_handle(&mut self, wm: &mut WindowManager, compositor: &mut Compositor, _shell: &mut Shell) {
        let sw = compositor.width();
        let sh = compositor.height();

        while let Some(event) = self.mouse.poll_event() {
            let dx = event.dx;
            let dy = event.dy;
            let old_x = self.mouse_x;
            let old_y = self.mouse_y;

            self.mouse_x = (self.mouse_x + dx).clamp(0, (sw - 1) as i32);
            self.mouse_y = (self.mouse_y - dy).clamp(0, (sh - 1) as i32);

            if self.mouse_x != old_x || self.mouse_y != old_y {
                compositor.mark_dirty(Rect::new(old_x - 1, old_y - 1, 18, 18));
                compositor.mark_dirty(Rect::new(self.mouse_x - 1, self.mouse_y - 1, 18, 18));
            }

            if event.left_button {
                if !self.mouse_left_down {
                    self.handle_click_start(wm, compositor);
                } else {
                    self.handle_drag_move(wm, compositor);
                }
            } else if self.mouse_left_down {
                self.handle_click_end(wm, compositor);
            }
            self.mouse_left_down = event.left_button;
        }

        while let Some(scancode) = keyboard_poll() {
            let pressed = (scancode & 0x80) == 0;
            let code = scancode & 0x7F;
            match code {
                scancodes::TAB if pressed => {
                    if wm.windows.len() > 1 {
                        let id = wm.windows[0].id;
                        wm.focus_window(id);
                        compositor.mark_dirty(Rect::new(0, 0, sw, sh));
                    }
                }
                _ => {}
            }
        }
    }

    fn handle_click_start(&mut self, wm: &mut WindowManager, _compositor: &mut Compositor) {
        let mut found = None;
        for window in wm.windows.iter().rev() {
            if window.contains(self.mouse_x, self.mouse_y) {
                found = Some(window.id);
                break;
            }
        }

        if let Some(id) = found {
            wm.focus_window(id);
            let w = wm.get_window_mut(id).unwrap();

            let border = 8i32;
            let is_right = self.mouse_x >= (w.x + w.width as i32 - border);
            let is_bottom = self.mouse_y >= (w.y + w.height as i32 - border);

            if is_right && is_bottom {
                self.action = MouseAction::Resizing(id, ResizeDirection::BottomRight);
            } else if is_right {
                self.action = MouseAction::Resizing(id, ResizeDirection::Right);
            } else if is_bottom {
                self.action = MouseAction::Resizing(id, ResizeDirection::Bottom);
            } else if self.mouse_y < w.y + WINDOW_HEADER_HEIGHT as i32 {
                self.action = MouseAction::Dragging(id);
            } else {
                self.action = MouseAction::None;
            }

            self.drag_start_mouse_x = self.mouse_x;
            self.drag_start_mouse_y = self.mouse_y;
            self.drag_start_win_x = w.x;
            self.drag_start_win_y = w.y;
            self.drag_start_win_w = w.width;
            self.drag_start_win_h = w.height;
        } else {
            self.action = MouseAction::None;
        }
    }

    fn handle_drag_move(&mut self, wm: &mut WindowManager, compositor: &mut Compositor) {
        let sw = compositor.width();
        let sh = compositor.height();

        match self.action {
            MouseAction::Dragging(id) => {
                let nx = self.drag_start_win_x + (self.mouse_x - self.drag_start_mouse_x);
                let ny = self.drag_start_win_y + (self.mouse_y - self.drag_start_mouse_y);
                wm.move_window(id, nx, ny);
                compositor.mark_dirty(Rect::new(0, 0, sw, sh));
            }
            MouseAction::Resizing(id, dir) => {
                let mut nw = self.drag_start_win_w;
                let mut nh = self.drag_start_win_h;

                match dir {
                    ResizeDirection::Right => {
                        nw = (self.drag_start_win_w as i32 + (self.mouse_x - self.drag_start_mouse_x)).max(120) as u32;
                    }
                    ResizeDirection::Bottom => {
                        nh = (self.drag_start_win_h as i32 + (self.mouse_y - self.drag_start_mouse_y)).max(80) as u32;
                    }
                    ResizeDirection::BottomRight => {
                        nw = (self.drag_start_win_w as i32 + (self.mouse_x - self.drag_start_mouse_x)).max(120) as u32;
                        nh = (self.drag_start_win_h as i32 + (self.mouse_y - self.drag_start_mouse_y)).max(80) as u32;
                    }
                }
                wm.resize_window(id, nw, nh);
                compositor.mark_dirty(Rect::new(0, 0, sw, sh));
            }
            _ => {}
        }
    }

    fn handle_click_end(&mut self, wm: &mut WindowManager, compositor: &mut Compositor) {
        let sw = compositor.width();
        let sh = compositor.height();

        if let MouseAction::Dragging(id) = self.action {
            if self.mouse_x <= 0 {
                wm.snap_window(id, WindowState::SnappedLeft);
            } else if self.mouse_x >= sw as i32 - 1 {
                wm.snap_window(id, WindowState::SnappedRight);
            } else if self.mouse_y <= 0 {
                wm.snap_window(id, WindowState::Maximized);
            }
            compositor.mark_dirty(Rect::new(0, 0, sw, sh));
        }
        self.action = MouseAction::None;
    }
}
