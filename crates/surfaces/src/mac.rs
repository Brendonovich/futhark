//! IOSurfaces, handed between processes as Mach ports.
//!
//! The daemon checks a Mach service in with the bootstrap server under a name of its own
//! (`dev.futhark.futharkd.<pid>.surfaces`): launchd lets any process register a name nobody holds in its own bootstrap
//! namespace (the user's login session), and forgets it when the process exits. A client sends the service a request
//! holding a token and a reply port; the reply carries one send right per surface (`IOSurfaceCreateMachPort`), which
//! the client turns back into surfaces (`IOSurfaceLookupFromMachPort`).

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_void};
use std::mem::{size_of, zeroed};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use mach2::bootstrap::{bootstrap_check_in, bootstrap_look_up, bootstrap_port, bootstrap_strerror};
use mach2::kern_return::{KERN_SUCCESS, kern_return_t};
use mach2::mach_port::{mach_port_allocate, mach_port_deallocate, mach_port_mod_refs};
use mach2::message::{
    MACH_MSG_PORT_DESCRIPTOR, MACH_MSG_SUCCESS, MACH_MSG_TIMEOUT_NONE, MACH_MSG_TYPE_COPY_SEND,
    MACH_MSG_TYPE_MAKE_SEND_ONCE, MACH_MSG_TYPE_MOVE_SEND, MACH_MSG_TYPE_MOVE_SEND_ONCE, MACH_MSGH_BITS,
    MACH_MSGH_BITS_COMPLEX, MACH_RCV_MSG, MACH_RCV_TIMED_OUT, MACH_RCV_TIMEOUT, MACH_SEND_INVALID_DEST, MACH_SEND_MSG,
    MACH_SEND_TIMED_OUT, MACH_SEND_TIMEOUT, mach_msg, mach_msg_body_t, mach_msg_destroy, mach_msg_header_t,
    mach_msg_port_descriptor_t, mach_msg_return_t, mach_msg_trailer_t,
};
use mach2::port::{MACH_PORT_NULL, MACH_PORT_RIGHT_RECEIVE, mach_port_t};
use mach2::traps::mach_task_self;
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString};
use objc2_io_surface::{
    IOSurfaceLockOptions, IOSurfaceRef, kIOSurfaceBytesPerElement, kIOSurfaceBytesPerRow, kIOSurfaceColorSpace,
    kIOSurfaceHeight, kIOSurfacePixelFormat, kIOSurfaceWidth,
};

use crate::{Error, MAX_SURFACES, Token};

/// `msgh_id`s of the service's messages.
const REQUEST: i32 = 0x0f07_4a00;
const REPLY: i32 = REQUEST + 1;
/// The token is unknown or was used already.
const REFUSED: i32 = REQUEST + 2;

/// How long the daemon waits to hand a reply to a client.
const REPLY_TIMEOUT_MS: u32 = 1000;

/// An 8-bit BGRA picture in GPU memory that several processes can use at once, tagged sRGB. Cloning shares the same
/// surface.
#[derive(Clone)]
pub struct Surface(CFRetained<IOSurfaceRef>);

// SAFETY: IOSurfaces are reference counted atomically and may be used from any thread; their pixels are written by
// the GPU, or by `read_bgra` under the surface's own lock.
unsafe impl Send for Surface {}
// SAFETY: as for `Send`; no method takes `&mut`.
unsafe impl Sync for Surface {}

