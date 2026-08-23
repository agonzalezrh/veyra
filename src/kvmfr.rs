//! Looking Glass KVMFR/LGMP frame producer.
//!
//! Pluggable transport: tries IvshmemTransport first, then PosixShmTransport,
//! then falls back to simulated frames. The LGMP client + FrameProducer
//! are independent of which transport is active.

use std::ffi::CString;
use std::os::unix::io::RawFd;
use std::path::PathBuf;
use std::ptr;

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::gles::GlesTexture;
use smithay::backend::renderer::ImportMem;
use tracing::{info, warn};

use crate::input_router::{InputSink, PointerEventKind};
use crate::producer::{FrameProducer, FrameResult};

// ── LGMP FFI ──────────────────────────────────────────────────────────

mod ffi {
    use std::ffi::c_void;
    pub type LGMPClient = c_void;
    pub type LGMPClientQueue = c_void;

    pub const LGMP_Q_FRAME: u32 = 2;
    pub const LGMP_Q_CURSOR: u32 = 1;

    /// Cursor input message format expected by Looking Glass.
    /// type=0: move (x,y), type=1: button (button, pressed, 0, 0)
    #[repr(C, packed)]
    pub struct CursorChannelMessage {
        pub msg_type: u32,
        pub x: u32,
        pub y: u32,
        pub buttons: u32,
    }

    #[repr(C)]
    pub struct LGMPMessage {
        pub udata: u64,
        pub size: u32,
        pub mem: *mut c_void,
    }

    extern "C" {
        pub fn lgmpClientInit(mem: *mut c_void, size: usize, result: *mut *mut LGMPClient) -> i32;
        pub fn lgmpClientFree(client: *mut *mut LGMPClient);
        pub fn lgmpClientSessionInit(
            client: *mut LGMPClient, udata_size: *mut u32, udata: *mut *mut u8,
            client_id: *mut u32, remote_version: *mut u32,
        ) -> i32;
        pub fn lgmpClientSubscribe(
            client: *mut LGMPClient, queue_id: u32, result: *mut *mut LGMPClientQueue,
        ) -> i32;
        pub fn lgmpClientAdvanceToLast(queue: *mut LGMPClientQueue) -> i32;
        pub fn lgmpClientProcess(queue: *mut LGMPClientQueue, result: *mut LGMPMessage) -> i32;
        pub fn lgmpClientMessageDone(queue: *mut LGMPClientQueue) -> i32;
        pub fn lgmpClientSendData(
            queue: *mut LGMPClientQueue, data: *const u8, size: u32, serial: *mut u32,
        ) -> i32;
    }
}

// ── Transport abstraction ─────────────────────────────────────────────
// A transport provides the mapped memory and path name for LGMP.
// The mapping must stay alive for the LGMP client's lifetime.

struct LgmpMemoryMapping {
    mem: *mut u8,
    size: usize,
}

impl LgmpMemoryMapping {
    fn new(mem: *mut u8, size: usize) -> Self {
        LgmpMemoryMapping { mem, size }
    }

    fn ptr(&self) -> *mut libc::c_void {
        self.mem as *mut libc::c_void
    }

    fn len(&self) -> usize {
        self.size
    }
}

// ── POSIX SHM transport (for testing without KVMFR hardware) ──────────

struct PosixShmTransport;

impl PosixShmTransport {
    fn open(name: &str) -> Option<LgmpMemoryMapping> {
        let cname = CString::new(name).ok()?;
        let fd = unsafe { libc::shm_open(cname.as_ptr(), libc::O_RDWR, 0) };
        if fd < 0 { return None; }
        let size = unsafe { libc::lseek(fd, 0, libc::SEEK_END) };
        if size <= 0 { unsafe { libc::close(fd) }; return None; }
        let size = size as usize;
        let mem = unsafe {
            libc::mmap(ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED, fd, 0)
        };
        unsafe { libc::close(fd); }
        if mem == libc::MAP_FAILED { return None; }
        info!(?name, ?size, "POSIX SHM transport opened");
        Some(LgmpMemoryMapping::new(mem as *mut u8, size))
    }
}

