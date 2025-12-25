// PS/2 Keyboard Driver (User Space)
// Based on OSDev PS/2 Keyboard documentation

#include "syscalls.h"

// PS/2 Ports
#define PS2_DATA_PORT    0x60
#define PS2_STATUS_PORT  0x64

// Status register bits
#define STATUS_OUTPUT_FULL  0x01

// IRQ
#define KEYBOARD_IRQ 1

// Scancode set 1 translation table
static const uint8_t scancode_to_ascii[128] = {
    0,    0,    '1',  '2',  '3',  '4',  '5',  '6',  // 0x00-0x07
    '7',  '8',  '9',  '0',  '-',  '=',  '\b', '\t', // 0x08-0x0F
    'q',  'w',  'e',  'r',  't',  'y',  'u',  'i',  // 0x10-0x17
    'o',  'p',  '[',  ']',  '\n', 0,    'a',  's',  // 0x18-0x1F
    'd',  'f',  'g',  'h',  'j',  'k',  'l',  ';',  // 0x20-0x27
    '\'', '`',  0,    '\\', 'z',  'x',  'c',  'v',  // 0x28-0x2F
    'b',  'n',  'm',  ',',  '.',  '/',  0,    '*',  // 0x30-0x37
    0,    ' ',  0,    0,    0,    0,    0,    0,    // 0x38-0x3F
    0,    0,    0,    0,    0,    0,    0,    '7',  // 0x40-0x47
    '8',  '9',  '-',  '4',  '5',  '6',  '+',  '1',  // 0x48-0x4F
    '2',  '3',  '0',  '.',  0,    0,    0,    0,    // 0x50-0x57
    0,    0,    0,    0,    0,    0,    0,    0,    // 0x58-0x5F
    0,    0,    0,    0,    0,    0,    0,    0,    // 0x60-0x67
    0,    0,    0,    0,    0,    0,    0,    0,    // 0x68-0x6F
    0,    0,    0,    0,    0,    0,    0,    0,    // 0x70-0x77
    0,    0,    0,    0,    0,    0,    0,    0     // 0x78-0x7F
};

static const uint8_t scancode_to_ascii_shift[128] = {
    0,    0,    '!',  '@',  '#',  '$',  '%',  '^',  // 0x00-0x07
    '&',  '*',  '(',  ')',  '_',  '+',  '\b', '\t', // 0x08-0x0F
    'Q',  'W',  'E',  'R',  'T',  'Y',  'U',  'I',  // 0x10-0x17
    'O',  'P',  '{',  '}',  '\n', 0,    'A',  'S',  // 0x18-0x1F
    'D',  'F',  'G',  'H',  'J',  'K',  'L',  ':',  // 0x20-0x27
    '"',  '~',  0,    '|',  'Z',  'X',  'C',  'V',  // 0x28-0x2F
    'B',  'N',  'M',  '<',  '>',  '?',  0,    '*',  // 0x30-0x37
    0,    ' ',  0,    0,    0,    0,    0,    0,    // 0x38-0x3F
    0,    0,    0,    0,    0,    0,    0,    '7',  // 0x40-0x47
    '8',  '9',  '-',  '4',  '5',  '6',  '+',  '1',  // 0x48-0x4F
    '2',  '3',  '0',  '.',  0,    0,    0,    0,    // 0x50-0x57
    0,    0,    0,    0,    0,    0,    0,    0,    // 0x58-0x5F
    0,    0,    0,    0,    0,    0,    0,    0,    // 0x60-0x67
    0,    0,    0,    0,    0,    0,    0,    0,    // 0x68-0x6F
    0,    0,    0,    0,    0,    0,    0,    0,    // 0x70-0x77
    0,    0,    0,    0,    0,    0,    0,    0     // 0x78-0x7F
};

typedef struct {
    uint8_t shift;
    uint8_t ctrl;
    uint8_t alt;
    uint8_t caps_lock;
    uint8_t extended;
} keyboard_state_t;

static keyboard_state_t kbd_state = {0};
static uint64_t irq_port = 0;

static uint8_t read_scancode(void) {
    // Check if data is available
    uint8_t status = (uint8_t)io_inb(PS2_STATUS_PORT);
    if ((status & STATUS_OUTPUT_FULL) == 0) {
        return 0;
    }

    return (uint8_t)io_inb(PS2_DATA_PORT);
}

static uint8_t translate_scancode(uint8_t scancode) {
    if (kbd_state.shift) {
        return scancode_to_ascii_shift[scancode & 0x7F];
    } else {
        uint8_t ch = scancode_to_ascii[scancode & 0x7F];
        if (kbd_state.caps_lock && ch >= 'a' && ch <= 'z') {
            return ch - 32;  // Convert to uppercase
        }
        return ch;
    }
}

static void process_scancode(uint8_t scancode) {
    if (kbd_state.extended) {
        kbd_state.extended = 0;
        return;
    }

    if (scancode == 0xE0) {
        kbd_state.extended = 1;
        return;
    }

    uint8_t is_break = scancode & 0x80;
    uint8_t code = scancode & 0x7F;

    // Handle modifier keys
    switch (code) {
        case 0x2A:  // Left Shift
        case 0x36:  // Right Shift
            kbd_state.shift = !is_break;
            return;

        case 0x1D:  // Ctrl
            kbd_state.ctrl = !is_break;
            return;

        case 0x38:  // Alt
            kbd_state.alt = !is_break;
            return;

        case 0x3A:  // Caps Lock
            if (!is_break) {
                kbd_state.caps_lock = !kbd_state.caps_lock;
            }
            return;

        default:
            break;
    }

    // Ignore key releases
    if (is_break) {
        return;
    }

    // Translate scancode to ASCII
    uint8_t ascii = translate_scancode(code);
    if (ascii != 0) {
        // TODO: Send key via IPC to terminal/graphics server
    }
}

static void handle_interrupt(void) {
    // Read all available scancodes
    uint8_t scancode;
    while ((scancode = read_scancode()) != 0) {
        process_scancode(scancode);
    }
}

int keyboard_init(void) {
    // Reset keyboard state
    kbd_state.shift = 0;
    kbd_state.ctrl = 0;
    kbd_state.alt = 0;
    kbd_state.caps_lock = 0;
    kbd_state.extended = 0;

    // Create IPC port for receiving IRQs
    irq_port = ipc_create_port();
    if (irq_port == EINVAL) {
        return 0;  // Failed to create port
    }

    // Register for IRQ1
    if (register_irq_handler(KEYBOARD_IRQ, irq_port) != ESUCCESS) {
        return 0;  // Failed to register IRQ
    }

    return 1;  // Success
}

void keyboard_main_loop(void) {
    uint8_t buffer[64];

    while (1) {
        // Wait for IRQ notification via IPC
        uint64_t result = ipc_recv(irq_port, (uint64_t)buffer, sizeof(buffer), (uint64_t)-1);

        if (result != EWOULDBLOCK && result != ETIMEDOUT && result != EINVAL) {
            // IRQ received, handle it
            handle_interrupt();
        }

        // Yield to allow other processes to run
        thread_yield();
    }
}

// Entry point
void _start(void) {
    if (keyboard_init()) {
        keyboard_main_loop();
    }

    thread_exit(1);  // Exit with error if init failed
}