impl Surface {
    /// A new `width`×`height` surface of 8-bit BGRA (`Bgra8Unorm` on the GPU) in the sRGB colour space, its contents
    /// undefined. It is not global: other processes only get it through an [`Exporter`].
    pub fn bgra(width: u32, height: u32) -> Result<Self, Error> {
        if width == 0 || height == 0 || width > i32::MAX as u32 || height > i32::MAX as u32 {
            return Err(Error::Size(width, height));
        }
        // SAFETY: the IOSurface property keys are immutable CFStrings the framework defines.
        let (width_key, height_key, element_key, row_key, format_key, colour_key) = unsafe {
            (
                kIOSurfaceWidth,
                kIOSurfaceHeight,
                kIOSurfaceBytesPerElement,
                kIOSurfaceBytesPerRow,
                kIOSurfacePixelFormat,
                kIOSurfaceColorSpace,
            )
        };
        let row = IOSurfaceRef::align_property(row_key, width as usize * 4);
        let numbers = [
            CFNumber::new_i32(width as i32),
            CFNumber::new_i32(height as i32),
            CFNumber::new_i32(4),
            CFNumber::new_i64(row as i64),
            CFNumber::new_i32(u32::from_be_bytes(*b"BGRA") as i32),
        ];
        let keys = [width_key, height_key, element_key, row_key, format_key];
        let values: Vec<&CFNumber> = numbers.iter().map(|n| &**n).collect();
        let properties = CFDictionary::<CFString, CFNumber>::from_slices(&keys, &values);
        // SAFETY: `properties` is a dictionary of IOSurface property keys to numbers of the types the keys expect.
        let surface = unsafe { IOSurfaceRef::new(properties.as_opaque()) }.ok_or(Error::Create(width, height))?;
        // The name of the colour space, as Chromium tags its IOSurfaces; `kCGColorSpaceSRGB`'s value.
        let srgb = CFString::from_static_str("kCGColorSpaceSRGB");
        // SAFETY: `kIOSurfaceColorSpace` takes a colour space name or property list, and a CFString is one.
        unsafe { surface.set_value(colour_key, &srgb) };
        Ok(Self(surface))
    }

    pub fn width(&self) -> u32 {
        self.0.width() as u32
    }

    pub fn height(&self) -> u32 {
        self.0.height() as u32
    }

    /// The `IOSurfaceRef` itself, for wrapping it in another API. It stays valid while this `Surface` (or a clone) is
    /// alive; retain it to keep it longer.
    pub fn as_ptr(&self) -> *mut c_void {
        (&*self.0 as *const IOSurfaceRef).cast_mut().cast()
    }

