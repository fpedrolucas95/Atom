# Terminal Integration Implementation

## Overview
This document describes the terminal integration implementation that enables proper interaction between the desktop compositor (ui_shell) and the terminal application.

## What Has Been Implemented

### Phase 1: Dock with Terminal Icon ✅
- Added macOS-style bottom dock to the compositor with semi-transparent background
- Implemented dock icon tracking with click detection
- Added Terminal icon (">_") to the dock alongside Files, Settings, and Browser icons
- Implemented dock click handler that:
  - Launches a new terminal window if none exists
  - Focuses existing terminal window if already running
- Added window type tracking (Static vs Terminal) for lifecycle management
- Added process tracking to prevent duplicate terminal instances

### Phase 2: Window Management ✅
- Removed static terminal window from compositor initialization
- Terminal window is now created dynamically when dock icon is clicked
- Implemented proper window focus management:
  - Clicking window brings it to focus
  - Only focused window receives keyboard input
- Close button properly tracks terminal lifecycle:
  - Removes window from compositor
  - Clears terminal process tracking
  - Updates focus to next window or none

### Phase 3: IPC-Based Input Routing ✅
- Added keyboard modifier state tracking in compositor (shift, ctrl, alt, caps_lock)
- Implemented IPC keyboard event sending:
  - Compositor creates IPC port for terminal window
  - Keyboard scancodes translated to KeyPress IPC messages
  - Messages include scancode, character, and modifiers
  - Only focused window receives keyboard events
- Modified terminal to receive keyboard via IPC:
  - Terminal creates IPC port on initialization
  - Event loop checks IPC port for keyboard messages
  - Falls back to direct keyboard polling for standalone mode
  - Processes scancodes through existing InputHandler
- Both compositor and terminal compile successfully

### Code Structure
```
userspace/
├── drivers/
│   ├── ui_shell/           # Desktop compositor
│   │   ├── src/main.rs     # Main compositor logic
│   │   │   ├── Dock management (icons, click detection)
│   │   │   ├── Window management (focus, close)
│   │   │   ├── Keyboard routing (modifiers, IPC sending)
│   │   │   └── Terminal launching
│   │   └── Cargo.toml
│   │
│   └── terminal/           # Terminal application
│       ├── src/
│       │   ├── main.rs     # Main event loop with IPC input
│       │   ├── input.rs    # Keyboard event processing
│       │   ├── window.rs   # Terminal rendering
│       │   └── ...
│       └── Cargo.toml
│
└── libs/
    └── libipc/             # IPC message definitions
        └── src/
            ├── messages.rs # KeyEvent, MessageType, etc.
            └── ports.rs    # Well-known port definitions
```

## What Remains To Be Implemented

### Phase 4: Framebuffer Management 🚧
**Current Issue**: Both compositor and terminal can call `Framebuffer::new()`, potentially creating conflicts.

**Required Changes**:

#### Option A: Shared Memory Surfaces (Recommended)
1. Add shared memory syscall support in kernel
2. Compositor allocates shared memory regions for each application window
3. Terminal renders to its dedicated surface buffer
4. Compositor composites all surfaces to hardware framebuffer
5. Only compositor has direct hardware framebuffer access

Implementation:
```rust
// In compositor
let surface_id = create_shared_surface(width, height);
window.surface_id = Some(surface_id);
send_surface_info_to_app(port, surface_id, width, height);

// In terminal
let surface = attach_shared_surface(surface_id);
terminal.render_to_surface(surface);
notify_compositor_of_changes(dirty_rect);
```

#### Option B: Draw Command IPC Protocol (Simpler)
1. Define IPC messages for basic drawing operations:
   - `DrawChar(x, y, char, fg_color, bg_color)`
   - `FillRect(x, y, width, height, color)`
   - `DrawString(x, y, text, fg_color, bg_color)`
2. Terminal sends draw commands to compositor via IPC
3. Compositor executes commands within terminal's window bounds
4. Terminal never calls `Framebuffer::new()`

