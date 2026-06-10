// Terminal Window and Rendering Module
//
// This module handles all graphical rendering for the terminal window.
// It communicates with the display server via syscalls and IPC,
// never accessing kernel internals directly.

use atom_syscall::graphics::{Color, Framebuffer};

/// Terminal color theme - Modern dark
pub struct Theme;
impl Theme {
    // Window chrome colors
    pub const WINDOW_BG: Color = Color::new(18, 20, 28); // Deep dark terminal background
    pub const WINDOW_BORDER: Color = Color::new(42, 48, 64); // Subtle border
    pub const TITLE_BAR_BG: Color = Color::new(24, 27, 36); // Title bar background
    pub const TITLE_BAR_TEXT: Color = Color::new(180, 186, 200);

    // Terminal content colors
    pub const TEXT_NORMAL: Color = Color::new(210, 215, 225); // Default text
    pub const TEXT_BRIGHT: Color = Color::WHITE; // Bright/bold text
    pub const TEXT_DIM: Color = Color::new(110, 118, 138); // Dimmed text
    pub const TEXT_ERROR: Color = Color::new(240, 85, 96); // Error messages
    pub const TEXT_SUCCESS: Color = Color::new(72, 199, 142); // Success messages
    pub const TEXT_INFO: Color = Color::new(86, 182, 245); // Info messages
    pub const TEXT_WARNING: Color = Color::new(245, 189, 65); // Warning messages
    pub const RESOURCE_SYSTEM: Color = Color::new(99, 143, 255);
    pub const RESOURCE_FRAMEBUFFER: Color = Color::new(200, 120, 255);
    pub const RESOURCE_SHARED: Color = Color::new(65, 205, 190);
    pub const RESOURCE_FREE: Color = Color::new(70, 76, 94);
    pub const RESOURCE_APP_1: Color = Color::new(245, 189, 65);
    pub const RESOURCE_APP_2: Color = Color::new(240, 105, 120);
    pub const RESOURCE_APP_3: Color = Color::new(72, 199, 142);
    pub const RESOURCE_APP_4: Color = Color::new(255, 145, 75);

    // Prompt colors
    pub const PROMPT_USER: Color = Color::new(99, 143, 255); // User part of prompt
    pub const PROMPT_PATH: Color = Color::new(72, 199, 142); // Path part of prompt
    pub const PROMPT_SYMBOL: Color = Color::new(200, 160, 255); // $ or # symbol

    // Cursor
    pub const CURSOR_BG: Color = Color::new(99, 143, 255); // Cursor block color (accent)

    // Selection (future use)
    pub const SELECTION_BG: Color = Color::new(50, 70, 110); // Selected text background

    // Separator / divider lines in help output
    pub const SEPARATOR: Color = Color::new(48, 54, 72);

    // Scrollbar
    pub const SCROLLBAR_TRACK: Color = Color::new(22, 25, 34);
    pub const SCROLLBAR_THUMB: Color = Color::new(52, 60, 82);
    pub const SCROLLBAR_THUMB_ACTIVE: Color = Color::new(80, 92, 125);
}

/// Configuration for window dimensions and layout
pub struct WindowConfig {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub title_bar_height: u32,
    pub border_width: u32,
    pub padding: u32,
    pub char_width: u32,
    pub char_height: u32,
}

impl WindowConfig {
    pub const fn new() -> Self {
        Self {
            x: 80,
            y: 60,
            width: 640,
            height: 400,
            title_bar_height: 24,
            border_width: 1,
            padding: 8,
            char_width: 8,
            char_height: 8,
        }
    }

    /// Calculate content area dimensions
    pub fn content_x(&self) -> u32 {
        self.x + self.border_width + self.padding
    }

    pub fn content_y(&self) -> u32 {
        self.y + self.title_bar_height + self.padding
    }

    pub fn content_width(&self) -> u32 {
        self.width - 2 * self.border_width - 2 * self.padding
    }

    pub fn content_height(&self) -> u32 {
        self.height - self.title_bar_height - 2 * self.padding - self.border_width
    }