    /// The surface as a `Bgra8Unorm` texture of `device` (a Metal device) with `usage`, in place: the GPU reads and
    /// writes the surface's own memory. The texture keeps the surface alive.
    pub fn texture(&self, device: &wgpu::Device, usage: wgpu::TextureUsages) -> Result<wgpu::Texture, Error> {
        use objc2_metal::{MTLDevice, MTLPixelFormat, MTLTextureDescriptor, MTLTextureType, MTLTextureUsage};
        use wgpu::hal;

        let metal = {
            // SAFETY: the raw device is only used to create one texture, and is not destroyed.
            let device = unsafe { device.as_hal::<hal::api::Metal>() }.ok_or(Error::NotMetal)?;
            device.raw_device().clone()
        };
        let (width, height) = (self.width(), self.height());
        // SAFETY: a 2D descriptor of a valid format and the surface's non-zero size.
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm,
                width as usize,
                height as usize,
                false,
            )
        };
        // Metal's usage must cover what wgpu will do with the texture.
        let mut metal_usage = MTLTextureUsage::Unknown;
        if usage.contains(wgpu::TextureUsages::TEXTURE_BINDING) {
            metal_usage |= MTLTextureUsage::ShaderRead;
        }
        if usage.contains(wgpu::TextureUsages::RENDER_ATTACHMENT) {
            metal_usage |= MTLTextureUsage::RenderTarget;
        }
        if usage.contains(wgpu::TextureUsages::STORAGE_BINDING) {
            metal_usage |= MTLTextureUsage::ShaderWrite;
        }
        descriptor.setUsage(metal_usage);
        let raw = metal
            .newTextureWithDescriptor_iosurface_plane(&descriptor, &self.0, 0)
            .ok_or(Error::Texture)?;
        let keep = self.clone();
        let size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };
        // SAFETY: `raw` is a 2D, single-layer, single-mip BGRA8Unorm texture of exactly `size`, and the callback keeps
        // the surface backing it alive until wgpu drops the texture.
        let hal_texture = unsafe {
            hal::metal::Device::texture_from_raw(
                raw,
                wgpu::TextureFormat::Bgra8Unorm,
                MTLTextureType::Type2D,
                1,
                1,
                hal::CopyExtent {
                    width,
                    height,
                    depth: 1,
                },
                Some(Box::new(move || drop(keep))),
            )
        };
        let state = if usage.contains(wgpu::TextureUsages::TEXTURE_BINDING) {
            wgpu::TextureUses::RESOURCE
        } else {
            wgpu::TextureUses::COLOR_TARGET
        };
        // SAFETY: `hal_texture` was made from `device`'s own Metal device, and the descriptor matches it (format,
        // size, one mip, one sample) with a usage Metal was told about.
        Ok(unsafe {
            device.create_texture_from_hal::<hal::api::Metal>(
                hal_texture,
                &wgpu::TextureDescriptor {
                    label: Some("shared surface"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Bgra8Unorm,
                    usage,
                    view_formats: &[],
                },
                state,
            )
        })
    }

    /// The surface's pixels, 4 bytes each in B, G, R, A order, rows tightly packed. Wait for the GPU to finish writing
    /// first.
    pub fn read_bgra(&self) -> Vec<u8> {
        let (width, height) = (self.width() as usize, self.height() as usize);
        let row = width * 4;
        let stride = self.0.bytes_per_row();
        let mut pixels = Vec::with_capacity(row * height);
        // SAFETY: a null seed is allowed.
        let status = unsafe { self.0.lock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
        assert_eq!(status, KERN_SUCCESS, "could not lock an IOSurface");
        let base = self.0.base_address().as_ptr().cast::<u8>().cast_const();
        for y in 0..height {
            // SAFETY: the surface is locked, so its base address is valid for `height` rows of `stride` bytes, and
            // `row <= stride`.
            pixels.extend_from_slice(unsafe { std::slice::from_raw_parts(base.add(y * stride), row) });
        }
        // SAFETY: unlocks the lock taken above, with the same options.
        unsafe { self.0.unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
        pixels
    }
}

impl std::fmt::Debug for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Surface({} {}×{})", self.0.id(), self.width(), self.height())
    }
}

/// The daemon's side: a Mach service that hands each offered set of surfaces to the client holding its token, once.
pub struct Exporter {
    service: String,
    offers: Arc<Offers>,
}

/// Surfaces offered and not fetched yet, by token.
type Offers = Mutex<HashMap<Token, Vec<Surface>>>;

/// A request: the token, nothing else.
#[repr(C)]
struct Request {
    header: mach_msg_header_t,
    token: [u8; 16],
}

/// Room for a request and the trailer the kernel appends.
#[repr(C)]
struct RequestBuffer {
    request: Request,
    trailer: mach_msg_trailer_t,
    spare: [u8; 64],
}

/// A reply: a send right for each surface.
#[repr(C)]
struct Reply {
    header: mach_msg_header_t,
    body: mach_msg_body_t,
    ports: [mach_msg_port_descriptor_t; MAX_SURFACES],
}

#[repr(C)]
struct ReplyBuffer {
    reply: Reply,
    trailer: mach_msg_trailer_t,
    spare: [u8; 64],
}

