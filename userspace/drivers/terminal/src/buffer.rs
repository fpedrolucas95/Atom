// Terminal Buffer Module
//
// This module manages the terminal's text content, including:
// - Line buffer for current input
// - Display buffer for visible content
// - Scrollback history (ring buffer)
// - Cursor position tracking

extern crate alloc;

use crate::window::Theme;
use atom_syscall::graphics::Color;
use alloc::vec::Vec;

/// Maximum characters per line
pub const MAX_LINE_LENGTH: usize = 256;

/// Maximum visible lines (will be set dynamically based on window size)
pub const MAX_VISIBLE_LINES: usize = 50;

/// Maximum scrollback lines kept in the ring buffer (heap-allocated)
pub const SCROLLBACK_MAX: usize = 60;

/// A single character cell with color attributes
#[derive(Clone, Copy)]
pub struct Cell {
    pub ch: u8,
    pub fg: Color,
    pub bg: Color,
}

impl Cell {
    pub const fn empty() -> Self {
        Self {
            ch: b' ',
            fg: Theme::TEXT_NORMAL,
            bg: Theme::WINDOW_BG,
        }
    }

    pub const fn new(ch: u8, fg: Color, bg: Color) -> Self {
        Self { ch, fg, bg }
    }
}

impl Default for Cell {
    fn default() -> Self {
        Self::empty()
    }
}

/// A single line in the terminal buffer
#[derive(Clone)]
pub struct Line {
    pub cells: [Cell; MAX_LINE_LENGTH],
    pub len: usize,
}

impl Line {
    pub const fn empty() -> Self {
        Self {
            cells: [Cell::empty(); MAX_LINE_LENGTH],
            len: 0,
        }
    }

    pub fn clear(&mut self) {
        for cell in self.cells.iter_mut() {
            *cell = Cell::empty()
        }
        self.len = 0;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn get(&self, index: usize) -> Option<&Cell> {
        if index < self.len {
            Some(&self.cells[index])
        } else {
            None
        }
    }

    pub fn set(&mut self, index: usize, cell: Cell) {
        if index < MAX_LINE_LENGTH {
            self.cells[index] = cell;
            if index >= self.len {
                self.len = index + 1;
            }
        }
    }

    pub fn push(&mut self, cell: Cell) -> bool {
        if self.len < MAX_LINE_LENGTH {
            self.cells[self.len] = cell;
            self.len += 1;
            true
        } else {
            false
        }
    }

    pub fn push_char(&mut self, ch: u8, fg: Color) -> bool {
        self.push(Cell::new(ch, fg, Theme::WINDOW_BG))
    }

    pub fn push_str(&mut self, s: &str, fg: Color) {
        for byte in s.bytes() {
            if !self.push_char(byte, fg) {
                break;
            }
        }
    }

    /// Copy contents from another Line
    pub fn copy_from(&mut self, src: &Line) {
        self.len = src.len;
        for i in 0..MAX_LINE_LENGTH {
            self.cells[i] = src.cells[i];
        }
    }
}

impl Default for Line {
    fn default() -> Self {
        Self::empty()
    }
}

/// Command line input buffer with editing support
pub struct InputBuffer {
    buffer: [u8; MAX_LINE_LENGTH],
    len: usize,
    cursor: usize,
}

impl InputBuffer {
    pub const fn new() -> Self {
        Self {
            buffer: [0u8; MAX_LINE_LENGTH],
            len: 0,
            cursor: 0,
        }
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.cursor = 0;
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Insert a character at the cursor position
    pub fn insert(&mut self, ch: u8) -> bool {
        if self.len >= MAX_LINE_LENGTH - 1 {
            return false;
        }

        // Shift characters right to make room
        for i in (self.cursor..self.len).rev() {
            self.buffer[i + 1] = self.buffer[i];
        }

        self.buffer[self.cursor] = ch;
        self.len += 1;
        self.cursor += 1;
        true
    }

    /// Delete the character before the cursor (backspace)
    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }

        // Shift characters left
        for i in self.cursor..self.len {
            self.buffer[i - 1] = self.buffer[i];
        }

        self.len -= 1;
        self.cursor -= 1;
        true
    }

    /// Delete the character at the cursor (delete key)
    pub fn delete(&mut self) -> bool {
        if self.cursor >= self.len {
            return false;
        }

        // Shift characters left
        for i in self.cursor + 1..self.len {
            self.buffer[i - 1] = self.buffer[i];
        }

        self.len -= 1;
        true
    }

    /// Move cursor left
    pub fn cursor_left(&mut self) -> bool {
        if self.cursor > 0 {
            self.cursor -= 1;
            true
        } else {
            false
        }
    }

