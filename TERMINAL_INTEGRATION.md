# Terminal Integration Implementation

## Overview
This document describes the corrected terminal integration implementation following proper separation of concerns: ui_shell manages windows as abstract containers, while the terminal remains a pure userspace application.

## Architecture Principles

### Critical: Applications Must NOT Know About Windows
**The terminal application has NO knowledge of:**
- Windows or window decorations
- Close buttons or window chrome  
- Screen coordinates or window positioning
- The compositor or ui_shell

**The terminal ONLY:**
- Polls keyboard input directly
- Renders terminal content to framebuffer
- Processes commands and displays output
- Runs as an independent userspace process

### ui_shell Responsibilities
**ui_shell manages windows as abstract containers:**
- Creates/destroys window containers
- Provides window chrome (title bar, borders, close button)
- Manages focus and z-order
- Composites windows onto framebuffer
- Routes input to applications (future: via IPC)

## What Has Been Implemented

### Phase 1: Dock with Terminal Icon ✅
- Added macOS-style bottom dock to the compositor (centered, semi-transparent)
- Single Terminal icon (">_") in the dock
- Implemented dock icon tracking with click detection
- Dock click handler:
  - Creates new terminal window container if none exists
  - Focuses existing terminal window if already open
- Added window type tracking (Static vs Terminal) for lifecycle management

### Phase 2: Window Management ✅
- Removed static terminal window from compositor initialization
- Terminal window container created dynamically when dock icon is clicked
- Implemented proper window focus management:
  - Clicking window brings it to focus and top of z-order
  - Close button removes window container
- Window lifecycle tracking:
  - Tracks if terminal window exists
  - Clears tracking when window closed
  - Updates focus to next window or none

### Phase 3: Terminal Independence ✅
- Terminal remains UNCHANGED from original implementation
- Terminal continues to poll keyboard directly via `keyboard_poll()`
- Terminal renders to framebuffer independently
- NO IPC port creation in terminal
- NO window awareness in terminal
- Terminal works standalone or when "hosted" in ui_shell window

### Code Structure
```
userspace/
├── drivers/
│   ├── ui_shell/           # Desktop compositor/window manager
│   │   ├── src/main.rs     # Main compositor logic
│   │   │   ├── Dock management (icons, click detection)
│   │   │   ├── Window management (focus, close, z-order)
│   │   │   ├── Window chrome rendering
│   │   │   └── Dock rendering
│   │   └── Cargo.toml
│   │
│   └── terminal/           # Terminal application (UNCHANGED)
│       ├── src/
│       │   ├── main.rs     # Polls keyboard, renders content
│       │   ├── input.rs    # Keyboard event processing
│       │   ├── window.rs   # Terminal rendering (NOT window management)
│       │   └── ...
│       └── Cargo.toml
│
└── libs/
    ├── libipc/             # IPC message definitions
    └── syscall/            # System call wrappers
        └── src/alloc.rs    # Shared allocator
```

## What Remains To Be Implemented

### Process Spawning 🚧
**Current State:** Clicking dock icon creates window container but doesn't spawn terminal process.

**Required:**
1. Kernel syscall for process spawning: `spawn_process(binary_path, args, capabilities)`
2. ui_shell sends spawn request when dock icon clicked
3. Service manager launches terminal binary
4. Terminal process runs independently
5. Close button sends termination signal to process

**Current Workaround:** Window container exists, but terminal process must be started separately.
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

## Corrected Architecture (Latest)

### Key Changes from Initial Implementation
The initial implementation incorrectly made the terminal "window-aware" by adding IPC ports and event routing. This has been corrected:

**Terminal (Now Correct):**
- NO IPC port creation for compositor events
- NO `event_port` field
- NO window/compositor knowledge
- Polls keyboard directly via `keyboard_poll()`
- Renders independently to framebuffer
- Works standalone, doesn't know it's in a window

**ui_shell (Now Correct):**
- Manages windows as abstract containers only
- Provides window chrome (title bar, close, borders)
- Does NOT draw fake terminal content
- Dock creates window containers
- Window is just chrome around where terminal draws

### Current Implementation Status
✅ Dock with Terminal icon (bottom center, macOS-style)
✅ Window container management (create, focus, close)
✅ Window chrome rendering (title bar, buttons, borders)
✅ Terminal remains pure userspace app (unchanged)
✅ Clean separation: ui_shell = windows, terminal = app

🚧 Process spawning (requires kernel syscall)
🚧 Framebuffer coordination (both can access, need boundaries)
🚧 Input routing to apps (terminal polls directly for now)