// ── IVSHMEM transport (real Looking Glass hardware) ───────────────────

struct IvshmemTransport;

impl IvshmemTransport {
    /// Try to open /dev/kvmfr{N} for N=0..7
    fn open() -> Option<LgmpMemoryMapping> {
        for i in 0..8 {
            let path = PathBuf::from(format!("/dev/kvmfr{}", i));
            if !path.exists() { continue; }
            match Self::open_path(&path) {
                Some(m) => {
                    info!(device = ?path, size = m.size, "IVSHMEM/KVMFR transport opened");
                    return Some(m);
                }
                None => continue,
            }
        }
        None
    }

    fn open_path(path: &std::path::Path) -> Option<LgmpMemoryMapping> {
        use std::fs::OpenOptions;
        use std::os::unix::io::AsRawFd;

        let file = OpenOptions::new().read(true).write(true).open(path).ok()?;
        let size = unsafe { libc::lseek(file.as_raw_fd(), 0, libc::SEEK_END) };
        if size > 0 {
            let size = size as usize;
            let mem = unsafe {
                libc::mmap(ptr::null_mut(), size, libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED, file.as_raw_fd(), 0)
            };
            if mem == libc::MAP_FAILED { return None; }
            return Some(LgmpMemoryMapping::new(mem as *mut u8, size));
        }

        // kvmfr static devices don't support SEEK_END.
        // The size is determined by the static_size_mb module parameter.
        // Try common sizes: 4MB, 32MB, 64MB, 128MB
        for &guess_mb in &[4, 32, 64, 128] {
            let guess = guess_mb * 1024 * 1024;
            let mem = unsafe {
                libc::mmap(ptr::null_mut(), guess, libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_SHARED, file.as_raw_fd(), 0)
            };
            if mem != libc::MAP_FAILED {
                info!(?path, size_mb = guess_mb, "KVMFR device mapped (static)");
                return Some(LgmpMemoryMapping::new(mem as *mut u8, guess));
            }
        }
        None
    }
}

/// Hold a mapping + free it on drop
struct MappedRegion {
    mapping: LgmpMemoryMapping,
}

impl Drop for MappedRegion {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.mapping.mem as *mut libc::c_void, self.mapping.size); }
    }
}

// ── KVMFR Input Sink ─────────────────────────────────────────────────

/// Sends pointer events to the KVMFR guest via LGMP cursor queue.
#[derive(Debug)]
pub struct KvmfrInputSink {
    cursor_queue: *mut ffi::LGMPClientQueue,
}

// Safety: cursor_queue is only accessed from the main thread (single-threaded compositor).
unsafe impl Send for KvmfrInputSink {}
unsafe impl Sync for KvmfrInputSink {}

impl KvmfrInputSink {
    fn new(cursor_queue: *mut ffi::LGMPClientQueue) -> Self {
        KvmfrInputSink { cursor_queue }
    }

    fn send_cursor_msg(&self, msg: &ffi::CursorChannelMessage) {
        let ptr = msg as *const ffi::CursorChannelMessage as *const u8;
        let size = std::mem::size_of::<ffi::CursorChannelMessage>() as u32;
        unsafe {
            ffi::lgmpClientSendData(self.cursor_queue, ptr, size, ptr::null_mut());
        }
    }
}

impl InputSink for KvmfrInputSink {
    fn handle_pointer(&mut self, kind: PointerEventKind, u: f64, v: f64) {
        let msg = match kind {
            PointerEventKind::Down | PointerEventKind::Up | PointerEventKind::Motion => {
                let msg_type = match kind {
                    PointerEventKind::Motion => 0u32,
                    PointerEventKind::Down => 1u32,
                    PointerEventKind::Up => 1u32,
                    _ => 0u32,
                };
                let x = (u * u32::MAX as f64) as u32;
                let y = (v * u32::MAX as f64) as u32;
                let buttons = match kind {
                    PointerEventKind::Down => 1u32,
                    _ => 0u32,
                };
                ffi::CursorChannelMessage { msg_type, x, y, buttons }
            }
            PointerEventKind::Scroll(_, _) => return,
        };
        self.send_cursor_msg(&msg);
    }