    /// Move cursor right
    pub fn cursor_right(&mut self) -> bool {
        if self.cursor < self.len {
            self.cursor += 1;
            true
        } else {
            false
        }
    }

    /// Move cursor to beginning
    pub fn cursor_home(&mut self) {
        self.cursor = 0;
    }

    /// Move cursor to end
    pub fn cursor_end(&mut self) {
        self.cursor = self.len;
    }

    /// Get the current content as a string slice
    pub fn as_str(&self) -> &str {
        // Safety: We only insert valid ASCII characters
        unsafe { core::str::from_utf8_unchecked(&self.buffer[..self.len]) }
    }

    /// Get the current content as bytes
    pub fn as_bytes(&self) -> &[u8] {
        &self.buffer[..self.len]
    }

    /// Set content from a string (for history navigation)
    pub fn set(&mut self, s: &str) {
        self.clear();
        for byte in s.bytes() {
            if self.len < MAX_LINE_LENGTH - 1 {
                self.buffer[self.len] = byte;
                self.len += 1;
            }
        }
        self.cursor = self.len;
    }
}

impl Default for InputBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Command history buffer
pub struct History {
    entries: [[u8; MAX_LINE_LENGTH]; 32],
    lengths: [usize; 32],
    count: usize,
    index: usize, // Current navigation index
    capacity: usize,
}

impl History {
    pub const fn new() -> Self {
        Self {
            entries: [[0u8; MAX_LINE_LENGTH]; 32],
            lengths: [0usize; 32],
            count: 0,
            index: 0,
            capacity: 32,
        }
    }

    /// Add a command to history
    pub fn push(&mut self, cmd: &str) {
        if cmd.is_empty() {
            return;
        }

        // Don't add duplicate of last entry
        if self.count > 0 {
            let last_idx = (self.count - 1) % self.capacity;
            let last_len = self.lengths[last_idx];
            if last_len == cmd.len() {
                let last = &self.entries[last_idx][..last_len];
                if last == cmd.as_bytes() {
                    self.reset_navigation();
                    return;
                }
            }
        }

        let idx = self.count % self.capacity;
        let bytes = cmd.as_bytes();
        let len = bytes.len().min(MAX_LINE_LENGTH - 1);

        self.entries[idx][..len].copy_from_slice(&bytes[..len]);
        self.lengths[idx] = len;
        self.count += 1;

        self.reset_navigation();
    }

    /// Reset navigation index to end
    pub fn reset_navigation(&mut self) {
        self.index = self.count;
    }

    /// Navigate to previous entry (up arrow)
    pub fn previous(&mut self) -> Option<&str> {
        if self.count == 0 || self.index == 0 {
            return None;
        }

        self.index -= 1;
        self.get_current()
    }

    /// Navigate to next entry (down arrow)
    pub fn next(&mut self) -> Option<&str> {
        if self.index >= self.count {
            return None;
        }

        self.index += 1;
        if self.index >= self.count {
            None // Return to empty input
        } else {
            self.get_current()
        }
    }

    /// Get the current history entry
    fn get_current(&self) -> Option<&str> {
        if self.index >= self.count {
            return None;
        }

        let start = if self.count <= self.capacity {
            0
        } else {
            self.count - self.capacity
        };

        if self.index < start {
            return None;
        }

        let idx = self.index % self.capacity;
        let len = self.lengths[idx];

        // Safety: We only store valid ASCII
        Some(unsafe { core::str::from_utf8_unchecked(&self.entries[idx][..len]) })
    }
}

impl Default for History {
    fn default() -> Self {
        Self::new()
    }
}

/// Terminal display buffer managing visible lines and scrollback
pub struct DisplayBuffer {
    // Visible lines array (static allocation for no_std)
    lines: [Line; MAX_VISIBLE_LINES],
    // Number of lines currently in use
    line_count: usize,
    // Current cursor row (within visible area)
    cursor_row: usize,
    // Current cursor column
    cursor_col: usize,
    // Maximum visible rows (set from window config)
    max_rows: usize,
    // Maximum columns
    max_cols: usize,

    // ── Scrollback ring buffer (heap-allocated to avoid large stack frames) ──
    // Vec is pre-allocated to SCROLLBACK_MAX capacity, never grows.
    scrollback: Vec<Line>,
    // Index of the *next* write slot (ring head)
    scrollback_head: usize,
    // How many scrollback entries are valid (0..=SCROLLBACK_MAX)
    scrollback_count: usize,

