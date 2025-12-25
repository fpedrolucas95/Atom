#ifndef SYSCALLS_H
#define SYSCALLS_H

#include <stdint.h>

// Syscall numbers
#define SYS_THREAD_YIELD 0
#define SYS_THREAD_EXIT 1
#define SYS_THREAD_SLEEP 2
#define SYS_THREAD_CREATE 3
#define SYS_IPC_CREATE_PORT 4
#define SYS_IPC_CLOSE_PORT 5
#define SYS_IPC_SEND 6
#define SYS_IPC_RECV 7
#define SYS_MAP_FRAMEBUFFER 34
#define SYS_IO_OUTB 35
#define SYS_IO_INB 36
#define SYS_IO_OUTW 37
#define SYS_IO_INW 38
#define SYS_REGISTER_IRQ_HANDLER 39

// Error codes
#define ESUCCESS 0
#define EINVAL ((uint64_t)-2)
#define ENOSYS ((uint64_t)-3)
#define ENOMEM ((uint64_t)-4)
#define EPERM ((uint64_t)-5)
#define EBUSY ((uint64_t)-6)
#define EMSGSIZE ((uint64_t)-7)
#define ETIMEDOUT ((uint64_t)-8)
#define EWOULDBLOCK ((uint64_t)-9)
#define EDEADLK ((uint64_t)-10)

// Syscall wrapper - performs syscall with up to 6 arguments
static inline uint64_t syscall(uint64_t num, uint64_t arg0, uint64_t arg1,
                               uint64_t arg2, uint64_t arg3, uint64_t arg4, uint64_t arg5) {
    uint64_t ret;
    register uint64_t r10 __asm__("r10") = arg3;
    register uint64_t r8 __asm__("r8") = arg4;
    register uint64_t r9 __asm__("r9") = arg5;

    __asm__ volatile(
        "syscall"
        : "=a"(ret)
        : "a"(num), "D"(arg0), "S"(arg1), "d"(arg2), "r"(r10), "r"(r8), "r"(r9)
        : "rcx", "r11", "memory"
    );

    return ret;
}

// Simplified syscall wrappers
static inline uint64_t syscall0(uint64_t num) {
    return syscall(num, 0, 0, 0, 0, 0, 0);
}

static inline uint64_t syscall1(uint64_t num, uint64_t arg0) {
    return syscall(num, arg0, 0, 0, 0, 0, 0);
}

static inline uint64_t syscall2(uint64_t num, uint64_t arg0, uint64_t arg1) {
    return syscall(num, arg0, arg1, 0, 0, 0, 0);
}

static inline uint64_t syscall3(uint64_t num, uint64_t arg0, uint64_t arg1, uint64_t arg2) {
    return syscall(num, arg0, arg1, arg2, 0, 0, 0);
}

// High-level syscall helpers
static inline void thread_yield(void) {
    syscall0(SYS_THREAD_YIELD);
}

static inline void thread_exit(uint64_t code) {
    syscall1(SYS_THREAD_EXIT, code);
}

static inline void thread_sleep(uint64_t ticks) {
    syscall1(SYS_THREAD_SLEEP, ticks);
}

static inline uint64_t ipc_create_port(void) {
    return syscall0(SYS_IPC_CREATE_PORT);
}

static inline uint64_t ipc_send(uint64_t port, uint64_t type, uint64_t len, uint64_t timeout) {
    return syscall(SYS_IPC_SEND, port, type, len, timeout, 0, 0);
}

static inline uint64_t ipc_recv(uint64_t port, uint64_t buffer, uint64_t size, uint64_t timeout) {
    return syscall(SYS_IPC_RECV, port, buffer, size, timeout, 0, 0);
}

static inline uint64_t map_framebuffer(uint64_t virt_addr, uint64_t as_id) {
    return syscall2(SYS_MAP_FRAMEBUFFER, virt_addr, as_id);
}

static inline uint64_t io_outb(uint16_t port, uint8_t value) {
    return syscall2(SYS_IO_OUTB, port, value);
}

static inline uint64_t io_inb(uint16_t port) {
    return syscall1(SYS_IO_INB, port);
}

static inline uint64_t io_outw(uint16_t port, uint16_t value) {
    return syscall2(SYS_IO_OUTW, port, value);
}

static inline uint64_t io_inw(uint16_t port) {
    return syscall1(SYS_IO_INW, port);
}

static inline uint64_t register_irq_handler(uint8_t irq, uint64_t port_id) {
    return syscall2(SYS_REGISTER_IRQ_HANDLER, irq, port_id);
}

#endif // SYSCALLS_H