impl Exporter {
    /// This process's exporter, its service registered on first use.
    pub fn shared() -> Result<&'static Exporter, Error> {
        static SHARED: OnceLock<Result<Exporter, String>> = OnceLock::new();
        SHARED
            .get_or_init(|| Exporter::start().map_err(|err| err.to_string()))
            .as_ref()
            .map_err(|err| Error::Service(err.clone()))
    }

    fn start() -> Result<Exporter, Error> {
        let service = format!("dev.futhark.futharkd.{}.surfaces", std::process::id());
        let name = CString::new(service.clone()).expect("no NUL in the service name");
        let mut port: mach_port_t = MACH_PORT_NULL;
        // SAFETY: `name` is a NUL-terminated string and `port` a valid place for the receive right.
        let status = unsafe { bootstrap_check_in(bootstrap_port, name.as_ptr(), &mut port) };
        if status != KERN_SUCCESS {
            return Err(Error::Register {
                service,
                reason: bootstrap_error(status),
            });
        }
        let offers = Arc::new(Offers::default());
        let served = offers.clone();
        thread::Builder::new()
            .name("shared surfaces".into())
            .spawn(move || serve(port, &served))
            .map_err(|err| Error::Service(format!("could not start the shared surface service: {err}")))?;
        Ok(Exporter { service, offers })
    }

    /// The Mach service's bootstrap name, which clients look up.
    pub fn service_name(&self) -> &str {
        &self.service
    }

    /// Offer `surfaces` (at most [`MAX_SURFACES`]) to whoever brings the token returned, once.
    pub fn offer(&self, surfaces: Vec<Surface>) -> Token {
        assert!(surfaces.len() <= MAX_SURFACES, "too many surfaces in one offer");
        let token = Token::random();
        self.offers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(token, surfaces);
        token
    }

    /// Take back an offer nobody fetched.
    pub fn withdraw(&self, token: &Token) {
        self.offers.lock().unwrap_or_else(|e| e.into_inner()).remove(token);
    }
}

/// Answer requests on `port` for as long as the process runs.
fn serve(port: mach_port_t, offers: &Offers) {
    loop {
        // SAFETY: Mach message structs are plain C data; all zeroes is a valid empty message.
        let mut buffer: RequestBuffer = unsafe { zeroed() };
        // SAFETY: receives into `buffer`, which is as large as the size given; `port` is our receive right.
        let status = unsafe {
            mach_msg(
                &mut buffer.request.header,
                MACH_RCV_MSG,
                0,
                size_of::<RequestBuffer>() as u32,
                port,
                MACH_MSG_TIMEOUT_NONE,
                MACH_PORT_NULL,
            )
        };
        if status != MACH_MSG_SUCCESS {
            // A message too large is destroyed by the kernel; keep going.
            continue;
        }
        let header = buffer.request.header;
        let complex = header.msgh_bits & MACH_MSGH_BITS_COMPLEX != 0;
        if complex
            || header.msgh_id != REQUEST
            || header.msgh_size as usize != size_of::<Request>()
            || header.msgh_remote_port == MACH_PORT_NULL
        {
            // SAFETY: the message was received whole into `buffer`; this lets go of any rights it carried.
            unsafe { mach_msg_destroy(&mut buffer.request.header) };
            continue;
        }
        let surfaces = offers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&Token(buffer.request.token));
        reply(header.msgh_remote_port, surfaces.as_deref());
    }
}

/// Send `surfaces` (or a refusal) to the send-once right `to`.
fn reply(to: mach_port_t, surfaces: Option<&[Surface]>) {
    // SAFETY: Mach message structs are plain C data; all zeroes is a valid empty message.
    let mut message: Reply = unsafe { zeroed() };
    let count = surfaces.map_or(0, <[Surface]>::len);
    message.header.msgh_remote_port = to;
    message.header.msgh_local_port = MACH_PORT_NULL;
    match surfaces {
        Some(surfaces) => {
            message.header.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_MOVE_SEND_ONCE, 0) | MACH_MSGH_BITS_COMPLEX;
            message.header.msgh_id = REPLY;
            message.header.msgh_size = (size_of::<mach_msg_header_t>()
                + size_of::<mach_msg_body_t>()
                + count * size_of::<mach_msg_port_descriptor_t>()) as u32;
            message.body.msgh_descriptor_count = count as u32;
            for (descriptor, surface) in message.ports.iter_mut().zip(surfaces) {
                descriptor.name = surface.0.create_mach_port();
                descriptor.disposition = MACH_MSG_TYPE_MOVE_SEND as u8;
                descriptor.type_ = MACH_MSG_PORT_DESCRIPTOR as u8;
            }
        }
        None => {
            message.header.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_MOVE_SEND_ONCE, 0);
            message.header.msgh_id = REFUSED;
            message.header.msgh_size = size_of::<mach_msg_header_t>() as u32;
        }
    }
    // SAFETY: `message` is a well-formed message of `msgh_size` bytes whose rights this task holds.
    let status = unsafe {
        mach_msg(
            &mut message.header,
            MACH_SEND_MSG | MACH_SEND_TIMEOUT,
            message.header.msgh_size,
            0,
            MACH_PORT_NULL,
            REPLY_TIMEOUT_MS,
            MACH_PORT_NULL,
        )
    };
    // These leave the message's rights with the sender.
    if status == MACH_SEND_INVALID_DEST || status == MACH_SEND_TIMED_OUT {
        for descriptor in &message.ports[..count] {
            // SAFETY: the send right was made above and not consumed.
            unsafe { mach_port_deallocate(mach_task_self(), descriptor.name) };
        }
        // SAFETY: the send-once right came with the request and was not consumed.
        unsafe { mach_port_deallocate(mach_task_self(), to) };
    }
}