    // ── View state ──
    // 0 = showing current content (bottom).  N = scrolled N lines above bottom.
    view_offset: usize,
}

impl DisplayBuffer {
    pub fn new() -> Self {
        const EMPTY_LINE: Line = Line::empty();
        // Heap-allocate the scrollback buffer so the struct itself (on the
        // calling function's stack) doesn't include 60 × ~1.8 KB of line data.
        let mut scrollback = Vec::with_capacity(SCROLLBACK_MAX);
        for _ in 0..SCROLLBACK_MAX {
            scrollback.push(Line::empty());
        }
        Self {
            lines: [EMPTY_LINE; MAX_VISIBLE_LINES],
            line_count: 0,
            cursor_row: 0,
            cursor_col: 0,
            max_rows: 25,
            max_cols: 80,
            scrollback,
            scrollback_head: 0,
            scrollback_count: 0,
            view_offset: 0,
        }
    }

    /// Initialize with actual window dimensions
    pub fn set_dimensions(&mut self, rows: usize, cols: usize) {
        self.max_rows = rows.min(MAX_VISIBLE_LINES);
        self.max_cols = cols.min(MAX_LINE_LENGTH);
    }

    /// Get current dimensions
    pub fn dimensions(&self) -> (usize, usize) {
        (self.max_rows, self.max_cols)
    }

    /// Get cursor position
    pub fn cursor_position(&self) -> (usize, usize) {
        (self.cursor_row, self.cursor_col)
    }

    // ── Scrollback helpers ────────────────────────────────────────────────────

    /// Save a line to the scrollback ring buffer.
    fn push_scrollback(&mut self, line: &Line) {
        let idx = self.scrollback_head;
        self.scrollback[idx].copy_from(line);
        self.scrollback_head = (self.scrollback_head + 1) % SCROLLBACK_MAX;
        if self.scrollback_count < SCROLLBACK_MAX {
            self.scrollback_count += 1;
        }
    }

    /// Return the `i`-th oldest scrollback line (0 = oldest, count-1 = newest).
    fn get_scrollback_entry(&self, i: usize) -> Option<&Line> {
        if i >= self.scrollback_count {
            return None;
        }
        // When the buffer is not yet full, oldest is slot 0.
        // When full, oldest is `scrollback_head` (the slot about to be overwritten).
        let oldest = if self.scrollback_count < SCROLLBACK_MAX {
            0
        } else {
            self.scrollback_head
        };
        let idx = (oldest + i) % SCROLLBACK_MAX;
        Some(&self.scrollback[idx])
    }

    // ── View-offset scrolling API ─────────────────────────────────────────────

    /// Number of lines the user can scroll back (= number of scrollback entries).
    pub fn max_view_offset(&self) -> usize {
        self.scrollback_count
    }

    /// Current scroll offset (0 = bottom / live view).
    pub fn view_offset(&self) -> usize {
        self.view_offset
    }

    /// Number of visible rows (needed by the scrollbar renderer).
    pub fn max_rows(&self) -> usize {
        self.max_rows
    }

    /// Scroll the view upward by `n` lines (toward older content).
    pub fn scroll_view_up(&mut self, n: usize) {
        self.view_offset = (self.view_offset + n).min(self.scrollback_count);
    }

    /// Scroll the view downward by `n` lines (toward newer content).
    pub fn scroll_view_down(&mut self, n: usize) {
        self.view_offset = self.view_offset.saturating_sub(n);
    }

    /// Jump to the bottom (live content).
    pub fn scroll_view_to_bottom(&mut self) {
        self.view_offset = 0;
    }

    /// Set the view offset directly (used by scrollbar drag).
    pub fn set_view_offset(&mut self, offset: usize) {
        self.view_offset = offset.min(self.scrollback_count);
    }

    // ── Content rendering ─────────────────────────────────────────────────────

    /// Get a line for rendering, taking `view_offset` into account.
    ///
    /// The virtual content sequence is:
    ///   `[scrollback_0 .. scrollback_{n-1}, lines_0 .. lines_{max_rows-1}]`
    ///                                        ^-- newest at the bottom
    ///
    /// `screen_row` 0 is the topmost row on screen.
    pub fn get_line_at_view(&self, screen_row: usize) -> Option<&Line> {
        if self.view_offset == 0 {
            // Fast path: normal bottom view.
            return self.get_line(screen_row);
        }

        // The virtual index where the screen's first row begins:
        //   virtual_start = scrollback_count - view_offset
        // (clamped to 0 if view_offset > scrollback_count, though set_view_offset prevents that)
        let virtual_start = self.scrollback_count.saturating_sub(self.view_offset);
        let virtual_idx   = virtual_start + screen_row;

        if virtual_idx < self.scrollback_count {
            self.get_scrollback_entry(virtual_idx)
        } else {
            let display_row = virtual_idx - self.scrollback_count;
            self.get_line(display_row)
        }
    }

