//! Showing a shared surface without copying: on macOS its IOSurface wrapped as a CoreVideo buffer and pushed into a
//! `NativeSurface`.

use futhark_surfaces::Surface;
use rune::NativeSurface;

/// Shows `surface` in `native`. The caller keeps the frame that owns `surface` until two newer ones replace it, so
/// the server doesn't draw into it while the GPU may still be reading it.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn push(native: &NativeSurface, surface: &Surface, frame: i64) -> Result<(), String> {
    use rune::native_surface::ColorInfo;
    use rune::native_surface::macos::PixelBuffer;

    // SAFETY: `as_ptr` is a live IOSurfaceRef that `surface` retains, and the buffer retains it again. The server
    // finished drawing into it before announcing the frame and won't draw into it again until the frame is released,
    // which the caller holds off as described above.
    let buffer = unsafe { PixelBuffer::from_iosurface(surface.as_ptr(), ColorInfo::default()) }?;
    native.push(buffer, frame.max(0) as u64)
}

#[cfg(not(target_os = "macos"))]
pub fn push(_: &NativeSurface, _: &Surface, _: i64) -> Result<(), String> {
    Err("shared GPU surfaces are only available on macOS so far".into())
}