Implementation:
```rust
// In terminal
fn render_char(&mut self, row: u32, col: u32, ch: u8, fg: Color, bg: Color) {
    let msg = DrawCharMessage { row, col, ch, fg, bg };
    send(self.compositor_port, &msg.to_bytes());
}

// In compositor
fn handle_draw_command(&mut self, window: &Window, cmd: DrawCommand) {
    let screen_x = window.x + cmd.x;
    let screen_y = window.y + cmd.y;
    // Execute drawing within window bounds
    self.fb.draw_char(screen_x, screen_y, cmd.ch, cmd.fg, cmd.bg);
}
```

### Phase 5: Process Management 🚧
**Current Issue**: Clicking dock icon creates a window but doesn't spawn actual terminal process.

**Required Changes**:
1. Implement process spawning in kernel or service manager
2. Compositor sends spawn request with IPC port ID
3. Service manager launches terminal binary with arguments
4. Terminal process connects to IPC port provided by compositor
5. Close button sends termination signal to process

Implementation requires:
- Kernel support for `spawn_process(path, args, capabilities)`
- Process ID tracking in compositor
- Signal handling for process termination
- Resource cleanup on process exit

## Testing Checklist

### Manual Testing (When System is Running)
- [ ] Boot OS and see dock at bottom with Terminal icon
- [ ] Click Terminal icon - window appears with title "Terminal"
- [ ] Click Terminal icon again - existing window is focused (no duplicate)
- [ ] Click on terminal window - it gains focus (title bar changes color)
- [ ] Click on another window - terminal loses focus
- [ ] Type on keyboard when terminal is focused - events are routed to terminal
- [ ] Type when terminal is not focused - events not sent to terminal
- [ ] Click close button on terminal - window disappears
- [ ] Click Terminal icon after closing - new window appears
- [ ] Test modifier keys (Shift, Ctrl, Alt) - proper events sent

### Integration Testing
- [ ] Multiple windows can be opened and managed independently
- [ ] Focus changes correctly between windows
- [ ] Keyboard events only go to focused window
- [ ] Window stacking order (z-order) is correct
- [ ] Dock icons respond to clicks properly
- [ ] Terminal content updates when receiving input (once framebuffer is fixed)

## Known Issues and Workarounds

### Issue 1: Terminal Process Not Spawned
**Problem**: Clicking dock icon creates window but doesn't launch actual terminal process.
**Workaround**: Window appears with placeholder content showing "Terminal Ready"
**Fix Required**: Implement process spawning in kernel

### Issue 2: Framebuffer Conflict
**Problem**: Both compositor and terminal can access hardware framebuffer directly
**Workaround**: None currently - they may draw over each other
**Fix Required**: Implement Option A or B from Phase 4 above

### Issue 3: No Backbuffer/Double Buffering
**Problem**: Direct framebuffer writes may cause tearing
**Workaround**: None
**Fix Required**: Implement double-buffering in compositor

## Architecture Decisions

### Why IPC for Input Routing?
- Maintains clear separation between compositor and applications
- Allows compositor to enforce window focus policy
- Enables future features like input recording, automation
- Consistent with microkernel design principles

### Why Bump Allocator?
- Simple and fast for no_std environment
- Sufficient for current needs (small allocations, no deallocation)
- Can be replaced with more sophisticated allocator later

### Why Simple Message Protocol?
- Minimal overhead for key events (15 bytes per event)
- Easy to extend with additional event types
- Compatible with existing libipc infrastructure

## Future Enhancements

### Short Term
1. Implement shared memory surfaces for application rendering
2. Add process spawning capability
3. Implement proper terminal lifecycle management
4. Add window minimize/maximize functionality

### Medium Term
1. Multiple terminal instances support
2. Terminal tabs or split panes
3. Copy/paste between windows
4. Window resize and drag functionality
5. Application menu bar

### Long Term
1. GPU-accelerated composition
2. Window animations and effects
3. Virtual desktops/workspaces
4. Advanced window management (tiling, snapping)
5. Theme customization

## Build Instructions

### Building Desktop Compositor
```bash
cd userspace/drivers/ui_shell
cargo build --target x86_64-unknown-uefi
```

### Building Terminal
```bash
cd userspace/drivers/terminal
cargo build --target x86_64-unknown-uefi
```

### Building Complete System
```bash
./build.sh
```

## References
- IPC Protocol: `userspace/libs/libipc/src/messages.rs`
- Syscall Interface: `userspace/libs/syscall/src/`
- Keyboard Scancodes: `userspace/drivers/terminal/src/input.rs`
