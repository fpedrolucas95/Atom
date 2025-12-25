// PS/2 Mouse Driver (User Space)
// Based on OSDev PS/2 Mouse documentation

#include "syscalls.h"

// PS/2 Controller Ports
#define PS2_DATA_PORT    0x60
#define PS2_STATUS_PORT  0x64
#define PS2_COMMAND_PORT 0x64

// Status register bits
#define STATUS_OUTPUT_FULL  0x01
#define STATUS_INPUT_FULL   0x02
#define STATUS_AUX_DATA     0x20

// Commands
#define CMD_READ_CONFIG     0x20
#define CMD_WRITE_CONFIG    0x60
#define CMD_ENABLE_AUX      0xA8

// Mouse commands
#define AUX_PREFIX                 0xD4
#define AUX_ENABLE_PACKET_STREAM   0xF4
#define AUX_SET_DEFAULTS           0xF6

// IRQ
#define MOUSE_IRQ 12

#define WAIT_SPINS 50000

typedef struct {
    uint8_t packet[3];
    uint8_t cycle;
    int8_t dx;
    int8_t dy;
} mouse_state_t;

static mouse_state_t mouse_state = {0};
static uint64_t irq_port = 0;

static uint8_t read_status(void) {
    return (uint8_t)io_inb(PS2_STATUS_PORT);
}

static uint8_t read_data(void) {
    return (uint8_t)io_inb(PS2_DATA_PORT);
}

static void wait_input_empty(void) {
    for (uint32_t i = 0; i < WAIT_SPINS; i++) {
        if ((read_status() & STATUS_INPUT_FULL) == 0) {
            return;
        }
    }
}

static void wait_output_full(void) {
    for (uint32_t i = 0; i < WAIT_SPINS; i++) {
        if (read_status() & STATUS_OUTPUT_FULL) {
            return;
        }
    }
}

static int aux_data_available(void) {
    uint8_t status = read_status();
    return (status & (STATUS_OUTPUT_FULL | STATUS_AUX_DATA)) == (STATUS_OUTPUT_FULL | STATUS_AUX_DATA);
}

static void drain_aux_buffer(void) {
    while (aux_data_available()) {
        read_data();
    }
}

static void mouse_write(uint8_t data) {
    wait_input_empty();
    io_outb(PS2_COMMAND_PORT, AUX_PREFIX);

    wait_input_empty();
    io_outb(PS2_DATA_PORT, data);
}

static uint8_t mouse_read(void) {
    wait_output_full();
    return read_data();
}

static uint8_t read_command_byte(void) {
    wait_input_empty();
    io_outb(PS2_COMMAND_PORT, CMD_READ_CONFIG);

    wait_output_full();
    return read_data();
}

static void write_command_byte(uint8_t config) {
    wait_input_empty();
    io_outb(PS2_COMMAND_PORT, CMD_WRITE_CONFIG);

    wait_input_empty();
    io_outb(PS2_DATA_PORT, config);
}

static void enable_aux_channel(void) {
    wait_input_empty();
    io_outb(PS2_COMMAND_PORT, CMD_ENABLE_AUX);
}

static void enable_interrupts_in_controller(void) {
    uint8_t config = read_command_byte();

    // Enable IRQ12 (bit 1) and IRQ1 (bit 0)
    config |= 0x03;
    // Disable clock (clear bit 5)
    config &= ~0x20;

    write_command_byte(config);
}

static int set_defaults_and_enable(void) {
    // Set defaults
    mouse_write(AUX_SET_DEFAULTS);
    uint8_t ack = mouse_read();
    if (ack != 0xFA) {
        return 0;  // Failed
    }

    // Enable streaming
    mouse_write(AUX_ENABLE_PACKET_STREAM);
    ack = mouse_read();
    if (ack != 0xFA) {
        return 0;  // Failed
    }

    return 1;  // Success
}

static void process_mouse_byte(uint8_t byte) {
    switch (mouse_state.cycle) {
        case 0:
            // First byte - check bit 3 for alignment
            if ((byte & 0x08) == 0) {
                return;  // Not aligned
            }
            mouse_state.packet[0] = byte;
            mouse_state.cycle = 1;
            break;

        case 1:
            mouse_state.packet[1] = byte;
            mouse_state.cycle = 2;
            break;

        case 2:
            mouse_state.packet[2] = byte;
            mouse_state.cycle = 0;

            // Process complete packet
            uint8_t flags = mouse_state.packet[0];

            // Check for overflow
            if ((flags & 0xC0) != 0) {
                return;  // Overflow, discard
            }

            // Get deltas (already signed)
            mouse_state.dx = (int8_t)mouse_state.packet[1];
            mouse_state.dy = (int8_t)mouse_state.packet[2];

            // TODO: Send delta via IPC to graphics server
            break;

        default:
            mouse_state.cycle = 0;
            break;
    }
}

static void handle_interrupt(void) {
    // Read all available AUX data
    while (aux_data_available()) {
        uint8_t byte = read_data();
        process_mouse_byte(byte);
    }
}

int mouse_init(void) {
    // Reset mouse state
    mouse_state.cycle = 0;
    mouse_state.dx = 0;
    mouse_state.dy = 0;

    // Create IPC port for receiving IRQs
    irq_port = ipc_create_port();
    if (irq_port == EINVAL) {
        return 0;  // Failed to create port
    }

    // Register for IRQ12
    if (register_irq_handler(MOUSE_IRQ, irq_port) != ESUCCESS) {
        return 0;  // Failed to register IRQ
    }

    // Initialize PS/2 mouse
    drain_aux_buffer();
    enable_aux_channel();
    enable_interrupts_in_controller();

    if (!set_defaults_and_enable()) {
        return 0;  // Failed to initialize
    }

    return 1;  // Success
}

void mouse_main_loop(void) {
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
    if (mouse_init()) {
        mouse_main_loop();
    }

    thread_exit(1);  // Exit with error if init failed
}