    fn handle_keyboard(&mut self, event: crate::input_router::KeyboardEvent) {
        info!(key = event.key, pressed = event.pressed, "KVMFR keyboard (LGMP stream API TODO)");
    }
}

// ── KVMFR Frame Producer ──────────────────────────────────────────────

/// Utility to hold an LGMP mapping + client, freeing on drop.
struct LgmpConnection {
    _mapping: LgmpMemoryMapping,
    client: *mut ffi::LGMPClient,
    frame_queue: *mut ffi::LGMPClientQueue,
    cursor_queue: Option<*mut ffi::LGMPClientQueue>,
    texture: GlesTexture,
    width: u32,
    height: u32,
}

// Safety: single-threaded compositor
unsafe impl Send for LgmpConnection {}

impl Drop for LgmpConnection {
    fn drop(&mut self) {
        // lgmpClientFree takes *mut LGMPClient and sets it to null
        unsafe {
            ffi::lgmpClientFree(&mut self.client);
        }
    }
}

pub struct KvmfrFrameProducer {
    state: ProducerState,
    frame_count: u64,
    pub connection: Option<LgmpConnection>,
}

enum ProducerState {
    Uninitialized,
    Simulated { texture: GlesTexture, width: u32, height: u32 },
    LgmpConnected, // connection field holds the actual connection
    NoDevice,
}

impl KvmfrFrameProducer {
    pub fn new() -> Self {
        KvmfrFrameProducer {
            state: ProducerState::Uninitialized,
            frame_count: 0,
            connection: None,
        }
    }

    /// Try transports in priority order, then init LGMP on the first that works.
    fn try_lgmp(&mut self, renderer: &mut GlesRenderer) -> Option<FrameResult> {
        let mapping = IvshmemTransport::open()
            .or_else(|| PosixShmTransport::open("/veyra-test"))?;

        let mem = mapping.ptr();
        let size = mapping.len();

        let mut client: *mut ffi::LGMPClient = ptr::null_mut();
        let s = unsafe { ffi::lgmpClientInit(mem, size, &mut client) };
        if s != 0 || client.is_null() {
            warn!(status = s, "lgmpClientInit failed");
            return None;
        }

        let mut client_id = 0u32;
        let mut remote_ver = 0u32;
        let mut session_ok = false;
        for _ in 0..100 {
            let mut udata_size = 0u32;
            let mut udata: *mut u8 = ptr::null_mut();
            if unsafe { ffi::lgmpClientSessionInit(client, &mut udata_size, &mut udata,
                &mut client_id, &mut remote_ver) } == 0
            { session_ok = true; break; }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if !session_ok {
            warn!("lgmpClientSessionInit failed");
            unsafe { ffi::lgmpClientFree(&mut client) };
            return None;
        }
        info!(?client_id, ?remote_ver, "LGMP session OK");

        let mut frame_queue: *mut ffi::LGMPClientQueue = ptr::null_mut();
        if unsafe { ffi::lgmpClientSubscribe(client, ffi::LGMP_Q_FRAME, &mut frame_queue) } != 0 || frame_queue.is_null() {
            warn!("lgmpClientSubscribe failed");
            unsafe { ffi::lgmpClientFree(&mut client) };
            return None;
        }
        info!("LGMP frame queue subscribed");

        let mut cursor_queue: *mut ffi::LGMPClientQueue = ptr::null_mut();
        let cursor_q = if unsafe { ffi::lgmpClientSubscribe(client, ffi::LGMP_Q_CURSOR, &mut cursor_queue) } == 0 && !cursor_queue.is_null() {
            info!("LGMP cursor queue subscribed");
            Some(cursor_queue)
        } else {
            info!("LGMP cursor queue not available (input disabled)");
            None
        };

        // Try to get the first frame to determine initial texture/size
        unsafe { ffi::lgmpClientAdvanceToLast(frame_queue); }
        let mut msg = ffi::LGMPMessage { udata: 0, size: 0, mem: ptr::null_mut() };
        let got_frame = unsafe { ffi::lgmpClientProcess(frame_queue, &mut msg) } == 0 && !msg.mem.is_null();

        if got_frame {
            let meta = unsafe { &*(msg.mem as *const [u32; 4]) };
            let width = meta[1];
            let height = meta[2];
            let frame_ptr = msg.udata as *const u8;
            if !frame_ptr.is_null() && width > 0 && height > 0 {
                let pixels = unsafe { std::slice::from_raw_parts(frame_ptr, (width * height * 4) as usize) };
                if let Ok(tex) = renderer.import_memory(pixels, Fourcc::Abgr8888,
                    (width as i32, height as i32).into(), false)
                {
                    unsafe { ffi::lgmpClientMessageDone(frame_queue); }
                    info!(?width, ?height, "LGMP frame acquired");

                    self.connection = Some(LgmpConnection {
                        _mapping: mapping,
                        client,
                        frame_queue,
                        cursor_queue: cursor_q,
                        texture: tex,
                        width,
                        height,
                    });
                    self.state = ProducerState::LgmpConnected;
                    return Some(FrameResult::Updated);
                }
            }
        }

        // No frame yet: show a checkerboard until one arrives
        let (w, h) = (512u32, 384u32);
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                if ((x / 64) + (y / 64)) % 2 == 0 { pixels.extend_from_slice(&[200, 60, 240, 255]); }
                else { pixels.extend_from_slice(&[15, 8, 20, 255]); }
            }
        }
        let tex = renderer.import_memory(&pixels, Fourcc::Abgr8888, (w as i32, h as i32).into(), false).ok()?;
        info!("LGMP transport connected, awaiting first frame");