    /// Calculate number of columns and rows for text
    pub fn cols(&self) -> u32 {
        self.content_width() / self.char_width
    }

    pub fn rows(&self) -> u32 {
        self.content_height() / self.char_height
    }
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Terminal window renderer
pub struct TerminalWindow {
    config: WindowConfig,
    title: &'static str,
    needs_full_redraw: bool,
}

impl TerminalWindow {
    pub fn new(title: &'static str) -> Self {
        Self {
            config: WindowConfig::new(),
            title,
            needs_full_redraw: true,
        }
    }

    /// Get window configuration
    pub fn config(&self) -> &WindowConfig {
        &self.config
    }

    /// Get mutable configuration
    pub fn config_mut(&mut self) -> &mut WindowConfig {
        &mut self.config
    }

    /// Draw the complete window frame (title bar, borders, background)
    pub fn draw_frame(&self, fb: &Framebuffer) {
        let cfg = &self.config;

        // Window border
        fb.fill_rect(cfg.x, cfg.y, cfg.width, cfg.height, Theme::WINDOW_BORDER);

        // Title bar background
        fb.fill_rect(
            cfg.x + cfg.border_width,
            cfg.y + cfg.border_width,
            cfg.width - 2 * cfg.border_width,
            cfg.title_bar_height - cfg.border_width,
            Theme::TITLE_BAR_BG,
        );

        // Title text (vertically centered)
        fb.draw_string(
            cfg.x + cfg.padding + cfg.border_width,
            cfg.y + (cfg.title_bar_height - cfg.char_height) / 2,
            self.title,
            Theme::TITLE_BAR_TEXT,
            Theme::TITLE_BAR_BG,
        );

        // Window control buttons (rounded circles)
        let button_y = cfg.y + (cfg.title_bar_height - 10) / 2;
        let button_x = cfg.x + cfg.width - cfg.padding - 10 - cfg.border_width;

        // Close button (red)
        fb.fill_rect_rounded_aa(button_x, button_y, 10, 10, 5, Color::new(237, 78, 83));

        // Minimize button (yellow)
        fb.fill_rect_rounded_aa(button_x - 16, button_y, 10, 10, 5, Color::new(245, 189, 65));

        // Maximize button (green)
        fb.fill_rect_rounded_aa(button_x - 32, button_y, 10, 10, 5, Color::new(72, 199, 142));

        // Content area background
        fb.fill_rect(
            cfg.x + cfg.border_width,
            cfg.y + cfg.title_bar_height,
            cfg.width - 2 * cfg.border_width,
            cfg.height - cfg.title_bar_height - cfg.border_width,
            Theme::WINDOW_BG,
        );
    }

    /// Clear only the content area
    pub fn clear_content(&self, fb: &Framebuffer) {
        let cfg = &self.config;
        fb.fill_rect(
            cfg.x + cfg.border_width,
            cfg.y + cfg.title_bar_height,
            cfg.width - 2 * cfg.border_width,
            cfg.height - cfg.title_bar_height - cfg.border_width,
            Theme::WINDOW_BG,
        );
    }

    /// Draw a single character at the given row/column position
    pub fn draw_char(&self, fb: &Framebuffer, row: u32, col: u32, ch: u8, fg: Color, bg: Color) {
        let cfg = &self.config;
        let x = cfg.content_x() + col * cfg.char_width;
        let y = cfg.content_y() + row * cfg.char_height;

        // Draw background
        fb.fill_rect(x, y, cfg.char_width, cfg.char_height, bg);

        // Draw character
        fb.draw_char(x, y, ch, fg, bg);
    }

    /// Draw a string at the given row/column position
    pub fn draw_text(
        &self,
        fb: &Framebuffer,
        row: u32,
        col: u32,
        text: &str,
        fg: Color,
        bg: Color,
    ) {
        let cfg = &self.config;
        let x = cfg.content_x() + col * cfg.char_width;
        let y = cfg.content_y() + row * cfg.char_height;

        fb.draw_string(x, y, text, fg, bg);
    }

