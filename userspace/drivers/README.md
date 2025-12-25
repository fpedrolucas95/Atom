# User Space Drivers

This directory contains device drivers that run in user space, isolated from the kernel.

## Architecture

The Atom kernel implements a microkernel architecture where drivers run as separate user-space processes. This provides:

- **Isolation**: Driver bugs don't crash the kernel
- **Security**: Capability-based access control for hardware
- **Flexibility**: Drivers can be loaded/unloaded dynamically
- **Reliability**: Failed drivers can be restarted without rebooting

## Available Drivers

### PS/2 Mouse Driver (`ps2_mouse.c`)

Full PS/2 mouse driver supporting:
- 3-byte PS/2 mouse packets
- Movement delta tracking (X/Y)
- Button state detection (left, right, middle)
- Overflow detection and packet alignment
- IRQ12 handling via IPC

**Hardware Access:**
- I/O Ports: 0x60 (data), 0x64 (command/status)
- IRQ: 12

### PS/2 Keyboard Driver (`ps2_keyboard.c`)

Full PS/2 keyboard driver supporting:
- Scancode Set 1 translation
- ASCII conversion with shift/caps lock
- Modifier keys (Shift, Ctrl, Alt, Caps Lock)
- Extended scancodes (0xE0 prefix)
- IRQ1 handling via IPC

**Hardware Access:**
- I/O Ports: 0x60 (data), 0x64 (status)
- IRQ: 1

## System Calls

Drivers use the following syscalls (defined in `syscalls.h`):

### Hardware I/O
- `SYS_IO_OUTB (35)` - Output byte to I/O port
- `SYS_IO_INB (36)` - Input byte from I/O port
- `SYS_IO_OUTW (37)` - Output word to I/O port
- `SYS_IO_INW (38)` - Input word from I/O port

### IRQ Handling
- `SYS_REGISTER_IRQ_HANDLER (39)` - Register for IRQ notifications via IPC

### Framebuffer Access
- `SYS_MAP_FRAMEBUFFER (34)` - Map framebuffer to user space

### IPC
- `SYS_IPC_CREATE_PORT (4)` - Create IPC port
- `SYS_IPC_SEND (6)` - Send IPC message
- `SYS_IPC_RECV (7)` - Receive IPC message

## Security Model

All hardware access is protected by the capability system:

1. **I/O Port Capabilities**: Required for `io_inb/outb/inw/outw`
   - Capability type: `ResourceType::IoPort { port_num }`
   - Permissions: `READ` for input, `WRITE` for output

2. **IRQ Capabilities**: Required for `register_irq_handler`
   - Capability type: `ResourceType::Irq { irq_num }`
   - Permissions: `WRITE` to register handler

3. **IPC Ports**: Communication channel for IRQ notifications
   - Created by driver on initialization
   - Registered with kernel for IRQ forwarding

## How It Works

### Driver Initialization Flow

```
1. User space driver starts
2. Creates IPC port for receiving IRQs
3. Requests I/O port capabilities (via init process)
4. Requests IRQ capability (via init process)
5. Registers IRQ handler → IPC port mapping
6. Initializes hardware via I/O ports
7. Enters main loop waiting for IRQ notifications
```

### IRQ Handling Flow

```
1. Hardware generates interrupt (e.g., key pressed)
2. CPU jumps to kernel IRQ handler
3. Kernel forwards IRQ to registered IPC port
4. Driver receives IPC message with IRQ number
5. Driver reads hardware data via I/O ports
6. Driver processes data and sends to UI/consumers
7. Driver yields and waits for next IRQ
```

## Building Drivers

**Note**: Currently, the drivers are provided as C source code. To run them as user-space executables, they need to be compiled to the ATXF executable format.

### Requirements for ATXF Compilation

1. **Freestanding C compiler** (no libc)
2. **Custom linker script** for ATXF format
3. **ATXF header generator**

### ATXF Format Structure

```c
struct AtxfHeader {
    uint32_t magic;        // 0x41545846 ("ATXF")
    uint16_t version;      // 1
    uint16_t header_size;  // sizeof(AtxfHeader)
    uint32_t entry_offset; // Offset to _start within .text
    uint32_t text_offset;  // Must be page-aligned (4096)
    uint32_t text_size;
    uint32_t data_offset;  // Must be page-aligned
    uint32_t data_size;
    uint32_t bss_size;
};
```

### Example Build Command (conceptual)

```bash
# Compile to object file
gcc -m64 -ffreestanding -fno-pic -nostdlib -c ps2_mouse.c -o ps2_mouse.o

# Link with custom linker script
ld -T atxf.ld ps2_mouse.o -o ps2_mouse.elf

# Convert to ATXF format
./elf2atxf ps2_mouse.elf mouse_driver.atxf
```

## Loading Drivers

Drivers are loaded by the init process during system initialization:

```rust
// In init_process.rs
executable::load_into_address_space(
    driver_image,      // ATXF binary
    driver_as,         // Separate address space
    driver_thread_id   // Owner thread
)
```

Capabilities are granted to the driver thread:
```rust
// Grant I/O port access
cap_manager.create_capability(
    ResourceType::IoPort { port_num: 0x60 },
    CapPermissions::READ | CapPermissions::WRITE,
    driver_thread_id
);

// Grant IRQ access
cap_manager.create_capability(
    ResourceType::Irq { irq_num: 12 },
    CapPermissions::WRITE,
    driver_thread_id
);
```

## Current Status

✅ **Implemented:**
- Syscalls for I/O and IRQ handling
- IRQ forwarding mechanism (kernel → IPC)
- Driver source code (C)
- Capability system integration

❌ **TODO:**
- Build toolchain (C → ATXF)
- Init process integration
- IPC protocol for driver ↔ UI communication
- Dynamic driver loading/unloading

## Testing

Once compiled, drivers can be tested by:
1. Loading them via init process
2. Monitoring kernel logs for IRQ forwarding
3. Checking IPC message delivery
4. Verifying hardware interaction via serial output

## References

- [OSDev PS/2 Mouse](https://wiki.osdev.org/PS/2_Mouse)
- [OSDev PS/2 Keyboard](https://wiki.osdev.org/PS/2_Keyboard)
- Atom kernel documentation: `/kernel/src/`