/// The client's side: fetch the surfaces offered under `token` (its 32 hex digits) from the daemon's Mach service
/// `service`, waiting up to `timeout`.
pub fn fetch(service: &str, token: &str, timeout: Duration) -> Result<Vec<Surface>, Error> {
    let token: Token = token.parse()?;
    let lookup_error = |reason: String| Error::Lookup {
        service: service.to_owned(),
        reason,
    };
    let name = CString::new(service).map_err(|_| lookup_error("not a valid name".into()))?;
    let mut server: mach_port_t = MACH_PORT_NULL;
    // SAFETY: `name` is a NUL-terminated string and `server` a valid place for the send right.
    let status = unsafe { bootstrap_look_up(bootstrap_port, name.as_ptr(), &mut server) };
    if status != KERN_SUCCESS {
        return Err(lookup_error(bootstrap_error(status)));
    }
    let result = request(server, token, timeout);
    // SAFETY: `server` is the send right the look-up gave us.
    unsafe { mach_port_deallocate(mach_task_self(), server) };
    result
}

fn request(server: mach_port_t, token: Token, timeout: Duration) -> Result<Vec<Surface>, Error> {
    let reply_port = ReplyPort::new()?;
    let mut request = Request {
        header: mach_msg_header_t {
            msgh_bits: MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MAKE_SEND_ONCE),
            msgh_size: size_of::<Request>() as u32,
            msgh_remote_port: server,
            msgh_local_port: reply_port.0,
            msgh_voucher_port: MACH_PORT_NULL,
            msgh_id: REQUEST,
        },
        token: token.0,
    };
    let millis = timeout.as_millis().clamp(1, u128::from(u32::MAX)) as u32;
    // SAFETY: `request` is a well-formed simple message; we hold a send right to `server` and the receive right of
    // `reply_port`.
    let status = unsafe {
        mach_msg(
            &mut request.header,
            MACH_SEND_MSG | MACH_SEND_TIMEOUT,
            size_of::<Request>() as u32,
            0,
            MACH_PORT_NULL,
            millis,
            MACH_PORT_NULL,
        )
    };
    if status == MACH_SEND_TIMED_OUT {
        return Err(Error::Timeout(timeout));
    }
    check_msg(status, "send a request to the surface service")?;
    // SAFETY: Mach message structs are plain C data; all zeroes is a valid empty message.
    let mut buffer: ReplyBuffer = unsafe { zeroed() };
    // SAFETY: receives into `buffer`, which is as large as the size given, on our own receive right.
    let status = unsafe {
        mach_msg(
            &mut buffer.reply.header,
            MACH_RCV_MSG | MACH_RCV_TIMEOUT,
            0,
            size_of::<ReplyBuffer>() as u32,
            reply_port.0,
            millis,
            MACH_PORT_NULL,
        )
    };
    if status == MACH_RCV_TIMED_OUT {
        return Err(Error::Timeout(timeout));
    }
    check_msg(status, "receive surfaces from the surface service")?;
    let header = buffer.reply.header;
    let complex = header.msgh_bits & MACH_MSGH_BITS_COMPLEX != 0;
    let count = buffer.reply.body.msgh_descriptor_count as usize;
    if header.msgh_id == REFUSED && !complex {
        return Err(Error::Refused);
    }
    if header.msgh_id != REPLY || !complex || count > MAX_SURFACES {
        // SAFETY: the message was received whole into `buffer`; this lets go of any rights it carried.
        unsafe { mach_msg_destroy(&mut buffer.reply.header) };
        return Err(Error::BadReply);
    }
    let mut surfaces = Vec::with_capacity(count);
    let mut failed = false;
    for descriptor in &buffer.reply.ports[..count] {
        let port = descriptor.name;
        if descriptor.type_ as u32 == MACH_MSG_PORT_DESCRIPTOR && port != MACH_PORT_NULL {
            match IOSurfaceRef::lookup_from_mach_port(port) {
                Some(surface) => surfaces.push(Surface(surface)),
                None => failed = true,
            }
            // SAFETY: the reply moved this send right to us; the surface holds its own reference.
            unsafe { mach_port_deallocate(mach_task_self(), port) };
        } else {
            failed = true;
        }
    }
    if failed {
        return Err(Error::Open);
    }
    Ok(surfaces)
}