    /// Get a visible-area line for rendering (no scroll offset applied).
    pub fn get_line(&self, row: usize) -> Option<&Line> {
        if row < self.max_rows && row < self.line_count {
            Some(&self.lines[row])
        } else {
            None
        }
    }

    // ── Mutation ──────────────────────────────────────────────────────────────

    /// Clear the entire display (visible area + scrollback + view offset).
    pub fn clear(&mut self) {
        for line in self.lines.iter_mut() {
            line.clear();
        }
        self.line_count = 0;
        self.cursor_row = 0;
        self.cursor_col = 0;
        // Reset scrollback and view
        for line in self.scrollback.iter_mut() {
            line.clear();
        }
        self.scrollback_head = 0;
        self.scrollback_count = 0;
        self.view_offset = 0;
    }

    /// Write a character at the cursor position
    pub fn write_char(&mut self, ch: u8, fg: Color) {
        if ch == b'\n' {
            self.newline();
            return;
        }

        if ch == b'\r' {
            self.cursor_col = 0;
            return;
        }

        if ch == 0x08 {
            // Backspace
            if self.cursor_col > 0 {
                self.cursor_col -= 1;
            }
            return;
        }

        // Ensure current line exists
        while self.line_count <= self.cursor_row {
            self.line_count += 1;
        }

        // Write character
        let cell = Cell::new(ch, fg, Theme::WINDOW_BG);
        self.lines[self.cursor_row].set(self.cursor_col, cell);
        self.cursor_col += 1;

        // Handle line wrap
        if self.cursor_col >= self.max_cols {
            self.newline();
        }
    }

    /// Write a string at the cursor position
    pub fn write_str(&mut self, s: &str, fg: Color) {
        for byte in s.bytes() {
            self.write_char(byte, fg);
        }
    }

    /// Write a string with newline
    pub fn writeln(&mut self, s: &str, fg: Color) {
        self.write_str(s, fg);
        self.newline();
    }

    /// Move to a new line
    pub fn newline(&mut self) {
        self.cursor_col = 0;
        self.cursor_row += 1;

        // Scroll if needed
        if self.cursor_row >= self.max_rows {
            self.scroll_up(1);
            self.cursor_row = self.max_rows - 1;
        }

        // Ensure line exists and is clear
        while self.line_count <= self.cursor_row {
            self.line_count += 1;
        }
        self.lines[self.cursor_row].clear();
    }

    /// Scroll the visible content upward by `n` lines, saving scrolled-off
    /// lines into the scrollback ring buffer.
    fn scroll_up(&mut self, n: usize) {
        if n >= self.max_rows {
            // Save all visible lines, then clear.
            let limit = self.line_count.min(self.max_rows);
            for i in 0..limit {
                // Clone line i to a local so we can &mut self for push_scrollback.
                let saved = self.lines[i].clone();
                self.push_scrollback(&saved);
            }
            // Clear visible area only (preserve scrollback).
            for line in self.lines.iter_mut() {
                line.clear();
            }
            self.line_count = 0;
            self.cursor_row = 0;
            self.cursor_col = 0;
            return;
        }

        // Save the first `n` lines (they'll be overwritten by the shift).
        for i in 0..n {
            if i < self.line_count {
                let saved = self.lines[i].clone();
                self.push_scrollback(&saved);
            }
        }

        // Shift remaining lines upward.
        for i in 0..(self.max_rows - n) {
            let src = i + n;
            if src < self.max_rows {
                for j in 0..MAX_LINE_LENGTH {
                    self.lines[i].cells[j] = self.lines[src].cells[j];
                }
                self.lines[i].len = self.lines[src].len;
            }
        }

        // Clear the new empty rows at the bottom.
        for i in (self.max_rows - n)..self.max_rows {
            self.lines[i].clear();
        }

        // Adjust line_count.
        self.line_count = self.line_count.saturating_sub(n).max(self.max_rows - n);
    }

    /// Set cursor position (for prompt rendering)
    pub fn set_cursor(&mut self, row: usize, col: usize) {
        self.cursor_row = row.min(self.max_rows.saturating_sub(1));
        self.cursor_col = col.min(self.max_cols.saturating_sub(1));
    }
}

impl Default for DisplayBuffer {
    fn default() -> Self {
        Self::new()
    }
}