    /// Draw text with the window background color
    pub fn draw_text_default(&self, fb: &Framebuffer, row: u32, col: u32, text: &str, fg: Color) {
        self.draw_text(fb, row, col, text, fg, Theme::WINDOW_BG);
    }

    /// Draw the cursor at the given position
    pub fn draw_cursor(&self, fb: &Framebuffer, row: u32, col: u32) {
        let cfg = &self.config;
        let x = cfg.content_x() + col * cfg.char_width;
        let y = cfg.content_y() + row * cfg.char_height;

        // Block cursor
        fb.fill_rect(x, y, cfg.char_width, cfg.char_height, Theme::CURSOR_BG);
    }

    /// Draw a character with cursor (inverted colors)
    pub fn draw_char_with_cursor(&self, fb: &Framebuffer, row: u32, col: u32, ch: u8) {
        let cfg = &self.config;
        let x = cfg.content_x() + col * cfg.char_width;
        let y = cfg.content_y() + row * cfg.char_height;

        // Draw cursor background
        fb.fill_rect(x, y, cfg.char_width, cfg.char_height, Theme::CURSOR_BG);

        // Draw character in inverted color
        fb.draw_char(x, y, ch, Theme::WINDOW_BG, Theme::CURSOR_BG);
    }

    /// Clear a specific row
    pub fn clear_row(&self, fb: &Framebuffer, row: u32) {
        let cfg = &self.config;
        let y = cfg.content_y() + row * cfg.char_height;

        fb.fill_rect(
            cfg.content_x(),
            y,
            cfg.content_width(),
            cfg.char_height,
            Theme::WINDOW_BG,
        );
    }

    /// Clear from cursor position to end of row
    pub fn clear_to_eol(&self, fb: &Framebuffer, row: u32, col: u32) {
        let cfg = &self.config;
        let x = cfg.content_x() + col * cfg.char_width;
        let y = cfg.content_y() + row * cfg.char_height;
        let remaining_width = cfg.content_width().saturating_sub(col * cfg.char_width);

        fb.fill_rect(x, y, remaining_width, cfg.char_height, Theme::WINDOW_BG);
    }

    /// Mark window as needing full redraw
    pub fn invalidate(&mut self) {
        self.needs_full_redraw = true;
    }

    /// Check and clear the redraw flag
    pub fn needs_redraw(&mut self) -> bool {
        let needs = self.needs_full_redraw;
        self.needs_full_redraw = false;
        needs
    }

    /// Scroll the visible content up by n lines
    /// This is a visual operation - the actual buffer management is in buffer.rs
    pub fn scroll_region(&self, fb: &Framebuffer, lines: u32) {
        let cfg = &self.config;
        let content_x = cfg.content_x();
        let content_y = cfg.content_y();
        let content_w = cfg.content_width();
        let line_height = cfg.char_height;
        let total_rows = cfg.rows();

        if lines == 0 || lines >= total_rows {
            self.clear_content(fb);
            return;
        }

        let scroll_pixels = lines * line_height;
        let remaining_rows = total_rows - lines;

        // Copy lines up (simple byte-by-byte copy for now)
        // In a real implementation, this would use a more efficient memory copy
        let fb_addr = fb.address();
        let stride = fb.stride();
        let bpp = fb.bytes_per_pixel();

        for row in 0..remaining_rows {
            let src_y = content_y + (row + lines) * line_height;
            let dst_y = content_y + row * line_height;

            for line in 0..line_height {
                for col in 0..content_w {
                    let src_offset = ((src_y + line) * stride + content_x + col) as usize * bpp;
                    let dst_offset = ((dst_y + line) * stride + content_x + col) as usize * bpp;

                    let src_ptr = (fb_addr + src_offset) as *const u32;
                    let dst_ptr = (fb_addr + dst_offset) as *mut u32;

                    unsafe {
                        let pixel = src_ptr.read_volatile();
                        dst_ptr.write_volatile(pixel);
                    }
                }
            }
        }

        // Clear the scrolled-in area at the bottom
        for row in remaining_rows..total_rows {
            self.clear_row(fb, row);
        }
    }
}