/// A receive right for one reply, destroyed when dropped.
struct ReplyPort(mach_port_t);

impl ReplyPort {
    fn new() -> Result<Self, Error> {
        let mut port = MACH_PORT_NULL;
        // SAFETY: `port` is a valid place for the new right's name.
        let status = unsafe { mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &mut port) };
        if status != KERN_SUCCESS {
            return Err(Error::Mach {
                doing: "allocate a Mach port",
                code: status,
            });
        }
        Ok(Self(port))
    }
}

impl Drop for ReplyPort {
    fn drop(&mut self) {
        // SAFETY: we hold this receive right and nothing else uses it once dropped.
        unsafe { mach_port_mod_refs(mach_task_self(), self.0, MACH_PORT_RIGHT_RECEIVE, -1) };
    }
}

fn check_msg(status: mach_msg_return_t, doing: &'static str) -> Result<(), Error> {
    if status != MACH_MSG_SUCCESS {
        return Err(Error::Mach { doing, code: status });
    }
    Ok(())
}

fn bootstrap_error(status: kern_return_t) -> String {
    // SAFETY: `bootstrap_strerror` takes any status and returns a static string or null.
    let text = unsafe { bootstrap_strerror(status) };
    if text.is_null() {
        return format!("error {status}");
    }
    // SAFETY: a non-null result is a NUL-terminated static string.
    unsafe { CStr::from_ptr(text) }.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futhark_gpu::Gpu;

    /// Odd, so the surface's rows are padded beyond `width * 4`.
    const WIDTH: u32 = 70;
    const HEIGHT: u32 = 9;

    /// Clear `surface` to the colour of `bgra` on the GPU and wait for it.
    fn clear(gpu: &Gpu, surface: &Surface, bgra: [u8; 4]) {
        let texture = surface
            .texture(&gpu.device, wgpu::TextureUsages::RENDER_ATTACHMENT)
            .unwrap();
        let view = texture.create_view(&Default::default());
        let channel = |byte: u8| f64::from(byte) / 255.0;
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("clear"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: channel(bgra[2]),
                        g: channel(bgra[1]),
                        b: channel(bgra[0]),
                        a: channel(bgra[3]),
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    }

    fn assert_filled(surface: &Surface, bgra: [u8; 4]) {
        let pixels = surface.read_bgra();
        assert_eq!(pixels.len(), (surface.width() * surface.height() * 4) as usize);
        for (index, pixel) in pixels.chunks(4).enumerate() {
            assert_eq!(pixel, bgra, "pixel {index}");
        }
    }

    #[test]
    fn surfaces_are_bgra_textures() {
        let gpu = Gpu::headless().unwrap();
        let surface = Surface::bgra(WIDTH, HEIGHT).unwrap();
        assert_eq!((surface.width(), surface.height()), (WIDTH, HEIGHT));
        assert!(surface.0.bytes_per_row() > WIDTH as usize * 4);
        assert!(!surface.as_ptr().is_null());
        // SAFETY: an immutable CFString the framework defines.
        let colour = surface.0.value(unsafe { kIOSurfaceColorSpace }).unwrap();
        assert_eq!(
            colour.downcast_ref::<CFString>().map(ToString::to_string).as_deref(),
            Some("kCGColorSpaceSRGB")
        );
        clear(&gpu, &surface, [204, 102, 51, 255]);
        assert_filled(&surface, [204, 102, 51, 255]);
        assert!(matches!(Surface::bgra(0, 4), Err(Error::Size(0, 4))));
    }

    #[test]
    fn offers_are_fetched_once() {
        let gpu = Gpu::headless().unwrap();
        let exporter = Exporter::shared().unwrap();
        let surfaces = vec![Surface::bgra(WIDTH, HEIGHT).unwrap(), Surface::bgra(32, 16).unwrap()];
        let token = exporter.offer(surfaces.clone()).to_string();
        let fetched = fetch(exporter.service_name(), &token, Duration::from_secs(5)).unwrap();
        let sizes: Vec<_> = fetched.iter().map(|s| (s.width(), s.height())).collect();
        assert_eq!(sizes, [(WIDTH, HEIGHT), (32, 16)]);
        clear(&gpu, &fetched[1], [10, 20, 30, 40]);
        assert_filled(&surfaces[1], [10, 20, 30, 40]);

        let again = fetch(exporter.service_name(), &token, Duration::from_secs(5));
        assert!(matches!(again, Err(Error::Refused)), "{again:?}");

        let token = exporter.offer(vec![Surface::bgra(4, 4).unwrap()]);
        exporter.withdraw(&token);
        let withdrawn = fetch(exporter.service_name(), &token.to_string(), Duration::from_secs(5));
        assert!(matches!(withdrawn, Err(Error::Refused)), "{withdrawn:?}");

        let missing = fetch(
            "dev.futhark.nobody.surfaces",
            &token.to_string(),
            Duration::from_secs(5),
        );
        assert!(matches!(missing, Err(Error::Lookup { .. })), "{missing:?}");
    }

    /// Set by `surfaces_cross_processes` when it runs this test binary as the client.
    const CHILD: &str = "FUTHARK_SURFACES_CHILD";

    #[test]
    fn surfaces_cross_processes() {
        let gpu = Gpu::headless().unwrap();
        let exporter = Exporter::shared().unwrap();
        let surfaces = vec![
            Surface::bgra(WIDTH, HEIGHT).unwrap(),
            Surface::bgra(WIDTH, HEIGHT).unwrap(),
        ];
        clear(&gpu, &surfaces[0], [1, 2, 3, 255]);
        let token = exporter.offer(surfaces.clone());
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "mac::tests::client_side", "--nocapture", "--test-threads=1"])
            .env(CHILD, format!("{} {token}", exporter.service_name()))
            .status()
            .unwrap();
        assert!(status.success(), "the client process failed");
        // The client drew into the second surface from its own process.
        assert_filled(&surfaces[1], [7, 8, 9, 255]);
    }

    /// The client half of `surfaces_cross_processes`; does nothing on its own.
    #[test]
    fn client_side() {
        let Ok(handoff) = std::env::var(CHILD) else {
            return;
        };
        let (service, token) = handoff.split_once(' ').unwrap();
        let surfaces = fetch(service, token, Duration::from_secs(5)).unwrap();
        assert_eq!(surfaces.len(), 2);
        assert_filled(&surfaces[0], [1, 2, 3, 255]);
        let gpu = Gpu::headless().unwrap();
        clear(&gpu, &surfaces[1], [7, 8, 9, 255]);
    }
}