        // Store connection with placeholder texture
        self.connection = Some(LgmpConnection {
            _mapping: mapping,
            client,
            frame_queue,
            cursor_queue: cursor_q,
            texture: tex,
            width: w,
            height: h,
        });
        self.state = ProducerState::LgmpConnected;
        Some(FrameResult::Updated)
    }
}

impl FrameProducer for KvmfrFrameProducer {
    fn update(&mut self, renderer: &mut GlesRenderer) -> FrameResult {
        self.frame_count += 1;

        // On first call: try LGMP, fall back to simulated
        if self.frame_count == 1 && matches!(self.state, ProducerState::Uninitialized) {
            match self.try_lgmp(renderer) {
                Some(r) => return r,
                None => {
                    info!("LGMP transport unavailable, using simulated frames");
                    return self.fallback_simulated(renderer);
                }
            }
        }

        match &mut self.state {
            ProducerState::LgmpConnected => {
                // Continuously read frames from LGMP queue
                let conn = match &mut self.connection {
                    Some(c) => c,
                    None => return FrameResult::Error("LgmpConnected but no connection".into()),
                };
                let queue = conn.frame_queue;
                unsafe { ffi::lgmpClientAdvanceToLast(queue); }
                let mut msg = ffi::LGMPMessage { udata: 0, size: 0, mem: ptr::null_mut() };
                if unsafe { ffi::lgmpClientProcess(queue, &mut msg) } != 0 || msg.mem.is_null() {
                    return FrameResult::Unchanged;
                }
                let meta = unsafe { &*(msg.mem as *const [u32; 4]) };
                let width = meta[1];
                let height = meta[2];
                let frame_ptr = msg.udata as *const u8;
                if frame_ptr.is_null() || width == 0 || height == 0 {
                    unsafe { ffi::lgmpClientMessageDone(queue); }
                    return FrameResult::Unchanged;
                }

                // Import new frame
                let pixels = unsafe { std::slice::from_raw_parts(frame_ptr, (width * height * 4) as usize) };
                match renderer.import_memory(pixels, Fourcc::Abgr8888,
                    (width as i32, height as i32).into(), false)
                {
                    Ok(tex) => {
                        conn.texture = tex;
                        conn.width = width;
                        conn.height = height;
                        unsafe { ffi::lgmpClientMessageDone(queue); }
                        FrameResult::Updated
                    }
                    Err(e) => {
                        unsafe { ffi::lgmpClientMessageDone(queue); }
                        FrameResult::Error(format!("LGMP tex import: {:?}", e))
                    }
                }
            }
            ProducerState::Simulated { ref mut texture, .. } => {
                if self.frame_count % 5 != 0 { return FrameResult::Unchanged; }
                let mut pixels = vec![0u8; 256 * 256 * 4];
                let shift = ((self.frame_count / 5) % 12) as u8;
                for y in 0..256u32 {
                    for x in 0..256u32 {
                        let i = (y * 256 + x) as usize * 4;
                        let bright = ((x / 32) + (y / 32) + (shift as u32)) % 2 == 0;
                        pixels[i + 0] = if bright { 200u8.wrapping_add(shift.wrapping_mul(5)) } else { 15 };
                        pixels[i + 1] = if bright { 60u8.wrapping_add(shift.wrapping_mul(3)) } else { 8 };
                        pixels[i + 2] = if bright { 240u8.wrapping_sub(shift.wrapping_mul(8)) } else { 20 };
                        pixels[i + 3] = 255;
                    }
                }
                match renderer.import_memory(&pixels, Fourcc::Abgr8888, (256, 256).into(), false) {
                    Ok(tex) => { *texture = tex; FrameResult::Updated }
                    Err(e) => FrameResult::Error(format!("tex: {:?}", e)),
                }
            }
            ProducerState::NoDevice => FrameResult::Error("KVMFR unavailable".into()),
            _ => FrameResult::Unchanged,
        }
    }

