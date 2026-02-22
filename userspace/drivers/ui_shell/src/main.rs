//! Atom Desktop Environment Main Entry Point

#![no_std]
#![no_main]
#![feature(alloc_error_handler)]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicUsize, Ordering};

struct BumpAllocator {
    start: AtomicUsize,
    size: AtomicUsize,
    next: AtomicUsize,
}

unsafe impl Sync for BumpAllocator {}

impl BumpAllocator {
    const fn new() -> Self {
        Self {
            start: AtomicUsize::new(0),
            size: AtomicUsize::new(0),
            next: AtomicUsize::new(0),
        }
    }

    fn init(&self, start: usize, size: usize) {
        self.start.store(start, Ordering::SeqCst);
        self.size.store(size, Ordering::SeqCst);
        self.next.store(0, Ordering::SeqCst);
    }
}

unsafe impl GlobalAlloc for BumpAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let size = layout.size();
        let align = layout.align().max(16);
        let heap_start = self.start.load(Ordering::Relaxed);
        let heap_size = self.size.load(Ordering::Relaxed);
        if heap_start == 0 || heap_size == 0 { return core::ptr::null_mut(); }
        loop {
            let current = self.next.load(Ordering::Relaxed);
            let aligned = (current + align - 1) & !(align - 1);
            let new_next = aligned + size;
            if new_next > heap_size { return core::ptr::null_mut(); }
            if self.next.compare_exchange_weak(current, new_next, Ordering::SeqCst, Ordering::Relaxed).is_ok() {
                return (heap_start as *mut u8).add(aligned);
            }
        }
    }
    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static ALLOCATOR: BumpAllocator = BumpAllocator::new();

#[alloc_error_handler]
fn alloc_error(_layout: Layout) -> ! { loop {} }

use core::panic::PanicInfo;
use atom_syscall::graphics::{get_framebuffer, shared_region_create, shared_region_map, SharedMemFlags, Framebuffer};
use atom_syscall::thread::exit;
use atom_syscall::debug::log;
use atom_syscall::ipc::{try_recv, wait_any, send, PortId};

use desktop_compositor::Compositor;
use desktop_wm::WindowManager;
use desktop_shell::Shell;
use desktop_input::InputHandler;
use desktop_ipc::DesktopIpc;
use desktop_gfx::Rect;

use libipc::messages::{MessageHeader, MessageType, AppRegisterMsg, SurfacePresentMsg, WmRequestType, WmCreateWindowRequest, WmCreateWindowResponse};
use libipc::protocol::register_service;

#[no_mangle]
pub extern "C" fn _start() -> ! { main() }

#[no_mangle]
pub extern "efiapi" fn efi_main(_ih: *const core::ffi::c_void, _st: *const core::ffi::c_void) -> usize { main() }

fn main() -> ! {
    log("Atom OS Desktop v2.0 Starting...");
    let fb_info = match get_framebuffer() { Some(info) => info, None => exit(1) };
    let fb_size = fb_info.stride as usize * fb_info.height as usize * 4;
    let heap_size = fb_size + 16 * 1024 * 1024;
    let region_id = match shared_region_create(heap_size) { Ok(id) => id, Err(_) => exit(1) };
    let heap_start = match shared_region_map(region_id, 0, SharedMemFlags::READ_WRITE) { Ok(addr) => addr, Err(_) => exit(1) };
    ALLOCATOR.init(heap_start, heap_size);

    let fb = Framebuffer::new().expect("fb");
    let screen_w = fb.width();
    let screen_h = fb.height();

    let mut compositor = Compositor::new(fb);
    let mut wm = WindowManager::new(screen_w, screen_h);
    let mut shell = Shell::new();
    let mut input = InputHandler::new(screen_w, screen_h);
    let ipc = DesktopIpc::new();

    let _ = register_service("compositor", ipc.event_port);
    let _ = register_service("compositor.register", ipc.register_port);

    log("Desktop Ready.");
    compositor.mark_dirty(Rect::new(0, 0, screen_w, screen_h));

    let mut buffer = [0u8; 1024];
    loop {
        while let Ok(Some(len)) = try_recv(ipc.register_port, &mut buffer) {
            handle_registration(&buffer[..len], &mut wm, &ipc);
        }

        while let Ok(Some(len)) = try_recv(ipc.event_port, &mut buffer) {
            handle_event(&buffer[..len], &mut wm, &mut compositor);
        }

        input.poll_and_handle(&mut wm, &mut compositor, &mut shell);

        compositor.render(&wm, &shell, input.mouse_x, input.mouse_y);

        let _ = wait_any(&[ipc.register_port, ipc.event_port], 10);
    }
}

fn handle_registration(data: &[u8], _wm: &mut WindowManager, _ipc: &DesktopIpc) {
    let header = match MessageHeader::from_bytes(data) { Some(h) => h, None => return };
    match header.msg_type {
        MessageType::AppRegister => {
            if let Some(_msg) = AppRegisterMsg::from_bytes(&data[MessageHeader::SIZE..]) {
                log("App registered");
            }
        }
        _ => {}
    }
}

fn handle_event(data: &[u8], wm: &mut WindowManager, compositor: &mut Compositor) {
    let header = match MessageHeader::from_bytes(data) { Some(h) => h, None => return };
    match header.msg_type {
        MessageType::WmRequest => {
            let payload = &data[MessageHeader::SIZE..];
            if payload.len() < 4 { return; }
            let req_type_raw = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
            let req_type = match WmRequestType::from_u32(req_type_raw) { Some(t) => t, None => return };
            match req_type {
                WmRequestType::CreateWindow => {
                    if let Some(req) = WmCreateWindowRequest::from_bytes(payload) {
                        let id = wm.create_window(&req.title, req.width, req.height);
                        if let Some(w) = wm.get_window_mut(id) {
                            w.event_port = Some(req.reply_port as PortId);
                            let resp = WmCreateWindowResponse {
                                window_id: id,
                                region_id: w.surface.as_ref().map(|s| s.region_id()).unwrap_or(0),
                                width: w.content_rect().width,
                                height: w.content_rect().height,
                                stride: w.content_rect().width,
                            };
                            let resp_header = MessageHeader::new(MessageType::WmResponse, WmCreateWindowResponse::SIZE as u32);
                            let mut full_resp = [0u8; 64];
                            full_resp[..16].copy_from_slice(&resp_header.to_bytes());
                            full_resp[16..16 + 24].copy_from_slice(&resp.to_bytes());
                            let _ = send(req.reply_port as PortId, &full_resp[..40]);
                            compositor.mark_dirty(Rect::new(0, 0, compositor.width(), compositor.height()));
                        }
                    }
                }
                WmRequestType::DestroyWindow => {
                    if payload.len() >= 8 {
                        let wid = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
                        wm.destroy_window(wid);
                        compositor.mark_dirty(Rect::new(0, 0, compositor.width(), compositor.height()));
                    }
                }
                _ => {}
            }
        }
        MessageType::SurfacePresent => {
             if let Some(msg) = SurfacePresentMsg::from_bytes(&data[MessageHeader::SIZE..]) {
                if let Some(w) = wm.get_window_mut(msg.window_id) {
                    let content = w.content_rect();
                    compositor.mark_dirty(Rect::new(content.x + msg.dirty_x, content.y + msg.dirty_y, msg.dirty_w, msg.dirty_h));
                }
            }
        }
        _ => {}
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! { log("Desktop Panic!"); exit(0xFF) }