    fn texture(&self) -> &GlesTexture {
        if let ProducerState::LgmpConnected = &self.state {
            return &self.connection.as_ref().unwrap().texture;
        }
        match &self.state { ProducerState::Simulated { texture, .. } => texture, _ => unreachable!() }
    }

    fn size(&self) -> (u32, u32) {
        if let ProducerState::LgmpConnected = &self.state {
            let conn = self.connection.as_ref().unwrap();
            return (conn.width, conn.height);
        }
        match &self.state { ProducerState::Simulated { width, height, .. } => (*width, *height), _ => (0, 0) }
    }

    fn create_input_sink(&mut self) -> Option<Box<dyn InputSink>> {
        if let ProducerState::LgmpConnected = &self.state {
            if let Some(q) = self.connection.as_ref().and_then(|c| c.cursor_queue) {
                return Some(Box::new(KvmfrInputSink::new(q)) as Box<dyn InputSink>);
            }
        }
        #[derive(Debug)]
        struct LogInputSink;
        impl InputSink for LogInputSink {
            fn handle_pointer(&mut self, kind: PointerEventKind, u: f64, v: f64) {
                info!(?kind, u, v, "kvmfr input (simulated)");
            }
            fn handle_keyboard(&mut self, event: crate::input_router::KeyboardEvent) {
                info!(key = event.key, pressed = event.pressed, "kvmfr keyboard (simulated)");
            }
        }
        Some(Box::new(LogInputSink))
    }

    fn capabilities(&self) -> crate::producer::ProviderCapabilities {
        match &self.state {
            ProducerState::LgmpConnected => crate::producer::ProviderCapabilities {
                pointer_input: self.connection.as_ref().and_then(|c| c.cursor_queue).is_some(),
                keyboard_input: false,
                resize: true,
                close: false,
                reconnect: true,
            },
            _ => crate::producer::ProviderCapabilities {
                pointer_input: true,
                keyboard_input: true,
                resize: true,
                close: false,
                reconnect: false,
            },
        }
    }
}

impl KvmfrFrameProducer {
    fn fallback_simulated(&mut self, renderer: &mut GlesRenderer) -> FrameResult {
        let w = 256u32;
        let h = 256u32;
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h { for x in 0..w {
            if ((x / 32) + (y / 32)) % 2 == 0 { pixels.extend_from_slice(&[200, 60, 240, 255]); }
            else { pixels.extend_from_slice(&[15, 8, 20, 255]); }
        }}
        match renderer.import_memory(&pixels, Fourcc::Abgr8888, (w as i32, h as i32).into(), false) {
            Ok(texture) => { self.state = ProducerState::Simulated { texture, width: w, height: h }; FrameResult::Updated }
            Err(e) => { self.state = ProducerState::NoDevice; FrameResult::Error(format!("fallback: {:?}", e)) }
        }
    }
}
